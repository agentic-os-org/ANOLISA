// SPDX-License-Identifier: Apache-2.0
//! Firecracker process ownership and HTTP API over Unix domain sockets.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use blaze_core::backend::{
    BackendKind, RestoreCapability, RestoreRequest, SnapshotKind, SnapshotRequest, SpawnRequest,
};
use blaze_core::policy::{
    BackendConfigs, FirecrackerConfig, VmConfig, parse_memory_value, to_mib_ceil,
};
use blaze_core::{BlazeError, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::netns::{NetworkManager, NetworkSlot};
#[cfg(target_os = "linux")]
use super::terminate_recorded_process;
use super::{
    BackendInstance, BackendRestoreRequest, BackendSpawnRequest, BackendSpawner,
    DynBackendInstance, OwnedRunDir, PinnedExecutable, RestoreResult, SpawnFailure, SpawnResult,
    configure_pid_handoff, prepare_pid_handoff, record_backend_stopped, remove_file_if_exists,
    spawn_result, stopped_marker, terminate_child,
};

const NETWORK_BOOT_IP: &str = "ip=169.254.0.2::169.254.0.1:255.255.255.252::eth0:off";
const MAX_API_RESPONSE_BYTES: usize = 64 * 1024;
const FIRECRACKER_LAUNCH_TOOLS: [&str; 3] = ["unshare", "mount", "sh"];
/// Stable in-namespace path every Firecracker owner sees as its root drive.
///
/// A Firecracker snapshot records the block device's host path, and
/// `PUT /snapshot/load` overrides only the network and vsock resources. Binding
/// each sandbox's own rootfs onto one shared path keeps that recorded path valid
/// for any sandbox, which is what lets one published template restore into many
/// independent sandboxes.
pub(crate) const PORTABLE_ROOTFS_PATH: &str = "/run/blaze-snapshot-view/rootfs.ext4";
#[cfg(target_os = "linux")]
const MOUNT_AND_EXEC: &str = r#"set -eu
rootfs_source=$1
rootfs_target=$2
binary=$3
api_socket=$4
instance_id=$5
shift 5
mount --bind "$rootfs_source" "$rootfs_target"
exec "$binary" --api-sock "$api_socket" --id "$instance_id" "$@"
"#;
/// Slowest guest-memory throughput a snapshot deadline still tolerates.
///
/// A control request such as `/version` or a pause should answer immediately, so
/// a short bound catches a wedged VMM. Snapshot work is different: it moves the
/// whole guest memory, so its duration scales with memory size and storage
/// speed, and a bound that does not scale with it would abandon work that is
/// still progressing. Because a timeout is reported as an unknown outcome, that
/// would fail checkpoints for larger guests and push them into recovery even
/// though Firecracker would have finished. Guest memory has no configured upper
/// bound, so no fixed deadline can be correct for every size.
///
/// Bare metal measured 512 MiB in about 29 s, so roughly 18 MiB/s once pause and
/// fsync overhead is included. This floor sits well below that so slower storage
/// still fits, because the deadline exists to catch a VMM that never answers, not
/// to enforce a latency target.
const SNAPSHOT_MIN_THROUGHPUT_BYTES_PER_SEC: u64 = 4 * 1024 * 1024;
/// Floor for a snapshot deadline, covering fixed request and fsync overhead.
const SNAPSHOT_TIMEOUT_FLOOR: Duration = Duration::from_secs(120);

/// Deadline for moving `memory_bytes` of guest memory.
fn snapshot_timeout(memory_bytes: u64) -> Duration {
    let scaled = Duration::from_secs(memory_bytes / SNAPSHOT_MIN_THROUGHPUT_BYTES_PER_SEC);
    scaled.max(SNAPSHOT_TIMEOUT_FLOOR)
}
const CHECKPOINT_SCRATCH_PREFIX: &str = ".firecracker-checkpoint-";
/// Names the Firecracker payload subtree carries. The layout inside a payload
/// belongs to the backend that wrote it, so these names are private to this
/// adapter: capture writes both files and restore reads the same two back.
/// The child-visible scratch is named from the same pair, so a transfer never
/// has to translate between two layouts.
const PAYLOAD_VM_STATE_FILE: &str = "vmstate.snap";
const PAYLOAD_MEMORY_FILE: &str = "memory.snap";
const CHECKPOINT_SCRATCH_FILES: [&str; 2] = [PAYLOAD_VM_STATE_FILE, PAYLOAD_MEMORY_FILE];

/// Firecracker backend factory.
pub struct FirecrackerSpawner {
    images_dir: PathBuf,
    /// Bound for each Firecracker HTTP request over the API socket.
    api_timeout: Duration,
    socket_timeout: Duration,
    network: Arc<NetworkManager>,
    network_required: bool,
    version: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum NetworkProcessState {
    PreSpawn,
    #[default]
    Launching,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct NetworkRecord {
    slot: usize,
    owner: Uuid,
    #[serde(default)]
    process_state: NetworkProcessState,
}

impl FirecrackerSpawner {
    /// Create a spawner without requiring host networking during startup
    /// probing. Individual network-enabled requests still run the full probe.
    pub fn new(images_dir: PathBuf) -> Self {
        Self {
            images_dir,
            api_timeout: Duration::from_secs(30),
            socket_timeout: Duration::from_secs(5),
            network: Arc::new(NetworkManager::default()),
            network_required: false,
            version: Mutex::new(None),
        }
    }

    /// Create a spawner whose startup probe includes network prerequisites
    /// when at least one loaded policy enables Firecracker networking.
    pub fn with_network_requirement(images_dir: PathBuf, network_required: bool) -> Self {
        Self {
            network_required,
            ..Self::new(images_dir)
        }
    }

    async fn network_probe_ready(&self) -> Result<bool> {
        if !self.network_required {
            return Ok(true);
        }
        self.network.probe().await
    }

    /// Start a Firecracker owner, optionally loading a checkpoint into it.
    ///
    /// A cold start writes a VM configuration and boots the guest kernel. A
    /// restore instead starts a bare VMM and hands it the captured VM state and
    /// guest memory, because the snapshot already carries the machine
    /// configuration the capture froze.
    async fn start(
        &self,
        request: BackendSpawnRequest,
        restore: Option<FirecrackerRestoreContext>,
    ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
        // A restore runs the file pinned during preflight, which was already
        // checked when it was opened; only a cold start resolves the path here.
        if restore.is_none() {
            validate_regular_file(&request.binary_path, "firecracker binary")?;
        }
        validate_regular_file(&request.storage.rootfs_path, "rootfs")?;
        if restore.is_none() {
            validate_regular_file(&self.images_dir.join("vmlinux"), "vmlinux")?;
        }
        prepare_portable_view_target().await?;
        let api_socket = request.run_dir.path().join("api.sock");
        let guest_socket = request.run_dir.path().join("vsock.uds");
        let pid_file = request.run_dir.path().join("firecracker.pid");
        let stopped_marker = stopped_marker(request.run_dir.path());
        let network_file = request.run_dir.path().join("network.json");
        let network_temp_file = network_metadata_temp(&network_file);
        remove_if_exists(&api_socket).await?;
        remove_if_exists(&guest_socket).await?;
        remove_file_if_exists(&stopped_marker).await?;
        remove_if_exists(&network_file).await?;
        remove_if_exists(&network_temp_file).await?;
        let fc_config = request
            .backend
            .firecracker
            .as_ref()
            .cloned()
            .unwrap_or_default();
        let network = if fc_config.enable_network {
            if !self.network.probe().await? {
                return Err(BlazeError::BackendError {
                    msg: "Firecracker networking is unavailable; it requires Linux root and executable ip, sysctl, and iptables commands".to_string(),
                }
                .into());
            }
            match self
                .network
                .create(request.instance_id, |slot| {
                    write_network_metadata(&network_file, slot)
                })
                .await
            {
                Ok(network) => Some(network),
                Err(error) => {
                    let (source, residual) = error.into_parts();
                    if let Some(network) = residual {
                        let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
                            request.instance_id,
                            None,
                            None,
                            runtime_files(
                                api_socket,
                                guest_socket,
                                pid_file,
                                stopped_marker,
                                network_file,
                            ),
                            Some(network),
                            self.network.clone(),
                            false,
                        ));
                        return Err(SpawnFailure::compensate_started(source, owner).await);
                    }
                    if let Err(cleanup) = remove_if_exists(&network_file).await {
                        let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
                            request.instance_id,
                            None,
                            None,
                            runtime_files(
                                api_socket,
                                guest_socket,
                                pid_file,
                                stopped_marker,
                                network_file,
                            ),
                            None,
                            self.network.clone(),
                            false,
                        ));
                        return Err(SpawnFailure::compensate_started(
                            BlazeError::BackendError {
                                msg: format!(
                                    "{source}; network metadata cleanup failed: {cleanup}"
                                ),
                            },
                            owner,
                        )
                        .await);
                    }
                    if let Err(cleanup) = remove_if_exists(&network_temp_file).await {
                        let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
                            request.instance_id,
                            None,
                            None,
                            runtime_files(
                                api_socket,
                                guest_socket,
                                pid_file,
                                stopped_marker,
                                network_file,
                            ),
                            None,
                            self.network.clone(),
                            false,
                        ));
                        return Err(SpawnFailure::compensate_started(
                            BlazeError::BackendError {
                                msg: format!(
                                    "{source}; temporary network metadata cleanup failed: {cleanup}"
                                ),
                            },
                            owner,
                        )
                        .await);
                    }
                    return Err(source.into());
                }
            }
        } else {
            None
        };

        // Guest memory size decides how long a snapshot of this VM may
        // legitimately take, including snapshots taken long after a restore. A
        // restore writes no VM configuration to resolve it from, so take it from
        // the captured image, whose size is the guest memory it holds. Reading it
        // from the reconstructed configuration instead would freeze the default
        // into a restored owner and leave a later capture of a large guest with
        // the minimum deadline.
        let memory_bytes = match restore.as_ref() {
            Some(context) => std::fs::metadata(&context.restore.memory)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
            None => resolve_memory(&fc_config, request.vm.as_ref())
                .map(|mib| mib.saturating_mul(1024 * 1024))
                .unwrap_or(0),
        };

        // A restore executes the pinned file rather than the configured path, so
        // replacing the binary after preflight cannot redirect this launch.
        let program = match restore.as_ref() {
            Some(context) => context.executable.program(),
            None => request.binary_path.clone(),
        };
        let mut command = build_launch_command(
            &program,
            network.as_ref(),
            &api_socket,
            request.instance_id,
            &request.storage.rootfs_path,
        );
        request.run_dir.inherit_into(&mut command);
        if let Some(context) = restore.as_ref() {
            context.executable.inherit_into(&mut command);
        }
        // A restore carries its machine configuration inside the snapshot, so
        // only a cold start writes and passes a configuration file.
        if restore.is_none() {
            let config_path = match write_vm_config(
                &self.images_dir,
                &request,
                &fc_config,
                &guest_socket,
                network.as_ref(),
            ) {
                Ok(path) => path,
                Err(error) => {
                    return Err(self
                        .compensate_before_spawn(
                            request.instance_id,
                            runtime_files(
                                api_socket,
                                guest_socket,
                                pid_file,
                                stopped_marker,
                                network_file,
                            ),
                            network,
                            error,
                        )
                        .await);
                }
            };
            command.arg("--config-file").arg(config_path);
        }
        if let Err(error) =
            configure_logs(&mut command, request.run_dir.path(), fc_config.serial_log)
        {
            return Err(self
                .compensate_before_spawn(
                    request.instance_id,
                    runtime_files(
                        api_socket,
                        guest_socket,
                        pid_file,
                        stopped_marker,
                        network_file,
                    ),
                    network,
                    error,
                )
                .await);
        }
        command.env("BLAZE_INSTANCE_ID", request.instance_id.to_string());
        if let Some(slot) = network.as_ref()
            && let Err(error) =
                write_network_record(&network_file, slot, NetworkProcessState::Launching)
        {
            return Err(self
                .compensate_before_spawn(
                    request.instance_id,
                    runtime_files(
                        api_socket,
                        guest_socket,
                        pid_file,
                        stopped_marker,
                        network_file,
                    ),
                    network,
                    error,
                )
                .await);
        }
        let pid_handoff = match configure_pid_handoff(&mut command, &pid_file) {
            Ok(pid_handoff) => pid_handoff,
            Err(error) => {
                return Err(self
                    .compensate_before_spawn(
                        request.instance_id,
                        runtime_files(
                            api_socket,
                            guest_socket,
                            pid_file,
                            stopped_marker,
                            network_file,
                        ),
                        network,
                        error,
                    )
                    .await);
            }
        };
        let child = command.spawn();
        drop(pid_handoff);
        let mut child = match child {
            Ok(child) => child,
            Err(source) => {
                return Err(self
                    .compensate_before_spawn(
                        request.instance_id,
                        runtime_files(
                            api_socket,
                            guest_socket,
                            pid_file,
                            stopped_marker,
                            network_file,
                        ),
                        network,
                        source.into(),
                    )
                    .await);
            }
        };
        if let Err(error) = wait_for_socket(&api_socket, &mut child, self.socket_timeout).await {
            let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
                request.instance_id,
                Some(child),
                None,
                runtime_files(
                    api_socket,
                    guest_socket,
                    pid_file,
                    stopped_marker,
                    network_file,
                ),
                network,
                self.network.clone(),
                false,
            ));
            return Err(SpawnFailure::compensate_started(error, owner).await);
        }

        // Resolve the version from the running VM rather than from the
        // configured binary, and freeze it into the owner. A concurrent binary
        // replacement therefore cannot make a capture claim a version this VM
        // was never started with.
        //
        // A VM that answers on its API socket but not for its version is still a
        // usable sandbox, so it keeps running and only gives up checkpoint capture:
        // the owner carries no capture context, reports no version, and
        // `supports_checkpoint_capture` is false, so a later capture is refused
        // before anything is paused. Treating this as fatal instead would turn a
        // lost checkpoint capability into a failed sandbox creation.
        let capture = match FirecrackerCapture::from_running(
            api_socket.clone(),
            self.api_timeout,
            memory_bytes,
        )
        .await
        {
            Ok(capture) => Some(capture),
            // A restore stays fatal: it must load the snapshot into a VM whose
            // version was confirmed to match the one that captured it, and an
            // unverified replacement is worse than a refused restore.
            Err(error) if restore.is_some() => {
                let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
                    request.instance_id,
                    Some(child),
                    None,
                    runtime_files(
                        api_socket,
                        guest_socket,
                        pid_file,
                        stopped_marker,
                        network_file,
                    ),
                    network,
                    self.network.clone(),
                    false,
                ));
                return Err(SpawnFailure::compensate_started(error, owner).await);
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    instance = %request.instance_id,
                    "Firecracker did not report a version; the sandbox stays running \
                     without checkpoint capture"
                );
                None
            }
        };

        let instance = FirecrackerInstance::new(
            request.instance_id,
            Some(child),
            capture,
            configured_runtime_files(
                runtime_files(
                    api_socket,
                    guest_socket,
                    pid_file,
                    stopped_marker,
                    network_file,
                ),
                fc_config.enable_vsock,
            ),
            network,
            self.network.clone(),
            fc_config.serial_log,
        )
        .with_run_dir(request.run_dir.clone());

        // Load the checkpoint only after the owner exists, so a failed load
        // transfers a runtime whose cleanup is still owned rather than leaking a
        // started VMM.
        if let Some(restore) = restore.as_ref().map(|context| &context.restore) {
            if let Err(error) = validate_restore_compatibility(
                restore,
                instance.version().unwrap_or_default(),
                &fc_config,
            ) {
                let owner: DynBackendInstance = Arc::new(instance);
                return Err(SpawnFailure::compensate_started(error, owner).await);
            }
            if let Err(error) = instance.load_snapshot(restore).await {
                let owner: DynBackendInstance = Arc::new(instance);
                return Err(SpawnFailure::compensate_started(error, owner).await);
            }
        }
        Ok(Arc::new(instance))
    }

    async fn compensate_before_spawn(
        &self,
        instance_id: Uuid,
        files: FirecrackerRuntimeFiles,
        network: Option<NetworkSlot>,
        source: BlazeError,
    ) -> SpawnFailure {
        if network.is_none() {
            return SpawnFailure::clean(source);
        }
        let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
            instance_id,
            None,
            None,
            files,
            network,
            self.network.clone(),
            false,
        ));
        SpawnFailure::compensate_started(source, owner).await
    }
}

#[async_trait]
impl BackendSpawner for FirecrackerSpawner {
    async fn prepare_spawn(&self, run_dir: &OwnedRunDir) -> Result<()> {
        prepare_pid_handoff(&run_dir.path().join("firecracker.pid"))
    }

    async fn spawn(
        &self,
        request: BackendSpawnRequest,
    ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
        self.start(request, None).await
    }

    /// Report what this executable can consume from a committed checkpoint.
    ///
    /// The version is read from the configured binary, so a restore is only
    /// attempted when the checkpoint recorded that exact version.
    async fn restore_capability(
        &self,
        executable: Option<&PinnedExecutable>,
    ) -> Result<Option<RestoreCapability>> {
        let executable = executable.ok_or_else(|| BlazeError::BackendError {
            msg: "no firecracker binary is configured, so its version cannot be \
                  compared with the checkpoint"
                .to_string(),
        })?;
        Ok(Some(RestoreCapability {
            backend: BackendKind::Firecracker,
            version: Some(read_pinned_backend_version(executable).await?),
            snapshot_kind: SnapshotKind::Full,
        }))
    }

    async fn restore(&self, request: BackendRestoreRequest) -> RestoreResult {
        let run_dir = request.run_dir.clone();
        let RestoreRequest {
            instance_id,
            binary_path,
            storage,
            payload_dir,
            checkpoint_backend,
            expected_version,
            snapshot_kind,
            expose_guest_socket,
            preserve_network,
            record_console_log,
            // Firecracker binds no sandbox identity into its snapshot, so a
            // template capture and a rollback capture load the same way.
            snapshot_from_other_sandbox: _,
        } = request.request;
        let executable = request
            .executable
            .clone()
            .ok_or_else(|| BlazeError::BackendError {
                msg: "no firecracker binary is configured".to_string(),
            })
            .map_err(SpawnFailure::clean)?;
        let restore = FirecrackerRestore {
            // The payload subtree is this adapter's own capture output, so the
            // two files are addressed by the names capture wrote.
            vm_state: payload_dir.join(PAYLOAD_VM_STATE_FILE),
            memory: payload_dir.join(PAYLOAD_MEMORY_FILE),
            expected_version,
            checkpoint_backend,
            snapshot_kind,
            expose_guest_socket,
        };
        let spawn = BackendSpawnRequest::new(
            SpawnRequest {
                instance_id,
                binary_path,
                storage,
                backend: BackendConfigs {
                    // Three of these fields shape the host and must travel with
                    // the request rather than fall back to defaults:
                    // `enable_network` and `enable_vsock` name host devices the
                    // snapshot references, and `serial_log` decides whether
                    // console output keeps being recorded.
                    //
                    // `boot_args`, `vcpus` and `memory` only feed
                    // `write_vm_config`, which a restore skips because the
                    // snapshot carries the machine configuration. Guest memory
                    // size is still needed, for snapshot deadlines, but `start`
                    // takes it from the captured image rather than from here.
                    firecracker: Some(FirecrackerConfig {
                        enable_vsock: expose_guest_socket,
                        enable_network: preserve_network,
                        serial_log: record_console_log,
                        ..FirecrackerConfig::default()
                    }),
                },
                vm: None,
            },
            run_dir,
        )
        .map_err(SpawnFailure::clean)?;
        self.start(
            spawn,
            Some(FirecrackerRestoreContext {
                restore,
                executable,
            }),
        )
        .await
    }

    async fn probe(&self, binary_path: &Path) -> Result<bool> {
        if !binary_path.is_file() || !firecracker_launch_tools_available(executable_in_path) {
            return Ok(false);
        }
        if !self.network_probe_ready().await? {
            return Ok(false);
        }
        match read_backend_version(binary_path).await {
            Ok(version) => {
                *self.version.lock().await = Some(version);
                Ok(true)
            }
            Err(error) => {
                tracing::debug!(%error, binary = %binary_path.display(), "firecracker version probe failed");
                Ok(false)
            }
        }
    }

    async fn cleanup_orphan(&self, instance_id: Uuid, run_dir: &OwnedRunDir) -> Result<()> {
        cleanup_orphan_run_dir_with(instance_id, run_dir.path(), &self.network).await
    }
}

struct FirecrackerInstance {
    instance_id: Uuid,
    child: Mutex<Option<Child>>,
    exit_result: Mutex<Option<SpawnResult>>,
    /// API ownership plus the version frozen when this owner started.
    ///
    /// Absent when the API could not be reached, which also disables checkpoint
    /// capture instead of reporting a capability the owner cannot honour.
    capture: Option<FirecrackerCapture>,
    /// Retained runtime directory used to root the private snapshot scratch.
    run_dir: Option<OwnedRunDir>,
    /// Whether this owner created a per-sandbox netns, tap and NAT slot.
    ///
    /// Recorded at construction because the shape cannot change afterwards, and
    /// because a restore needs it without awaiting the network lock.
    holds_network_slot: bool,
    /// Whether this owner sends guest console output to `serial.log`.
    records_console_log: bool,
    files: FirecrackerRuntimeFiles,
    network: Mutex<Option<NetworkSlot>>,
    network_manager: Arc<NetworkManager>,
    cleanup_complete: AtomicBool,
    killed: AtomicBool,
}

struct FirecrackerRuntimeFiles {
    api_socket: PathBuf,
    guest_socket: PathBuf,
    pid_file: PathBuf,
    stopped_marker: PathBuf,
    network_file: PathBuf,
}

fn runtime_files(
    api_socket: PathBuf,
    guest_socket: PathBuf,
    pid_file: PathBuf,
    stopped_marker: PathBuf,
    network_file: PathBuf,
) -> FirecrackerRuntimeFiles {
    FirecrackerRuntimeFiles {
        api_socket,
        guest_socket,
        pid_file,
        stopped_marker,
        network_file,
    }
}

fn configured_runtime_files(
    mut files: FirecrackerRuntimeFiles,
    enable_vsock: bool,
) -> FirecrackerRuntimeFiles {
    if !enable_vsock {
        files.guest_socket = PathBuf::new();
    }
    files
}

impl FirecrackerInstance {
    fn new(
        instance_id: Uuid,
        child: Option<Child>,
        capture: Option<FirecrackerCapture>,
        files: FirecrackerRuntimeFiles,
        network: Option<NetworkSlot>,
        network_manager: Arc<NetworkManager>,
        records_console_log: bool,
    ) -> Self {
        Self {
            instance_id,
            child: Mutex::new(child),
            exit_result: Mutex::new(None),
            capture,
            run_dir: None,
            holds_network_slot: network.is_some(),
            records_console_log,
            files,
            network: Mutex::new(network),
            network_manager,
            cleanup_complete: AtomicBool::new(false),
            killed: AtomicBool::new(false),
        }
    }

    fn with_run_dir(mut self, run_dir: OwnedRunDir) -> Self {
        self.run_dir = Some(run_dir);
        self
    }

    /// Borrow the API client, refusing checkpoint work without API ownership.
    fn capture_api(&self) -> Result<&FirecrackerApiClient> {
        self.capture
            .as_ref()
            .map(|capture| &capture.api)
            .ok_or_else(|| BlazeError::BackendError {
                msg: "Firecracker API ownership is unavailable".to_string(),
            })
    }

    /// Load a checkpoint into this freshly started VMM.
    ///
    /// The tap name comes from the network slot this owner just created, so the
    /// restored guest attaches to the device that exists now rather than the one
    /// recorded at capture.
    async fn load_snapshot(&self, restore: &FirecrackerRestore) -> Result<()> {
        let tap_name = self
            .network
            .lock()
            .await
            .as_ref()
            .map(|network| network.tap_name().to_string());
        let guest_socket = restore
            .expose_guest_socket
            .then(|| self.files.guest_socket.clone());
        // A load reads the captured memory image back, and the owner already
        // holds that image's size as this VM's guest memory, so both directions
        // are bounded from one source rather than two guesses.
        let memory_bytes = self
            .capture
            .as_ref()
            .map(|capture| capture.memory_bytes)
            .unwrap_or(0);
        self.capture_api()?
            .call_json_within(
                Method::PUT,
                "/snapshot/load",
                Some(snapshot_load_payload(
                    restore,
                    tap_name.as_deref(),
                    guest_socket.as_deref(),
                )?),
                snapshot_timeout(memory_bytes),
            )
            .await
            .map(|_| ())
            .map_err(FirecrackerApiError::into_error)
    }
}

#[async_trait]
impl BackendInstance for FirecrackerInstance {
    fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    fn backend(&self) -> BackendKind {
        BackendKind::Firecracker
    }

    /// Report the version frozen into this owner when it started.
    ///
    /// Firecracker snapshot formats are tied to the exact binary version, so a
    /// capture records this value and a later restore refuses to load a snapshot
    /// taken by a different build.
    fn version(&self) -> Option<&str> {
        self.capture
            .as_ref()
            .map(|capture| capture.backend_version.as_str())
    }

    fn supports_checkpoint_capture(&self) -> bool {
        self.capture.is_some()
    }

    fn guest_socket_path(&self) -> &Path {
        &self.files.guest_socket
    }

    fn holds_network_slot(&self) -> bool {
        self.holds_network_slot
    }

    fn records_console_log(&self) -> bool {
        self.records_console_log
    }

    async fn pause(&self) -> Result<()> {
        self.capture_api()?
            .call_json(
                Method::PATCH,
                "/vm",
                Some(serde_json::json!({"state": "Paused"})),
            )
            .await
            .map(|_| ())
            .map_err(FirecrackerApiError::into_error)
    }

    async fn resume(&self) -> Result<()> {
        self.capture_api()?
            .call_json(
                Method::PATCH,
                "/vm",
                Some(serde_json::json!({"state": "Resumed"})),
            )
            .await
            .map(|_| ())
            .map_err(FirecrackerApiError::into_error)
    }

    /// Capture a full snapshot into the payload subtree this backend owns.
    ///
    /// Firecracker writes snapshot files itself, and it runs inside a private
    /// mount namespace, so the request names a scratch directory below the
    /// runtime directory it already owns. The artifacts are then transferred
    /// into the payload subtree under the two names this adapter reads back on
    /// restore; no component outside it needs that layout.
    ///
    /// The scratch is only reclaimed when the outcome is known. A rejected
    /// request means Firecracker never wrote anything, so the scratch is
    /// removed. An unknown outcome means Firecracker may still be writing into
    /// it, so the scratch is retained for reconciliation instead of being
    /// deleted underneath a live writer.
    async fn snapshot(&self, request: SnapshotRequest) -> Result<()> {
        let capture = self
            .capture
            .as_ref()
            .ok_or_else(|| BlazeError::BackendError {
                msg: "Firecracker API ownership is unavailable".to_string(),
            })?;
        let run_dir = self
            .run_dir
            .as_ref()
            .ok_or_else(|| BlazeError::BackendError {
                msg: "Firecracker runtime-directory ownership is unavailable".to_string(),
            })?;
        let SnapshotRequest {
            payload_dir,
            kind: SnapshotKind::Full,
        } = request;
        let snapshot_path = payload_dir.join(PAYLOAD_VM_STATE_FILE);
        let mem_path = payload_dir.join(PAYLOAD_MEMORY_FILE);
        let scratch = SnapshotScratch::new(run_dir)?;
        let snapshot_child_path = scratch.snapshot_path.clone();
        let memory_child_path = scratch.memory_path.clone();
        let api_result = capture
            .api
            .call_json_within(
                Method::PUT,
                "/snapshot/create",
                Some(serde_json::json!({
                    "snapshot_path": snapshot_child_path,
                    "mem_file_path": memory_child_path,
                    "snapshot_type": "Full"
                })),
                snapshot_timeout(capture.memory_bytes),
            )
            .await;
        match api_result {
            Ok(_) => {}
            Err(FirecrackerApiError::Known(error)) => {
                if let Err(cleanup) = scratch.cleanup() {
                    return Err(unknown_scratch_cleanup_error(
                        Some(error),
                        cleanup,
                        "rejected snapshot request",
                    ));
                }
                return Err(error);
            }
            Err(FirecrackerApiError::Unknown(error)) => {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "{error}; Firecracker snapshot has an unknown outcome and its \
                         checkpoint scratch is retained for reconciliation"
                    ),
                });
            }
        }
        crate::failpoint::spawn_blocking(move || scratch.transfer_into(&snapshot_path, &mem_path))
            .await
            .map_err(|error| BlazeError::BackendError {
                msg: format!(
                    "Firecracker snapshot transfer task failed with an unknown outcome: {error}"
                ),
            })?
    }

    async fn try_wait(&self) -> Result<Option<SpawnResult>> {
        let result = {
            let mut guard = self.child.lock().await;
            let Some(child) = guard.as_mut() else {
                let result = self.exit_result.lock().await.unwrap_or(SpawnResult {
                    instance_id: self.instance_id,
                    exit_code: None,
                    signal: None,
                });
                drop(guard);
                self.cleanup().await?;
                return Ok(Some(result));
            };
            let Some(status) = child.try_wait()? else {
                return Ok(None);
            };
            record_backend_stopped(&self.files.stopped_marker).await?;
            let result = spawn_result(self.instance_id, status);
            *self.exit_result.lock().await = Some(result);
            *guard = None;
            result
        };
        self.cleanup().await?;
        Ok(Some(result))
    }

    async fn kill(&self) -> Result<()> {
        if self.killed.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut guard = self.child.lock().await;
        if self.killed.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(child) = guard.as_mut() {
            terminate_child(child, "firecracker").await?;
        }
        record_backend_stopped(&self.files.stopped_marker).await?;
        *guard = None;
        drop(guard);
        self.cleanup().await?;
        self.killed.store(true, Ordering::Release);
        Ok(())
    }
}

struct FirecrackerCapture {
    api: FirecrackerApiClient,
    backend_version: String,
    /// Guest memory size, which sets how long a snapshot may legitimately take.
    memory_bytes: u64,
}

impl FirecrackerCapture {
    #[cfg(all(test, target_os = "linux"))]
    fn new(
        api_socket: PathBuf,
        api_timeout: Duration,
        backend_version: String,
        memory_bytes: u64,
    ) -> Self {
        Self {
            api: FirecrackerApiClient::new(api_socket, api_timeout),
            backend_version,
            memory_bytes,
        }
    }

    async fn from_running(
        api_socket: PathBuf,
        api_timeout: Duration,
        memory_bytes: u64,
    ) -> Result<Self> {
        let api = FirecrackerApiClient::new(api_socket, api_timeout);
        let response = api
            .call_json(Method::GET, "/version", None)
            .await
            .map_err(FirecrackerApiError::into_error)?;
        let backend_version = parse_runtime_version(&response)?;
        Ok(Self {
            api,
            backend_version,
            memory_bytes,
        })
    }
}

#[derive(Debug)]
enum FirecrackerApiError {
    Known(BlazeError),
    Unknown(BlazeError),
}

impl FirecrackerApiError {
    fn into_error(self) -> BlazeError {
        match self {
            Self::Known(error) | Self::Unknown(error) => error,
        }
    }
}

/// Checkpoint artifacts and identity a restore must honour.
///
/// The generic restore transaction already verified these artifacts and their
/// hashes, so the adapter receives the paths directly and only needs to confirm
/// that this backend can consume them.
/// A restore in progress, with the executable pinned during preflight.
///
/// The pin travels with the restore so the launch runs the same file the
/// capability check accepted, rather than whatever the configured path names by
/// the time the replacement starts.
struct FirecrackerRestoreContext {
    restore: FirecrackerRestore,
    executable: Arc<PinnedExecutable>,
}

struct FirecrackerRestore {
    vm_state: PathBuf,
    memory: PathBuf,
    expected_version: Option<String>,
    checkpoint_backend: BackendKind,
    snapshot_kind: SnapshotKind,
    expose_guest_socket: bool,
}

/// Refuse a checkpoint this executable cannot load.
///
/// Firecracker serializes VM state in a version-specific layout, so loading a
/// snapshot taken by a different build can fail in ways that are not detectable
/// up front. The network and guest-transport shapes must match too, because the
/// snapshot references devices by name and a mismatch would restore a VM whose
/// devices do not exist.
fn validate_restore_compatibility(
    restore: &FirecrackerRestore,
    actual_version: &str,
    config: &FirecrackerConfig,
) -> Result<()> {
    if restore.checkpoint_backend != BackendKind::Firecracker {
        return Err(BlazeError::BackendError {
            msg: format!(
                "checkpoint backend {:?} cannot be restored by Firecracker",
                restore.checkpoint_backend
            ),
        });
    }
    if restore.snapshot_kind != SnapshotKind::Full {
        return Err(BlazeError::BackendError {
            msg: "Firecracker restore requires a full snapshot".to_string(),
        });
    }
    match restore.expected_version.as_deref() {
        Some(expected) if expected == actual_version => {}
        Some(expected) => {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "Firecracker checkpoint version {expected:?} does not match \
                     executable version {actual_version:?}"
                ),
            });
        }
        None => {
            return Err(BlazeError::BackendError {
                msg: "Firecracker checkpoint records no backend version".to_string(),
            });
        }
    }
    if restore.expose_guest_socket != config.enable_vsock {
        return Err(BlazeError::BackendError {
            msg: "Firecracker checkpoint guest transport does not match restore policy".to_string(),
        });
    }
    Ok(())
}

/// Build the `/snapshot/load` request body.
///
/// The tap device and guest socket are recreated with fresh host names on every
/// start, so the load overrides the names the snapshot recorded instead of
/// requiring the previous host resources to still exist.
fn snapshot_load_payload(
    restore: &FirecrackerRestore,
    tap_name: Option<&str>,
    guest_socket: Option<&Path>,
) -> Result<serde_json::Value> {
    let mut payload = serde_json::json!({
        "snapshot_path": path_string(&restore.vm_state, "VM-state snapshot")?,
        "mem_backend": {
            "backend_type": "File",
            "backend_path": path_string(&restore.memory, "memory snapshot")?
        },
        "track_dirty_pages": false,
        "resume_vm": true
    });
    if let Some(tap_name) = tap_name {
        payload["network_overrides"] = serde_json::json!([{
            "iface_id": "eth0",
            "host_dev_name": tap_name
        }]);
    }
    if let Some(guest_socket) = guest_socket {
        payload["vsock_override"] = serde_json::json!({
            "uds_path": path_string(guest_socket, "guest socket")?
        });
    }
    Ok(payload)
}

/// Private child-visible snapshot namespace.  Its path is rooted at the
/// runtime directory descriptor inherited by Firecracker, never at a daemon
/// checkpoint descriptor or a configured runtime pathname.
struct SnapshotScratch {
    directory: PathBuf,
    snapshot_path: PathBuf,
    memory_path: PathBuf,
}

impl SnapshotScratch {
    fn new(run_dir: &OwnedRunDir) -> Result<Self> {
        let directory = run_dir
            .path()
            .join(format!("{CHECKPOINT_SCRATCH_PREFIX}{}", Uuid::new_v4()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&directory)?;
        Ok(Self {
            snapshot_path: directory.join(PAYLOAD_VM_STATE_FILE),
            memory_path: directory.join(PAYLOAD_MEMORY_FILE),
            directory,
        })
    }

    /// Move captured artifacts to their destinations and reclaim the scratch.
    ///
    /// A transfer failure whose scratch was reclaimed is reported as-is. When the
    /// scratch itself cannot be reclaimed the outcome is unknown, so the error
    /// says so and the retained directory waits for reconciliation.
    fn transfer_into(self, snapshot_target: &Path, memory_target: &Path) -> Result<()> {
        let transfer = (|| {
            transfer_snapshot_file(&self.snapshot_path, snapshot_target)?;
            transfer_snapshot_file(&self.memory_path, memory_target)?;
            Ok(())
        })();
        let cleanup = self.cleanup();
        match (transfer, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(cleanup)) => Err(unknown_scratch_cleanup_error(
                None,
                cleanup,
                "completed snapshot transfer",
            )),
            (Err(error), Err(cleanup)) => Err(unknown_scratch_cleanup_error(
                Some(error),
                cleanup,
                "failed snapshot transfer",
            )),
        }
    }

    fn cleanup(&self) -> Result<()> {
        cleanup_snapshot_scratch_directory(&self.directory)?;
        let parent = self
            .directory
            .parent()
            .ok_or_else(|| BlazeError::BackendError {
                msg: format!(
                    "Firecracker checkpoint scratch {} has no parent directory",
                    self.directory.display()
                ),
            })?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

/// Describe a boundary whose outcome could not be confirmed.
///
/// Retained scratch is the only remaining evidence of what Firecracker wrote, so
/// the message names the boundary and keeps the original cause alongside the
/// cleanup failure.
fn unknown_scratch_cleanup_error(
    original: Option<BlazeError>,
    cleanup: BlazeError,
    boundary: &str,
) -> BlazeError {
    let original = original
        .map(|error| format!("{error}; "))
        .unwrap_or_default();
    BlazeError::BackendError {
        msg: format!(
            "{original}Firecracker {boundary} could not clean its checkpoint scratch, so the \
             outcome is unknown and the scratch is retained: {cleanup}"
        ),
    }
}

fn cleanup_snapshot_scratch(run_dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(run_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(id) = name.strip_prefix(CHECKPOINT_SCRATCH_PREFIX) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(id) else {
            continue;
        };
        if format!("{CHECKPOINT_SCRATCH_PREFIX}{id}") != name {
            continue;
        }
        cleanup_snapshot_scratch_directory(&entry.path())?;
    }
    std::fs::File::open(run_dir)?.sync_all()?;
    Ok(())
}

fn cleanup_snapshot_scratch_directory(directory: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir() {
        return Err(BlazeError::BackendError {
            msg: format!(
                "Firecracker checkpoint scratch {} is not a directory",
                directory.display()
            ),
        });
    }
    let mut artifacts = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "Firecracker checkpoint scratch {} contains a non-UTF-8 entry",
                    directory.display()
                ),
            });
        };
        if !CHECKPOINT_SCRATCH_FILES.contains(&name) {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "Firecracker checkpoint scratch {} contains unexpected entry {name}",
                    directory.display()
                ),
            });
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_file() {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "Firecracker checkpoint scratch artifact {} is not a regular file",
                    entry.path().display()
                ),
            });
        }
        artifacts.push(entry.path());
    }
    for artifact in artifacts {
        std::fs::remove_file(artifact)?;
    }
    std::fs::remove_dir(directory)?;
    Ok(())
}

fn transfer_snapshot_file(source: &Path, target: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if !metadata.file_type().is_file() {
        return Err(BlazeError::BackendError {
            msg: format!(
                "Firecracker snapshot artifact {} is not a regular file",
                source.display()
            ),
        });
    }
    let parent = target.parent().ok_or_else(|| BlazeError::BackendError {
        msg: format!("checkpoint target {} has no parent", target.display()),
    })?;
    let name = target.file_name().ok_or_else(|| BlazeError::BackendError {
        msg: format!("checkpoint target {} has no file name", target.display()),
    })?;
    let temporary = parent.join(format!(
        ".{}.firecracker-transfer-{}",
        name.to_string_lossy(),
        Uuid::new_v4()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut input = std::fs::File::open(source)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        std::fs::rename(&temporary, target)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}

#[derive(Debug, Clone)]
struct FirecrackerApiClient {
    socket: PathBuf,
    timeout: Duration,
}

impl FirecrackerApiClient {
    fn new(socket: PathBuf, timeout: Duration) -> Self {
        Self { socket, timeout }
    }

    /// Issue a control request under the short bound.
    async fn call_json(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> std::result::Result<Vec<u8>, FirecrackerApiError> {
        self.call_json_within(method, path, body, self.timeout)
            .await
    }

    /// Issue a request under a caller-chosen bound.
    async fn call_json_within(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
        timeout: Duration,
    ) -> std::result::Result<Vec<u8>, FirecrackerApiError> {
        let operation = async {
            let stream = UnixStream::connect(&self.socket)
                .await
                .map_err(|error| FirecrackerApiError::Known(error.into()))?;
            let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
                .await
                .map_err(|error| FirecrackerApiError::Known(backend_protocol_error(error)))?;
            tokio::spawn(async move {
                if let Err(error) = connection.await {
                    tracing::debug!(%error, "firecracker API connection ended");
                }
            });
            let bytes = match body {
                Some(body) => serde_json::to_vec(&body).map_err(|error| {
                    FirecrackerApiError::Known(BlazeError::BackendError {
                        msg: format!("serialize Firecracker API request: {error}"),
                    })
                })?,
                None => Vec::new(),
            };
            let mut builder = Request::builder()
                .method(method.clone())
                .uri(format!("http://localhost{path}"));
            if !bytes.is_empty() {
                builder = builder.header("content-type", "application/json");
            }
            let request = builder
                .body(Full::new(Bytes::from(bytes)))
                .map_err(|error| {
                    FirecrackerApiError::Known(BlazeError::BackendError {
                        msg: format!("build Firecracker API request: {error}"),
                    })
                })?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|error| FirecrackerApiError::Unknown(backend_protocol_error(error)))?;
            let status = response.status();
            let mut response_body = response.into_body();
            let mut collected = Vec::new();
            while let Some(frame) = response_body.frame().await {
                let frame = frame
                    .map_err(|error| FirecrackerApiError::Unknown(backend_protocol_error(error)))?;
                if let Ok(data) = frame.into_data() {
                    let remaining = MAX_API_RESPONSE_BYTES.saturating_sub(collected.len());
                    collected.extend_from_slice(&data[..data.len().min(remaining)]);
                    if data.len() > remaining {
                        return Err(FirecrackerApiError::Unknown(BlazeError::BackendError {
                            msg: format!(
                                "Firecracker {method} {path} response exceeded \
                                 {MAX_API_RESPONSE_BYTES} bytes"
                            ),
                        }));
                    }
                }
            }
            if !status.is_success() {
                return Err(FirecrackerApiError::Known(BlazeError::BackendError {
                    msg: format!(
                        "Firecracker {method} {path} returned {status}: {}",
                        String::from_utf8_lossy(&collected)
                    ),
                }));
            }
            Ok(collected)
        };
        tokio::time::timeout(timeout, operation)
            .await
            .map_err(|_| {
                FirecrackerApiError::Unknown(BlazeError::BackendError {
                    msg: format!("Firecracker {method} {path} timed out after {timeout:?}"),
                })
            })?
    }
}

impl FirecrackerInstance {
    async fn cleanup(&self) -> Result<()> {
        if self.cleanup_complete.load(Ordering::Acquire) {
            return Ok(());
        }
        remove_if_exists(&self.files.api_socket).await?;
        remove_if_exists(&self.files.guest_socket).await?;
        remove_if_exists(&self.files.pid_file).await?;
        // Reclaim scratch a capture retained after an unknown outcome. Destroy
        // is the point where the writer is gone for certain.
        if let Some(run_dir) = self.run_dir.as_ref() {
            cleanup_snapshot_scratch(run_dir.path())?;
        }
        let mut network = self.network.lock().await;
        if let Some(slot) = network.as_ref().cloned() {
            self.network_manager.destroy(&slot).await?;
            *network = None;
        }
        remove_if_exists(&self.files.network_file).await?;
        remove_if_exists(&network_metadata_temp(&self.files.network_file)).await?;
        self.cleanup_complete.store(true, Ordering::Release);
        Ok(())
    }
}

/// Resolve the effective vCPU and memory-MiB shape for one Firecracker config.
///
/// Template create uses this to confirm that a published snapshot's recorded VM
/// shape matches the shape the current policy would launch, using the same
/// precedence (backend override, then policy `[vm]`, then code default) as the
/// normal boot path in [`write_vm_config`].
pub(crate) fn effective_vm_shape(
    config: &FirecrackerConfig,
    vm: Option<&VmConfig>,
) -> Result<(u32, u64)> {
    let vcpus = config.vcpus.or(vm.map(|vm| vm.vcpus)).unwrap_or(1);
    let memory_mib = resolve_memory(config, vm)?;
    Ok((vcpus, memory_mib))
}

/// Resolve the kernel command line a cold start would write into Firecracker's
/// machine configuration.
///
/// Networking uses one fixed guest address. Keep that derived argument in one
/// place so a template restore can compare its captured command line with the
/// exact command line a cold start under the same policy would use.
pub(crate) fn effective_boot_args(
    config: &FirecrackerConfig,
    network_enabled: bool,
) -> Result<String> {
    let mut boot_args = config.boot_args.clone();
    if network_enabled {
        let network_arguments = boot_args
            .split_whitespace()
            .filter(|argument| argument.starts_with("ip="))
            .collect::<Vec<_>>();
        match network_arguments.as_slice() {
            [] => {
                boot_args.push(' ');
                boot_args.push_str(NETWORK_BOOT_IP);
            }
            [argument] if *argument == NETWORK_BOOT_IP => {}
            arguments => {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "Firecracker networking requires exactly {NETWORK_BOOT_IP:?}, found {}",
                        arguments
                            .iter()
                            .map(|argument| format!("{argument:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
        }
    }
    Ok(boot_args)
}

fn write_vm_config(
    images_dir: &Path,
    request: &BackendSpawnRequest,
    config: &FirecrackerConfig,
    guest_socket: &Path,
    network: Option<&NetworkSlot>,
) -> Result<PathBuf> {
    let vcpus = config
        .vcpus
        .or(request.vm.as_ref().map(|vm| vm.vcpus))
        .unwrap_or(1);
    let memory_mib = resolve_memory(config, request.vm.as_ref())?;
    let boot_args = effective_boot_args(config, network.is_some())?;
    let mut value = serde_json::json!({
        "boot-source": {
            "kernel_image_path": path_string(&images_dir.join("vmlinux"), "vmlinux")?,
            "boot_args": boot_args
        },
        "drives": [{
            "drive_id": "rootfs",
            // Name the stable in-namespace path, not this sandbox's own path, so
            // a snapshot captured here stays loadable by another sandbox.
            "path_on_host": PORTABLE_ROOTFS_PATH,
            "is_root_device": true,
            "is_read_only": false
        }],
        "machine-config": {
            "vcpu_count": vcpus,
            "mem_size_mib": memory_mib
        }
    });
    if config.enable_vsock {
        value["vsock"] = serde_json::json!({
            "guest_cid": 3,
            "uds_path": path_string(guest_socket, "guest socket")?
        });
    }
    if let Some(network) = network {
        value["network-interfaces"] = serde_json::json!([{
            "iface_id": "eth0",
            "guest_mac": "02:FC:00:00:00:02",
            "host_dev_name": network.tap_name()
        }]);
    }
    let path = request.run_dir.path().join("vmconfig.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&value).map_err(|error| BlazeError::BackendError {
            msg: format!("serialize Firecracker VM config: {error}"),
        })?,
    )?;
    Ok(path)
}

fn resolve_memory(config: &FirecrackerConfig, vm: Option<&VmConfig>) -> Result<u64> {
    let value = config
        .memory
        .as_deref()
        .or_else(|| vm.map(|vm| vm.memory.as_str()))
        .unwrap_or("256Mi");
    parse_memory_value(value)
        .map(to_mib_ceil)
        .map_err(|error| BlazeError::BackendError {
            msg: format!("invalid Firecracker memory {value:?}: {error}"),
        })
}

/// Ensure the shared bind-mount target for the portable rootfs path exists.
///
/// The target is only a mount point: each sandbox binds its own rootfs over it
/// inside a private mount namespace, so the empty file on the host is never
/// read and no sandbox observes another's mount.
#[cfg(target_os = "linux")]
async fn prepare_portable_view_target() -> Result<()> {
    let target = Path::new(PORTABLE_ROOTFS_PATH);
    prepare_portable_view_target_at(target).await
}

#[cfg(any(target_os = "linux", all(test, unix)))]
async fn prepare_portable_view_target_at(target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| BlazeError::BackendError {
                msg: format!(
                    "cannot create snapshot view directory {}: {error}",
                    parent.display()
                ),
            })?;
    }
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = tokio::fs::symlink_metadata(target).await.map_err(|error| {
                BlazeError::BackendError {
                    msg: format!(
                        "cannot inspect snapshot view target {}: {error}",
                        target.display()
                    ),
                }
            })?;
            if !metadata.file_type().is_file() {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "snapshot view target {} is not a regular file",
                        target.display()
                    ),
                });
            }
            Ok(())
        }
        Err(error) => Err(BlazeError::BackendError {
            msg: format!(
                "cannot create snapshot view target {}: {error}",
                target.display()
            ),
        }),
    }
}

#[cfg(not(target_os = "linux"))]
async fn prepare_portable_view_target() -> Result<()> {
    Ok(())
}

fn build_launch_command(
    binary: &Path,
    network: Option<&NetworkSlot>,
    api_socket: &Path,
    instance_id: Uuid,
    rootfs_source: &Path,
) -> Command {
    #[cfg(target_os = "linux")]
    let mut command = if let Some(network) = network {
        let mut command = Command::new("ip");
        command
            .arg("netns")
            .arg("exec")
            .arg(network.netns())
            .arg("unshare")
            .arg("--mount")
            .arg("--propagation")
            .arg("private")
            .arg("--");
        command
    } else {
        let mut command = Command::new("unshare");
        command
            .arg("--mount")
            .arg("--propagation")
            .arg("private")
            .arg("--");
        command
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = {
        let _ = (network, rootfs_source);
        Command::new(binary)
    };
    // Bind this sandbox's own rootfs onto one stable path inside the private
    // mount namespace, then exec Firecracker. The machine configuration a
    // snapshot records therefore names a path that resolves to whichever
    // sandbox is running, so a snapshot captured by one sandbox restores
    // against the restoring sandbox's independent copy instead of the
    // capture-time path.
    #[cfg(target_os = "linux")]
    command
        .arg("sh")
        .arg("-c")
        .arg(MOUNT_AND_EXEC)
        .arg("blaze-firecracker")
        .arg(rootfs_source)
        .arg(PORTABLE_ROOTFS_PATH)
        .arg(binary)
        .arg(api_socket)
        .arg(format!("fc-{instance_id}"));
    #[cfg(not(target_os = "linux"))]
    {
        command.arg("--api-sock").arg(api_socket);
        command.arg("--id").arg(format!("fc-{instance_id}"));
    }
    command
}

fn configure_logs(command: &mut Command, run_dir: &Path, serial_log: bool) -> Result<()> {
    if serial_log {
        let serial_log = run_dir.join("serial.log");
        rotate_serial_log_if_needed(&serial_log)?;
        // Append rather than truncate, here and for stderr below. A cold start
        // owns a fresh runtime directory, so there is nothing to append to, but
        // a restore reuses the directory of the sandbox it replaces; truncating
        // would erase the console output and VMM diagnostics captured before the
        // restore, including the diagnostics a failed replacement would be
        // debugged from. Rotation above still bounds the console log.
        let stdout = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(serial_log)?;
        command.stdout(stdout);
    } else {
        command.stdout(Stdio::null());
    }
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("stderr.log"))?;
    command.stderr(stderr);
    command.stdin(Stdio::null());
    Ok(())
}

/// Read the version from a pinned executable rather than a path.
async fn read_pinned_backend_version(executable: &PinnedExecutable) -> Result<String> {
    let program = executable.program();
    let mut command = Command::new(&program);
    command.arg("--version");
    executable.inherit_into(&mut command);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| BlazeError::BackendError {
            msg: format!(
                "firecracker probe timed out: {}",
                executable.configured_path().display()
            ),
        })??;
    if !output.status.success() {
        return Err(BlazeError::BackendError {
            msg: format!(
                "firecracker version probe failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    parse_backend_version(&output.stdout)
}

async fn read_backend_version(binary_path: &Path) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(binary_path).arg("--version").output(),
    )
    .await
    .map_err(|_| BlazeError::BackendError {
        msg: format!("firecracker probe timed out: {}", binary_path.display()),
    })??;
    if !output.status.success() {
        return Err(BlazeError::BackendError {
            msg: format!(
                "firecracker version probe failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    parse_backend_version(&output.stdout)
}

fn parse_backend_version(stdout: &[u8]) -> Result<String> {
    let stdout = std::str::from_utf8(stdout).map_err(|error| BlazeError::BackendError {
        msg: format!("firecracker version probe returned non-UTF-8 output: {error}"),
    })?;
    let mut versions = stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("Firecracker v"));
    let version = versions.next().ok_or_else(|| BlazeError::BackendError {
        msg: "firecracker version probe did not return a Firecracker version line".to_string(),
    })?;
    if versions.next().is_some() {
        return Err(BlazeError::BackendError {
            msg: "firecracker version probe returned multiple Firecracker version lines"
                .to_string(),
        });
    }
    let release = version
        .strip_prefix("Firecracker v")
        .expect("version prefix checked");
    if release.is_empty() || release.chars().any(char::is_whitespace) {
        return Err(BlazeError::BackendError {
            msg: format!("firecracker version probe returned an invalid version line: {version:?}"),
        });
    }
    Ok(version.to_string())
}

fn parse_runtime_version(response: &[u8]) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct VersionResponse {
        firecracker_version: String,
    }

    let response: VersionResponse =
        serde_json::from_slice(response).map_err(|error| BlazeError::BackendError {
            msg: format!("invalid Firecracker /version response: {error}"),
        })?;
    let version = response.firecracker_version.trim();
    if version.is_empty() || version.chars().any(char::is_whitespace) {
        return Err(BlazeError::BackendError {
            msg: "Firecracker /version response has an invalid version".to_string(),
        });
    }
    Ok(format!("Firecracker v{version}"))
}

async fn wait_for_socket(socket: &Path, child: &mut Child, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        if socket.exists() && UnixStream::connect(socket).await.is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "Firecracker exited before API socket {} became ready: {status}",
                    socket.display()
                ),
            });
        }
        if started.elapsed() >= timeout {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "Firecracker API socket {} was not ready within {timeout:?}",
                    socket.display()
                ),
            });
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn remove_if_exists(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_network_metadata(path: &Path, network: &NetworkSlot) -> Result<()> {
    write_network_record(path, network, NetworkProcessState::PreSpawn)
}

fn write_network_record(
    path: &Path,
    network: &NetworkSlot,
    process_state: NetworkProcessState,
) -> Result<()> {
    let parent = path.parent().ok_or_else(|| BlazeError::BackendError {
        msg: format!("network metadata has no parent: {}", path.display()),
    })?;
    let temporary = network_metadata_temp(path);
    (|| -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&NetworkRecord {
            slot: network.slot(),
            owner: network.owner(),
            process_state,
        })
        .map_err(|error| BlazeError::BackendError {
            msg: format!("serialize network metadata: {error}"),
        })?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })()
}

fn read_network_metadata(path: &Path) -> Result<(NetworkSlot, NetworkProcessState)> {
    let record: NetworkRecord = serde_json::from_slice(&std::fs::read(path)?).map_err(|error| {
        BlazeError::BackendError {
            msg: format!("parse network metadata {}: {error}", path.display()),
        }
    })?;
    Ok((
        NetworkSlot::from_record(record.slot, record.owner)?,
        record.process_state,
    ))
}

fn network_metadata_temp(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn validate_regular_file(path: &Path, label: &str) -> Result<()> {
    if !path.is_file() {
        return Err(BlazeError::BackendError {
            msg: format!("{label} not found at {}", path.display()),
        });
    }
    Ok(())
}

fn executable_in_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| is_executable_file(&directory.join(name)))
}

fn firecracker_launch_tools_available(is_available: impl FnMut(&str) -> bool) -> bool {
    FIRECRACKER_LAUNCH_TOOLS.into_iter().all(is_available)
}

fn is_executable_file(candidate: &Path) -> bool {
    if !candidate.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(candidate)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn rotate_serial_log_if_needed(path: &Path) -> Result<()> {
    const MAX_SERIAL_LOG_BYTES: u64 = 16 * 1024 * 1024;
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() <= MAX_SERIAL_LOG_BYTES {
        return Ok(());
    }
    let backup = path.with_extension("log.1");
    match std::fs::remove_file(&backup) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::rename(path, backup)?;
    Ok(())
}

fn path_string<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| BlazeError::BackendError {
        msg: format!("{label} path is not valid UTF-8: {}", path.display()),
    })
}

fn backend_protocol_error(error: hyper::Error) -> BlazeError {
    BlazeError::BackendError {
        msg: format!("Firecracker API protocol error: {error}"),
    }
}

async fn cleanup_orphan_run_dir_with(
    instance_id: Uuid,
    run_dir: &Path,
    network_manager: &NetworkManager,
) -> Result<()> {
    let stopped_marker = stopped_marker(run_dir);
    let pid_file = run_dir.join("firecracker.pid");
    let network_file = run_dir.join("network.json");
    let network_temp_file = network_metadata_temp(&network_file);
    let record_path = if network_file.is_file() {
        Some(network_file.as_path())
    } else if network_temp_file.is_file() {
        Some(network_temp_file.as_path())
    } else {
        None
    };
    let network_record = match record_path {
        Some(path) => match read_network_metadata(path) {
            Ok((network, state)) => {
                if network.owner() != instance_id {
                    return Err(BlazeError::BackendError {
                        msg: format!(
                            "network record owner {} does not match instance {instance_id}",
                            network.owner()
                        ),
                    });
                }
                Some((network, Some(state)))
            }
            Err(error) if path == network_temp_file.as_path() && !network_file.exists() => {
                match network_manager.find_by_owner(instance_id).await? {
                    // The namespace name proves ownership, but it cannot prove
                    // whether the backend crossed the spawn boundary.
                    Some(network) => Some((network, None)),
                    None => return Err(error),
                }
            }
            Err(error) => return Err(error),
        },
        None => network_manager
            .find_by_owner(instance_id)
            .await?
            .map(|network| (network, None)),
    };
    let process_may_exist = pid_file.exists()
        || network_record
            .as_ref()
            .is_none_or(|(_, state)| *state != Some(NetworkProcessState::PreSpawn));
    if !stopped_marker.is_file() {
        #[cfg(target_os = "linux")]
        {
            if process_may_exist {
                terminate_recorded_process(instance_id, &pid_file, "firecracker").await?;
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = instance_id;
            if process_may_exist {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "cannot validate Firecracker orphan {} outside Linux",
                        pid_file.display()
                    ),
                });
            }
        }
        record_backend_stopped(&stopped_marker).await?;
    }

    // Startup reclamation also owns any scratch a previous capture retained.
    cleanup_snapshot_scratch(run_dir)?;

    if let Some((network, _)) = network_record {
        network_manager.destroy(&network).await?;
        remove_if_exists(&network_file).await?;
    }
    remove_if_exists(&network_temp_file).await?;
    remove_if_exists(&run_dir.join("api.sock")).await?;
    remove_if_exists(&run_dir.join("vsock.uds")).await?;
    remove_if_exists(&pid_file).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    #[test]
    fn an_owner_without_a_version_keeps_running_but_refuses_capture() {
        // A start whose VM answers on its API socket but not for its version stays
        // usable and only loses checkpoint capture. This pins the contract the
        // degrade relies on: no capture context means no advertised capability and
        // no reported version, so a later capture is refused by the generic layer
        // before anything is paused — rather than the sandbox failing to be created.
        let temp = tempfile::tempdir().expect("temp");
        let instance_id = Uuid::new_v4();
        let instance = FirecrackerInstance::new(
            instance_id,
            None,
            None,
            runtime_files(
                temp.path().join("api.sock"),
                temp.path().join("vsock.uds"),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                temp.path().join("network.json"),
            ),
            None,
            Arc::new(NetworkManager::default()),
            false,
        );

        assert_eq!(instance.instance_id(), instance_id);
        assert_eq!(
            instance.version(),
            None,
            "an owner without a capture context reports no version"
        );
        assert!(
            !instance.supports_checkpoint_capture(),
            "capture must not be advertised without a confirmed version"
        );
    }

    #[test]
    fn restored_owner_is_sized_by_the_captured_image() {
        // A restore writes no VM configuration, so a restored owner that read its
        // memory size from the reconstructed configuration would freeze the
        // default. A later capture of a restored large guest would then get the
        // minimum deadline and time out with an unknown outcome.
        let temp = tempfile::tempdir().expect("temp");
        let memory = temp.path().join("memory.snap");
        let captured = 16 * 1024 * 1024 * 1024_u64;
        // Size the artifact without writing 16 GiB of zeroes.
        let file = std::fs::File::create(&memory).expect("create memory artifact");
        file.set_len(captured).expect("size memory artifact");
        drop(file);

        let from_artifact = std::fs::metadata(&memory).expect("stat artifact").len();
        assert_eq!(from_artifact, captured);
        assert!(
            snapshot_timeout(from_artifact)
                > snapshot_timeout(
                    FirecrackerConfig::default()
                        .memory
                        .as_deref()
                        .and_then(|value| parse_memory_value(value).ok())
                        .unwrap_or(256)
                        * 1024
                        * 1024
                ),
            "a restored large guest must not inherit the default's deadline"
        );
    }

    #[test]
    fn snapshot_deadline_scales_with_guest_memory() {
        // Guest memory has no configured upper bound, so a fixed deadline cannot
        // hold: at the measured rate a 16 GiB guest already outlives a
        // 15-minute bound, and a timeout is reported as an unknown outcome that
        // fails the checkpoint while Firecracker may still be writing.
        let gib = 1024 * 1024 * 1024_u64;
        let small = snapshot_timeout(512 * 1024 * 1024);
        let large = snapshot_timeout(16 * gib);
        let huge = snapshot_timeout(128 * gib);

        assert!(
            large > Duration::from_secs(15 * 60),
            "a 16 GiB guest must outlive a fixed 15-minute bound, got {large:?}"
        );
        assert!(large < huge, "the deadline must keep scaling with memory");
        assert!(
            small >= SNAPSHOT_TIMEOUT_FLOOR,
            "small guests keep a floor for fixed request and fsync overhead"
        );
        assert_eq!(
            snapshot_timeout(0),
            SNAPSHOT_TIMEOUT_FLOOR,
            "an unknown size still gets the floor rather than no time at all"
        );
    }

    #[cfg(target_os = "linux")]
    use std::convert::Infallible;

    use blaze_core::storage::StorageSlot;
    #[cfg(target_os = "linux")]
    use hyper::Response;
    #[cfg(target_os = "linux")]
    use hyper::server::conn::http1 as server_http1;
    #[cfg(target_os = "linux")]
    use hyper::service::service_fn;
    #[cfg(target_os = "linux")]
    use tokio::net::UnixListener;
    #[cfg(target_os = "linux")]
    use tokio::sync::oneshot;

    use crate::spawner::netns::{IpCommandRunner, IpOutput, NetworkManager, test_network_slot};

    use super::*;

    fn full_restore(expose_guest_socket: bool) -> FirecrackerRestore {
        FirecrackerRestore {
            vm_state: PathBuf::from("/checkpoint/vmstate.snap"),
            memory: PathBuf::from("/checkpoint/memory.snap"),
            expected_version: Some("Firecracker v1.16.0".to_string()),
            checkpoint_backend: BackendKind::Firecracker,
            snapshot_kind: SnapshotKind::Full,
            expose_guest_socket,
        }
    }

    #[test]
    fn snapshot_load_rebinds_host_resources_created_by_this_start() {
        let payload = snapshot_load_payload(
            &full_restore(true),
            Some("tap0"),
            Some(Path::new("/run/blaze/instance/vsock.uds")),
        )
        .expect("load payload");

        assert_eq!(payload["snapshot_path"], "/checkpoint/vmstate.snap");
        assert_eq!(
            payload["mem_backend"]["backend_path"],
            "/checkpoint/memory.snap"
        );
        assert_eq!(payload["mem_backend"]["backend_type"], "File");
        assert_eq!(payload["track_dirty_pages"], false);
        assert_eq!(payload["resume_vm"], true);
        // The tap and guest socket are recreated per start, so the load must
        // override whatever names the snapshot recorded.
        assert_eq!(payload["network_overrides"][0]["iface_id"], "eth0");
        assert_eq!(payload["network_overrides"][0]["host_dev_name"], "tap0");
        assert_eq!(
            payload["vsock_override"]["uds_path"],
            "/run/blaze/instance/vsock.uds"
        );
    }

    #[test]
    fn snapshot_load_omits_overrides_the_sandbox_does_not_own() {
        let payload =
            snapshot_load_payload(&full_restore(false), None, None).expect("load payload");

        assert!(payload.get("network_overrides").is_none());
        assert!(payload.get("vsock_override").is_none());
        assert_eq!(payload["resume_vm"], true);
    }

    #[test]
    fn console_log_survives_a_restore_into_the_same_runtime_directory() {
        // A restore reuses the runtime directory of the sandbox it replaces, so
        // reopening the console log must not discard what was captured before.
        let temp = tempfile::tempdir().expect("temp");
        let serial_log = temp.path().join("serial.log");
        std::fs::write(&serial_log, b"pre-restore console output\n").expect("seed log");
        let stderr_log = temp.path().join("stderr.log");
        std::fs::write(&stderr_log, b"pre-restore vmm diagnostics\n").expect("seed stderr");

        let mut command = Command::new("true");
        configure_logs(&mut command, temp.path(), true).expect("configure logs");
        drop(command);

        let retained = std::fs::read(&serial_log).expect("read log");
        assert_eq!(
            retained, b"pre-restore console output\n",
            "reopening the console log must preserve pre-restore history"
        );
        let retained_stderr = std::fs::read(&stderr_log).expect("read stderr");
        assert_eq!(
            retained_stderr, b"pre-restore vmm diagnostics\n",
            "reopening the VMM diagnostics must preserve pre-restore history"
        );
    }

    #[test]
    fn console_log_is_left_alone_when_recording_is_disabled() {
        let temp = tempfile::tempdir().expect("temp");
        std::fs::write(
            temp.path().join("stderr.log"),
            b"pre-restore vmm diagnostics\n",
        )
        .expect("seed stderr");
        let mut command = Command::new("true");
        configure_logs(&mut command, temp.path(), false).expect("configure logs");
        drop(command);

        assert!(
            !temp.path().join("serial.log").exists(),
            "a sandbox without console recording must not create the log"
        );
        assert_eq!(
            std::fs::read(temp.path().join("stderr.log")).expect("read stderr"),
            b"pre-restore vmm diagnostics\n",
            "VMM diagnostics are kept regardless of console recording"
        );
    }

    #[test]
    fn restore_config_preserves_the_captured_host_shape() {
        // Regression: reconstructing the spawn config from
        // `FirecrackerConfig::default()` silently dropped every policy-driven
        // field. Networking was the severe case — the load then referenced a tap
        // the previous owner's cleanup had removed, after the running VM was
        // already stopped — and console logging silently stopped being recorded.
        //
        // A restore consumes exactly these three fields; `boot_args`, `vcpus`
        // and `memory` only feed `write_vm_config`, which a restore skips.
        for expose_guest_socket in [false, true] {
            for preserve_network in [false, true] {
                for record_console_log in [false, true] {
                    let config = FirecrackerConfig {
                        enable_vsock: expose_guest_socket,
                        enable_network: preserve_network,
                        serial_log: record_console_log,
                        ..FirecrackerConfig::default()
                    };
                    assert_eq!(config.enable_vsock, expose_guest_socket);
                    assert_eq!(config.enable_network, preserve_network);
                    assert_eq!(config.serial_log, record_console_log);

                    // The reconstructed shape must also satisfy the
                    // compatibility check that runs before anything is stopped.
                    let restore = FirecrackerRestore {
                        expose_guest_socket,
                        ..full_restore(expose_guest_socket)
                    };
                    validate_restore_compatibility(&restore, "Firecracker v1.16.0", &config)
                        .expect("a faithfully reconstructed shape must be accepted");
                }
            }
        }
    }

    #[test]
    fn restore_requires_the_exact_captured_version() {
        let restore = full_restore(false);
        let policy = FirecrackerConfig::default();
        validate_restore_compatibility(&restore, "Firecracker v1.16.0", &policy)
            .expect("matching version restores");

        let error = validate_restore_compatibility(&restore, "Firecracker v1.15.0", &policy)
            .expect_err("a different build must be refused");
        assert!(
            error
                .to_string()
                .contains("does not match executable version"),
            "unexpected error: {error}"
        );

        let unversioned = FirecrackerRestore {
            expected_version: None,
            ..full_restore(false)
        };
        let error = validate_restore_compatibility(&unversioned, "Firecracker v1.16.0", &policy)
            .expect_err("a record without a version cannot be loaded safely");
        assert!(
            error.to_string().contains("records no backend version"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn restore_refuses_a_checkpoint_another_backend_captured() {
        let restore = FirecrackerRestore {
            checkpoint_backend: BackendKind::Mock,
            ..full_restore(false)
        };
        let error = validate_restore_compatibility(
            &restore,
            "Firecracker v1.16.0",
            &FirecrackerConfig::default(),
        )
        .expect_err("foreign checkpoints must be refused");
        assert!(
            error
                .to_string()
                .contains("cannot be restored by Firecracker"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn restore_requires_the_captured_guest_transport_shape() {
        // The snapshot references its vsock device, so restoring it under a
        // policy without the guest transport would produce a VM whose device
        // does not exist.
        let error = validate_restore_compatibility(
            &full_restore(true),
            "Firecracker v1.16.0",
            &FirecrackerConfig::default(),
        )
        .expect_err("a missing guest transport must be refused");
        assert!(
            error.to_string().contains("guest transport does not match"),
            "unexpected error: {error}"
        );

        let enabled = FirecrackerConfig {
            enable_vsock: true,
            ..FirecrackerConfig::default()
        };
        validate_restore_compatibility(&full_restore(true), "Firecracker v1.16.0", &enabled)
            .expect("matching guest transport restores");
    }

    #[test]
    fn version_parser_discards_non_version_log_lines() {
        let stdout = b"Firecracker v1.16.0\n\n\
            2026-07-24T21:55:14Z [anonymous-instance:main] \
            Firecracker exiting successfully. exit_code=0\n";
        assert_eq!(
            parse_backend_version(stdout).expect("version"),
            "Firecracker v1.16.0"
        );
    }

    #[test]
    fn version_parser_rejects_missing_or_ambiguous_version() {
        assert!(parse_backend_version(b"Firecracker exiting successfully\n").is_err());
        assert!(parse_backend_version(b"Firecracker v1.15.0\nFirecracker v1.16.0\n").is_err());
    }

    #[test]
    fn launch_command_uses_the_sandbox_uuid_as_the_backend_id() {
        let instance_id = Uuid::new_v4();
        let command = build_launch_command(
            Path::new("/usr/bin/firecracker"),
            None,
            Path::new("/proc/self/fd/17/api.sock"),
            instance_id,
            Path::new("/var/lib/blaze/instances/owner/rootfs.ext4"),
        );
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let expected = format!("fc-{instance_id}");

        assert!(
            arguments.contains(&expected),
            "backend id missing from {arguments:?}"
        );
    }

    /// The launch must bind this sandbox's own rootfs onto the shared path the
    /// recorded machine configuration names, or a restored snapshot would read
    /// the capture-time disk instead of this sandbox's copy.
    #[cfg(target_os = "linux")]
    #[test]
    fn launch_command_binds_the_owned_rootfs_to_the_portable_path() {
        let owned = Path::new("/var/lib/blaze/instances/owner/rootfs.ext4");
        let command = build_launch_command(
            Path::new("/usr/bin/firecracker"),
            None,
            Path::new("/proc/self/fd/17/api.sock"),
            Uuid::new_v4(),
            owned,
        );
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(
            arguments.iter().any(|argument| argument
                == "mount --bind \"$rootfs_source\" \"$rootfs_target\""
                || argument.contains("mount --bind")),
            "bind-mount step missing from {arguments:?}"
        );
        assert!(
            arguments
                .iter()
                .any(|argument| argument == owned.to_string_lossy().as_ref()),
            "owned rootfs source missing from {arguments:?}"
        );
        assert!(
            arguments
                .iter()
                .any(|argument| argument == PORTABLE_ROOTFS_PATH),
            "portable rootfs target missing from {arguments:?}"
        );
    }

    #[test]
    fn vm_config_omits_network_until_the_network_capability_is_enabled() {
        let temp = tempfile::tempdir().expect("temp");
        let request = spawn_request(temp.path());

        let path = write_vm_config(
            &temp.path().join("images"),
            &request,
            &FirecrackerConfig::default(),
            &temp.path().join("guest.sock"),
            None,
        )
        .expect("write config");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("read config"))
                .expect("parse config");
        assert!(value.get("network-interfaces").is_none());
    }

    #[test]
    fn vm_config_and_reported_guest_transport_agree() {
        let temp = tempfile::tempdir().expect("temp");
        let request = spawn_request(temp.path());
        let socket = temp.path().join("vsock.uds");
        let disabled = FirecrackerConfig::default();
        let disabled_path = write_vm_config(
            &temp.path().join("images"),
            &request,
            &disabled,
            &socket,
            None,
        )
        .expect("disabled config");
        let disabled_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(disabled_path).expect("read disabled config"))
                .expect("parse disabled config");
        assert!(disabled_value.get("vsock").is_none());
        let files = configured_runtime_files(
            runtime_files(
                temp.path().join("api.sock"),
                socket.clone(),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                temp.path().join("network.json"),
            ),
            disabled.enable_vsock,
        );
        assert!(files.guest_socket.as_os_str().is_empty());

        let enabled = FirecrackerConfig {
            enable_vsock: true,
            ..FirecrackerConfig::default()
        };
        let enabled_path = write_vm_config(
            &temp.path().join("images"),
            &request,
            &enabled,
            &socket,
            None,
        )
        .expect("enabled config");
        let enabled_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(enabled_path).expect("read enabled config"))
                .expect("parse enabled config");
        assert_eq!(
            enabled_value["vsock"]["uds_path"],
            path_string(&socket, "socket").expect("socket path")
        );
        let files = configured_runtime_files(
            runtime_files(
                temp.path().join("api.sock"),
                socket.clone(),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                temp.path().join("network.json"),
            ),
            enabled.enable_vsock,
        );
        assert_eq!(files.guest_socket, socket);
    }

    #[test]
    fn vm_config_wires_an_allocated_network_slot() {
        let temp = tempfile::tempdir().expect("temp");
        let request = spawn_request(temp.path());
        let network = test_network_slot(0);

        let path = write_vm_config(
            &temp.path().join("images"),
            &request,
            &FirecrackerConfig::default(),
            &temp.path().join("guest.sock"),
            Some(&network),
        )
        .expect("write config");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("read config"))
                .expect("parse config");
        assert_eq!(value["network-interfaces"][0]["iface_id"], "eth0");
        assert_eq!(value["network-interfaces"][0]["host_dev_name"], "tap0");
        assert!(
            value["boot-source"]["boot_args"]
                .as_str()
                .expect("boot args")
                .contains("::eth0:off")
        );
    }

    #[test]
    fn effective_boot_arguments_include_the_cold_start_network_argument() {
        let config = FirecrackerConfig {
            enable_network: true,
            ..FirecrackerConfig::default()
        };

        assert_eq!(
            effective_boot_args(&config, config.enable_network)
                .expect("effective network command line"),
            format!("{} {NETWORK_BOOT_IP}", config.boot_args)
        );
        assert_eq!(
            effective_boot_args(&config, false).expect("non-network command line"),
            config.boot_args
        );
    }

    #[test]
    fn vm_config_accepts_the_matching_network_boot_argument() {
        let temp = tempfile::tempdir().expect("temp");
        let request = spawn_request(temp.path());
        let network = test_network_slot(0);
        let config = FirecrackerConfig {
            boot_args: format!("console=ttyS0 {NETWORK_BOOT_IP}"),
            ..FirecrackerConfig::default()
        };

        write_vm_config(
            &temp.path().join("images"),
            &request,
            &config,
            &temp.path().join("guest.sock"),
            Some(&network),
        )
        .expect("matching network boot argument");
    }

    #[test]
    fn vm_config_rejects_an_incompatible_network_boot_argument() {
        let temp = tempfile::tempdir().expect("temp");
        let request = spawn_request(temp.path());
        let network = test_network_slot(0);
        let config = FirecrackerConfig {
            boot_args: "console=ttyS0 ip=dhcp".to_string(),
            ..FirecrackerConfig::default()
        };

        let error = write_vm_config(
            &temp.path().join("images"),
            &request,
            &config,
            &temp.path().join("guest.sock"),
            Some(&network),
        )
        .expect_err("incompatible network boot argument");

        assert!(error.to_string().contains("requires"));
        assert!(error.to_string().contains("ip=dhcp"));
    }

    #[test]
    fn vm_config_rejects_conflicting_network_boot_arguments() {
        let temp = tempfile::tempdir().expect("temp");
        let request = spawn_request(temp.path());
        let network = test_network_slot(0);
        let config = FirecrackerConfig {
            boot_args: format!("console=ttyS0 {NETWORK_BOOT_IP} ip=dhcp"),
            ..FirecrackerConfig::default()
        };

        let error = write_vm_config(
            &temp.path().join("images"),
            &request,
            &config,
            &temp.path().join("guest.sock"),
            Some(&network),
        )
        .expect_err("conflicting network boot arguments");

        assert!(error.to_string().contains("exactly"));
        assert!(error.to_string().contains("ip=dhcp"));
    }

    #[test]
    fn network_metadata_is_published_atomically() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("network.json");
        let slot = test_network_slot(7);

        write_network_metadata(&path, &slot).expect("write metadata");

        let (stored, state) = read_network_metadata(&path).expect("parse metadata");
        assert_eq!(stored, slot);
        assert_eq!(state, NetworkProcessState::PreSpawn);
        assert!(!network_metadata_temp(&path).exists());
    }

    #[test]
    fn network_metadata_records_launch_intent_before_spawn() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("network.json");
        let slot = test_network_slot(7);
        write_network_metadata(&path, &slot).expect("write pre-spawn metadata");

        write_network_record(&path, &slot, NetworkProcessState::Launching)
            .expect("record launch intent");

        let (stored, state) = read_network_metadata(&path).expect("parse metadata");
        assert_eq!(stored, slot);
        assert_eq!(state, NetworkProcessState::Launching);
        assert!(!network_metadata_temp(&path).exists());
    }

    #[test]
    fn network_metadata_rejects_out_of_range_slots() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("network.json");
        std::fs::write(
            &path,
            br#"{"slot":16383,"owner":"00000000-0000-0000-0000-000000000001"}"#,
        )
        .expect("metadata");

        let error = read_network_metadata(&path).expect_err("invalid slot");

        assert!(error.to_string().contains("outside"));
    }

    #[tokio::test]
    async fn network_cleanup_failure_retains_a_retryable_backend_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let network_file = temp.path().join("network.json");
        let slot = test_network_slot(0);
        write_network_metadata(&network_file, &slot).expect("network metadata");
        let namespace = format!("{}\n", slot.netns());
        let runner = Arc::new(TestIpRunner::with_responses([
            ip_success(namespace.as_bytes()),
            ip_failure("delete peer failed"),
            ip_success(namespace.as_bytes()),
            ip_success(b""),
            ip_success(b""),
        ]));
        let network_manager = Arc::new(NetworkManager::with_runner(runner.clone()));
        let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
            slot.owner(),
            None,
            None,
            runtime_files(
                temp.path().join("api.sock"),
                temp.path().join("guest.sock"),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                network_file.clone(),
            ),
            Some(slot.clone()),
            network_manager,
            false,
        ));

        owner.kill().await.expect_err("first cleanup must fail");
        assert!(network_file.exists());
        owner.kill().await.expect("retry cleanup");
        assert!(!network_file.exists());
        assert!(
            runner
                .calls()
                .iter()
                .any(|args| args == &["netns", "del", slot.netns()])
        );
        assert!(
            !runner
                .calls()
                .iter()
                .any(|args| args == &["link", "del", "blz-veth-0"])
        );
    }

    #[tokio::test]
    async fn try_wait_retries_cleanup_after_observing_process_exit() {
        let temp = tempfile::tempdir().expect("temp");
        let network_file = temp.path().join("network.json");
        let slot = test_network_slot(0);
        write_network_metadata(&network_file, &slot).expect("network metadata");
        let namespace = format!("{}\n", slot.netns());
        let runner = Arc::new(TestIpRunner::with_responses([
            ip_success(namespace.as_bytes()),
            ip_failure("delete peer failed"),
            ip_success(namespace.as_bytes()),
            ip_success(b""),
            ip_success(b""),
        ]));
        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 7")
            .spawn()
            .expect("spawn child");
        let instance = FirecrackerInstance::new(
            slot.owner(),
            Some(child),
            None,
            runtime_files(
                temp.path().join("api.sock"),
                temp.path().join("guest.sock"),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                network_file.clone(),
            ),
            Some(slot.clone()),
            Arc::new(NetworkManager::with_runner(runner.clone())),
            false,
        );

        let first_error = loop {
            match instance.try_wait().await {
                Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                Ok(Some(result)) => {
                    panic!("cleanup failure must not report completion: {result:?}")
                }
                Err(error) => break error,
            }
        };
        assert!(first_error.to_string().contains("delete peer failed"));
        assert!(network_file.exists());

        let result = instance
            .try_wait()
            .await
            .expect("retry cleanup")
            .expect("completed process");
        assert_eq!(result.exit_code, Some(7));
        assert!(!network_file.exists());
        assert!(
            runner
                .calls()
                .iter()
                .any(|args| args == &["netns", "del", slot.netns()])
        );
    }

    #[tokio::test]
    async fn stopped_orphan_still_releases_recorded_network() {
        let temp = tempfile::tempdir().expect("temp");
        record_backend_stopped(&stopped_marker(temp.path()))
            .await
            .expect("stopped marker");
        let network_file = temp.path().join("network.json");
        let network = test_network_slot(0);
        write_network_metadata(&network_file, &network).expect("network metadata");
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(TestIpRunner::with_responses([
            ip_success(namespace.as_bytes()),
            ip_success(b""),
            ip_success(b""),
        ]));
        let network_manager = NetworkManager::with_runner(runner.clone());

        cleanup_orphan_run_dir_with(network.owner(), temp.path(), &network_manager)
            .await
            .expect("orphan cleanup");

        assert!(!network_file.exists());
        let calls = runner.calls();
        assert!(calls.iter().any(|args| {
            args == &[
                "netns",
                "exec",
                network.netns(),
                "ip",
                "link",
                "del",
                "blz-vpeer-0",
            ]
        }));
        assert!(
            calls
                .iter()
                .any(|args| args == &["netns", "del", network.netns()])
        );
    }

    #[tokio::test]
    async fn orphan_cleanup_recovers_a_complete_temporary_network_record() {
        let temp = tempfile::tempdir().expect("temp");
        record_backend_stopped(&stopped_marker(temp.path()))
            .await
            .expect("stopped marker");
        let network_file = temp.path().join("network.json");
        let network_temp_file = network_metadata_temp(&network_file);
        let network = test_network_slot(0);
        let bytes = serde_json::to_vec(&NetworkRecord {
            slot: network.slot(),
            owner: network.owner(),
            process_state: NetworkProcessState::PreSpawn,
        })
        .expect("serialize metadata");
        std::fs::write(&network_temp_file, bytes).expect("temporary metadata");
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(TestIpRunner::with_responses([
            ip_success(namespace.as_bytes()),
            ip_success(b""),
            ip_success(b""),
        ]));
        let network_manager = NetworkManager::with_runner(runner.clone());

        cleanup_orphan_run_dir_with(network.owner(), temp.path(), &network_manager)
            .await
            .expect("orphan cleanup");

        assert!(!network_file.exists());
        assert!(!network_temp_file.exists());
        assert!(
            runner
                .calls()
                .iter()
                .any(|args| args == &["netns", "del", network.netns()])
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphan_cleanup_retains_a_truncated_network_record_without_pid_proof() {
        let temp = tempfile::tempdir().expect("temp");
        let network_file = temp.path().join("network.json");
        let network_temp_file = network_metadata_temp(&network_file);
        std::fs::write(&network_temp_file, b"{").expect("truncated metadata");
        let network = test_network_slot(0);
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(TestIpRunner::with_responses([ip_success(
            namespace.as_bytes(),
        )]));
        let network_manager = NetworkManager::with_runner(runner.clone());

        let error = cleanup_orphan_run_dir_with(network.owner(), temp.path(), &network_manager)
            .await
            .expect_err("unknown launch state must fail closed");

        assert!(error.to_string().contains("missing PID handoff"));
        assert!(network_temp_file.exists());
        assert!(!stopped_marker(temp.path()).exists());
        assert_eq!(
            runner.calls(),
            vec![vec!["netns".to_string(), "list".to_string()]]
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphan_cleanup_retains_an_unrecorded_namespace_without_pid_proof() {
        let temp = tempfile::tempdir().expect("temp");
        let network = test_network_slot(0);
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(TestIpRunner::with_responses([ip_success(
            namespace.as_bytes(),
        )]));
        let network_manager = NetworkManager::with_runner(runner.clone());

        let error = cleanup_orphan_run_dir_with(network.owner(), temp.path(), &network_manager)
            .await
            .expect_err("unknown launch state must fail closed");

        assert!(error.to_string().contains("missing PID handoff"));
        assert!(!stopped_marker(temp.path()).exists());
        assert_eq!(
            runner.calls(),
            vec![vec!["netns".to_string(), "list".to_string()]]
        );
    }

    #[tokio::test]
    async fn stopped_orphan_releases_an_unrecorded_owner_namespace() {
        let temp = tempfile::tempdir().expect("temp");
        record_backend_stopped(&stopped_marker(temp.path()))
            .await
            .expect("stopped marker");
        let network = test_network_slot(0);
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(TestIpRunner::with_responses([
            ip_success(namespace.as_bytes()),
            ip_success(namespace.as_bytes()),
            ip_success(b""),
            ip_success(b""),
        ]));
        let network_manager = NetworkManager::with_runner(runner.clone());

        cleanup_orphan_run_dir_with(network.owner(), temp.path(), &network_manager)
            .await
            .expect("stopped process permits network recovery");

        assert!(
            runner
                .calls()
                .iter()
                .any(|args| args == &["netns", "del", network.netns()])
        );
    }

    #[tokio::test]
    async fn network_record_owner_mismatch_issues_no_host_commands() {
        let temp = tempfile::tempdir().expect("temp");
        let network_file = temp.path().join("network.json");
        let network = test_network_slot(0);
        write_network_metadata(&network_file, &network).expect("network metadata");
        let runner = Arc::new(TestIpRunner::default());
        let network_manager = NetworkManager::with_runner(runner.clone());

        let error = cleanup_orphan_run_dir_with(Uuid::from_u128(2), temp.path(), &network_manager)
            .await
            .expect_err("mismatched owner must fail");

        assert!(error.to_string().contains("does not match instance"));
        assert!(network_file.exists());
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn stale_network_record_does_not_delete_a_reused_slot() {
        let temp = tempfile::tempdir().expect("temp");
        record_backend_stopped(&stopped_marker(temp.path()))
            .await
            .expect("stopped marker");
        let network_file = temp.path().join("network.json");
        let old_network = test_network_slot(0);
        write_network_metadata(&network_file, &old_network).expect("network metadata");
        let new_network =
            NetworkSlot::from_record(0, Uuid::from_u128(2)).expect("new network owner");
        let namespace = format!("{}\n", new_network.netns());
        let runner = Arc::new(TestIpRunner::with_responses([ip_success(
            namespace.as_bytes(),
        )]));
        let network_manager = NetworkManager::with_runner(runner.clone());

        cleanup_orphan_run_dir_with(old_network.owner(), temp.path(), &network_manager)
            .await
            .expect("retire stale record");

        assert!(!network_file.exists());
        let calls = runner.calls();
        assert_eq!(calls, vec![vec!["netns".to_string(), "list".to_string()]]);
    }

    #[tokio::test]
    async fn pre_spawn_orphan_releases_network_without_pid_metadata() {
        let temp = tempfile::tempdir().expect("temp");
        let network_file = temp.path().join("network.json");
        let network = test_network_slot(0);
        write_network_metadata(&network_file, &network).expect("network metadata");
        let namespace = format!("{}\n", network.netns());
        let runner = Arc::new(TestIpRunner::with_responses([
            ip_success(namespace.as_bytes()),
            ip_success(b""),
            ip_success(b""),
        ]));
        let network_manager = NetworkManager::with_runner(runner.clone());

        cleanup_orphan_run_dir_with(network.owner(), temp.path(), &network_manager)
            .await
            .expect("pre-spawn cleanup");

        assert!(!network_file.exists());
        assert!(stopped_marker(temp.path()).exists());
        assert!(
            runner
                .calls()
                .iter()
                .any(|args| args == &["netns", "del", network.netns()])
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unconfirmed_process_ownership_retains_network_metadata() {
        let temp = tempfile::tempdir().expect("temp");
        let network_file = temp.path().join("network.json");
        let network = test_network_slot(0);
        write_network_record(&network_file, &network, NetworkProcessState::Launching)
            .expect("network metadata");
        let runner = Arc::new(TestIpRunner::default());
        let network_manager = NetworkManager::with_runner(runner.clone());

        let error = cleanup_orphan_run_dir_with(network.owner(), temp.path(), &network_manager)
            .await
            .expect_err("missing process metadata must block cleanup");

        assert!(error.to_string().contains("missing PID handoff"));
        assert!(network_file.exists());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn serial_log_rotates_before_reuse() {
        let temp = tempfile::tempdir().expect("temp");
        let log = temp.path().join("serial.log");
        let file = std::fs::File::create(&log).expect("create log");
        file.set_len(16 * 1024 * 1024 + 1).expect("grow log");

        rotate_serial_log_if_needed(&log).expect("rotate");

        assert!(!log.exists());
        assert_eq!(
            std::fs::metadata(temp.path().join("serial.log.1"))
                .expect("rotated log")
                .len(),
            16 * 1024 * 1024 + 1
        );
    }

    #[cfg(unix)]
    #[test]
    fn executable_check_requires_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp");
        let tool = temp.path().join("tool");
        std::fs::write(&tool, b"#!/bin/sh\n").expect("write tool");
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644))
            .expect("non-executable permissions");
        assert!(!is_executable_file(&tool));
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755))
            .expect("executable permissions");
        assert!(is_executable_file(&tool));
    }

    #[tokio::test]
    async fn backend_probe_skips_network_checks_when_no_policy_enables_them() {
        let temp = tempfile::tempdir().expect("temp");
        let called = Arc::new(AtomicBool::new(false));
        let network = Arc::new(NetworkManager::with_runner(Arc::new(
            UnavailableNetworkRunner {
                called: called.clone(),
            },
        )));
        let spawner = FirecrackerSpawner {
            images_dir: temp.path().join("images"),
            api_timeout: Duration::from_secs(1),
            socket_timeout: Duration::from_secs(1),
            network,
            network_required: false,
            version: Mutex::new(None),
        };

        assert!(spawner.network_probe_ready().await.expect("probe"));
        assert!(!called.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn start_failure_terminates_child_and_removes_process_metadata() {
        let temp = tempfile::tempdir().expect("temp");
        let pid_file = temp.path().join("firecracker.pid");
        let termination_marker = temp.path().join("terminated");
        let child = Command::new("sh")
            .arg("-c")
            .arg("trap 'printf term > \"$MARKER\"; exit 0' TERM; while :; do sleep 1; done")
            .env("MARKER", &termination_marker)
            .spawn()
            .expect("spawn child");
        std::fs::write(&pid_file, format!("{}\n", child.id().expect("child pid")))
            .expect("pid metadata");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let owner: DynBackendInstance = Arc::new(FirecrackerInstance::new(
            Uuid::new_v4(),
            Some(child),
            None,
            runtime_files(
                temp.path().join("api.sock"),
                temp.path().join("guest.sock"),
                pid_file.clone(),
                stopped_marker(temp.path()),
                temp.path().join("network.json"),
            ),
            None,
            Arc::new(NetworkManager::default()),
            false,
        ));
        let failure = SpawnFailure::compensate_started(
            BlazeError::BackendError {
                msg: "injected start failure".to_string(),
            },
            owner,
        )
        .await;
        let (source, owner) = failure.into_parts();

        assert!(source.to_string().contains("injected start failure"));
        assert!(
            owner.is_none(),
            "successful compensation must drop ownership"
        );
        assert_eq!(
            std::fs::read_to_string(termination_marker).expect("termination marker"),
            "term"
        );
        assert!(!pid_file.exists());
    }

    fn spawn_request(root: &Path) -> BackendSpawnRequest {
        let instance_id = Uuid::new_v4();
        let run_dir = root.join("run");
        let slot_dir = root.join("slot");
        BackendSpawnRequest::new(
            SpawnRequest {
                instance_id,
                binary_path: root.join("firecracker"),
                storage: StorageSlot {
                    id: instance_id.to_string(),
                    rootfs_path: slot_dir.join("rootfs.ext4"),
                    mem_path: slot_dir.join("mem.bin"),
                    mem_diff_path: slot_dir.join("mem.diff"),
                    rootfs_diff_path: slot_dir.join("rootfs.diff"),
                    instance_dir: slot_dir,
                },
                backend: blaze_core::policy::BackendConfigs::default(),
                vm: None,
            },
            OwnedRunDir::for_test(instance_id, run_dir),
        )
        .expect("matching backend request")
    }

    #[derive(Default)]
    struct TestIpRunner {
        responses: std::sync::Mutex<VecDeque<IpOutput>>,
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    struct UnavailableNetworkRunner {
        called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl IpCommandRunner for UnavailableNetworkRunner {
        async fn output(&self, _args: &[String], _timeout: Duration) -> Result<IpOutput> {
            self.called.store(true, Ordering::Release);
            Ok(ip_failure("network commands unavailable"))
        }

        #[cfg(target_os = "linux")]
        fn executable_in_path(&self, _name: &str) -> bool {
            false
        }

        #[cfg(target_os = "linux")]
        fn has_network_admin(&self) -> bool {
            false
        }
    }

    impl TestIpRunner {
        fn with_responses<const N: usize>(responses: [IpOutput; N]) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    #[async_trait]
    impl IpCommandRunner for TestIpRunner {
        async fn output(&self, args: &[String], _timeout: Duration) -> Result<IpOutput> {
            self.calls.lock().expect("calls lock").push(args.to_vec());
            Ok(self
                .responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .unwrap_or_else(|| ip_success(b"")))
        }
    }

    fn ip_success(stdout: &[u8]) -> IpOutput {
        IpOutput {
            success: true,
            status: "exit status: 0".to_string(),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    fn ip_failure(stderr: &str) -> IpOutput {
        IpOutput {
            success: false,
            status: "exit status: 1".to_string(),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[cfg(target_os = "linux")]
    async fn spawn_api(
        socket: &Path,
        call_count: usize,
    ) -> oneshot::Receiver<Vec<(Method, String, serde_json::Value)>> {
        let listener = UnixListener::bind(socket).expect("bind");
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let observed = Arc::new(Mutex::new(Vec::with_capacity(call_count)));
            for _ in 0..call_count {
                let (stream, _) = listener.accept().await.expect("accept");
                let observed = observed.clone();
                let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                    let observed = observed.clone();
                    async move {
                        let method = request.method().clone();
                        let path = request.uri().path().to_string();
                        let body = request
                            .into_body()
                            .collect()
                            .await
                            .expect("request body")
                            .to_bytes();
                        let body = if body.is_empty() {
                            serde_json::Value::Null
                        } else {
                            serde_json::from_slice(&body).expect("request JSON")
                        };
                        if path == "/snapshot/create" {
                            let snapshot = body["snapshot_path"].as_str().expect("snapshot path");
                            let memory = body["mem_file_path"].as_str().expect("memory path");
                            std::fs::write(snapshot, b"vmstate").expect("write snapshot");
                            std::fs::write(memory, b"memory").expect("write memory");
                        }
                        observed.lock().await.push((method, path, body));
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(hyper::StatusCode::NO_CONTENT)
                                .body(Full::new(Bytes::new()))
                                .expect("response"),
                        )
                    }
                });
                server_http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                    .expect("serve");
            }
            let calls = observed.lock().await.clone();
            let _ = tx.send(calls);
        });
        rx
    }

    #[cfg(target_os = "linux")]
    fn spawn_api_response(
        socket: &Path,
        status: hyper::StatusCode,
        body: Vec<u8>,
        delay: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(socket).expect("bind");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let body = Bytes::from(body);
            let service = service_fn(move |_request: Request<hyper::body::Incoming>| {
                let body = body.clone();
                async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(status)
                            .body(Full::new(body))
                            .expect("response"),
                    )
                }
            });
            let _ = server_http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        })
    }

    #[cfg(target_os = "linux")]
    fn spawn_api_disconnect(socket: &Path) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(socket).expect("bind");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let service = service_fn(|request: Request<hyper::body::Incoming>| async move {
                assert_eq!(request.method(), Method::PATCH);
                assert_eq!(request.uri().path(), "/vm");
                Err::<Response<Full<Bytes>>, _>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "close after accepting the request",
                ))
            });
            let _ = server_http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        })
    }

    #[cfg(target_os = "linux")]
    fn spawn_snapshot_api_disconnect(socket: &Path) -> tokio::task::JoinHandle<PathBuf> {
        let listener = UnixListener::bind(socket).expect("bind");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let scratch = Arc::new(Mutex::new(None));
            let observed_scratch = scratch.clone();
            let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                let scratch = scratch.clone();
                async move {
                    assert_eq!(request.method(), Method::PUT);
                    assert_eq!(request.uri().path(), "/snapshot/create");
                    let body = request
                        .into_body()
                        .collect()
                        .await
                        .expect("request body")
                        .to_bytes();
                    let body: serde_json::Value =
                        serde_json::from_slice(&body).expect("request JSON");
                    let snapshot =
                        PathBuf::from(body["snapshot_path"].as_str().expect("snapshot path"));
                    let memory =
                        PathBuf::from(body["mem_file_path"].as_str().expect("memory path"));
                    assert_eq!(snapshot.parent(), memory.parent());
                    std::fs::write(&snapshot, b"vmstate").expect("write snapshot");
                    std::fs::write(&memory, b"memory").expect("write memory");
                    *scratch.lock().await = snapshot.parent().map(Path::to_path_buf);
                    Err::<Response<Full<Bytes>>, _>(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "close after snapshot request",
                    ))
                }
            });
            let _ = server_http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
            observed_scratch
                .lock()
                .await
                .take()
                .expect("observed scratch directory")
        })
    }

    #[cfg(target_os = "linux")]
    fn spawn_snapshot_api_with_unexpected_scratch_entry(
        socket: &Path,
        status: hyper::StatusCode,
    ) -> tokio::task::JoinHandle<PathBuf> {
        let listener = UnixListener::bind(socket).expect("bind");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let scratch = Arc::new(Mutex::new(None));
            let observed_scratch = scratch.clone();
            let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                let scratch = scratch.clone();
                async move {
                    assert_eq!(request.method(), Method::PUT);
                    assert_eq!(request.uri().path(), "/snapshot/create");
                    let body = request
                        .into_body()
                        .collect()
                        .await
                        .expect("request body")
                        .to_bytes();
                    let body: serde_json::Value =
                        serde_json::from_slice(&body).expect("request JSON");
                    let snapshot =
                        PathBuf::from(body["snapshot_path"].as_str().expect("snapshot path"));
                    let memory =
                        PathBuf::from(body["mem_file_path"].as_str().expect("memory path"));
                    let directory = snapshot.parent().expect("snapshot scratch").to_path_buf();
                    assert_eq!(Some(directory.as_path()), memory.parent());
                    if status.is_success() {
                        std::fs::write(&snapshot, b"vmstate").expect("write snapshot");
                        std::fs::write(&memory, b"memory").expect("write memory");
                    }
                    std::fs::write(directory.join("unexpected"), b"retain")
                        .expect("write unexpected scratch entry");
                    *scratch.lock().await = Some(directory);
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(status)
                            .body(Full::new(Bytes::from_static(b"snapshot response")))
                            .expect("response"),
                    )
                }
            });
            server_http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .expect("serve");
            observed_scratch
                .lock()
                .await
                .take()
                .expect("observed scratch directory")
        })
    }

    #[cfg(target_os = "linux")]
    fn write_version_binary(path: &Path, output: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, format!("#!/bin/sh\nprintf '%s\\n' '{output}'\n"))
            .expect("write version binary");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("make version binary executable");
    }

    #[cfg(target_os = "linux")]
    fn capture_instance(temp: &tempfile::TempDir, api_socket: PathBuf) -> FirecrackerInstance {
        let instance_id = Uuid::new_v4();
        let run_dir = OwnedRunDir::for_test(instance_id, temp.path().join("run"));
        FirecrackerInstance::new(
            instance_id,
            None,
            Some(FirecrackerCapture::new(
                api_socket.clone(),
                Duration::from_secs(1),
                "Firecracker v1.16.0".to_string(),
                512 * 1024 * 1024,
            )),
            runtime_files(
                api_socket,
                PathBuf::new(),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                temp.path().join("network.json"),
            ),
            None,
            Arc::new(NetworkManager::default()),
            false,
        )
        .with_run_dir(run_dir)
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn capture_freezes_the_running_api_version_after_binary_probe() {
        let temp = tempfile::tempdir().expect("temp");
        let binary = temp.path().join("firecracker");
        write_version_binary(&binary, "Firecracker v1.15.0");
        let spawner = FirecrackerSpawner::new(temp.path().join("images"));
        assert!(spawner.probe(&binary).await.expect("probe"));

        // Replacing the configured executable after the probe must not change
        // provenance: the running API is the authority for a capture.
        write_version_binary(&binary, "Firecracker v1.17.0");
        let socket = temp.path().join("api.sock");
        let server = spawn_api_response(
            &socket,
            hyper::StatusCode::OK,
            br#"{"firecracker_version":"1.16.0"}"#.to_vec(),
            Duration::ZERO,
        );
        let capture =
            FirecrackerCapture::from_running(socket, Duration::from_secs(1), 512 * 1024 * 1024)
                .await
                .expect("capture API version");
        server.await.expect("server");
        assert_eq!(capture.backend_version, "Firecracker v1.16.0");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn probe_checks_each_requested_binary() {
        let temp = tempfile::tempdir().expect("temp");
        let valid = temp.path().join("valid");
        let invalid = temp.path().join("invalid");
        write_version_binary(&valid, "Firecracker v1.16.0");
        write_version_binary(&invalid, "not a Firecracker version");
        let spawner = FirecrackerSpawner::new(temp.path().join("images"));

        assert!(spawner.probe(&valid).await.expect("valid probe"));
        assert!(!spawner.probe(&invalid).await.expect("invalid probe"));

        write_version_binary(&invalid, "Firecracker v1.17.0");
        assert!(spawner.probe(&invalid).await.expect("replaced probe"));
    }

    #[test]
    fn launch_tool_probe_requires_the_shell_used_by_the_mount_wrapper() {
        let mut checked = Vec::new();

        assert!(!firecracker_launch_tools_available(|tool| {
            checked.push(tool.to_string());
            tool != "sh"
        }));
        assert_eq!(checked, FIRECRACKER_LAUNCH_TOOLS);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn portable_view_target_rejects_existing_non_files() {
        let temp = tempfile::tempdir().expect("temp");
        let directory = temp.path().join("directory");
        std::fs::create_dir(&directory).expect("directory target");

        let directory_error = prepare_portable_view_target_at(&directory)
            .await
            .expect_err("directory target must be rejected");
        assert!(directory_error.to_string().contains("not a regular file"));

        let dangling = temp.path().join("dangling");
        std::os::unix::fs::symlink(temp.path().join("missing"), &dangling)
            .expect("dangling target");
        let symlink_error = prepare_portable_view_target_at(&dangling)
            .await
            .expect_err("symlink target must be rejected");
        assert!(symlink_error.to_string().contains("not a regular file"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn instance_reports_version_and_captures_full_snapshot_over_uds() {
        let temp = tempfile::tempdir().expect("temp");
        let api_socket = temp.path().join("api.sock");
        let observed = spawn_api(&api_socket, 3).await;
        let child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn child");
        let instance_id = Uuid::new_v4();
        let run_dir = OwnedRunDir::for_test(instance_id, temp.path().join("run"));
        let instance = FirecrackerInstance::new(
            instance_id,
            Some(child),
            Some(FirecrackerCapture::new(
                api_socket,
                Duration::from_secs(1),
                "Firecracker v1.16.0".to_string(),
                512 * 1024 * 1024,
            )),
            runtime_files(
                temp.path().join("api.sock"),
                PathBuf::new(),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                temp.path().join("network.json"),
            ),
            None,
            Arc::new(NetworkManager::default()),
            false,
        )
        .with_run_dir(run_dir);
        let target = temp.path().join("checkpoint");
        std::fs::create_dir(&target).expect("checkpoint target");
        // The payload subtree layout is the adapter's own, so the names are
        // spelled out here rather than taken from the request.
        let snapshot_path = target.join("vmstate.snap");
        let mem_path = target.join("memory.snap");

        assert_eq!(instance.instance_id(), instance_id);
        assert_eq!(instance.version(), Some("Firecracker v1.16.0"));
        assert!(instance.supports_checkpoint_capture());
        instance.pause().await.expect("pause");
        instance
            .snapshot(SnapshotRequest {
                payload_dir: target.clone(),
                kind: SnapshotKind::Full,
            })
            .await
            .expect("snapshot");
        instance.resume().await.expect("resume");

        let calls = observed.await.expect("observed calls");
        assert_eq!(
            calls[0],
            (
                Method::PATCH,
                "/vm".to_string(),
                serde_json::json!({"state": "Paused"})
            )
        );
        assert_eq!(calls[1].0, Method::PUT);
        assert_eq!(calls[1].1, "/snapshot/create");
        assert!(
            calls[1].2["snapshot_path"]
                .as_str()
                .expect("path")
                .starts_with("/proc/self/fd/")
        );
        assert!(
            calls[1].2["mem_file_path"]
                .as_str()
                .expect("path")
                .starts_with("/proc/self/fd/")
        );
        assert_eq!(
            calls[2],
            (
                Method::PATCH,
                "/vm".to_string(),
                serde_json::json!({"state": "Resumed"})
            )
        );
        assert_eq!(std::fs::read(&snapshot_path).expect("snapshot"), b"vmstate");
        assert_eq!(std::fs::read(&mem_path).expect("memory"), b"memory");
        instance.kill().await.expect("kill");
    }

    /// The scratch is read back through the run-directory descriptor
    /// Firecracker inherited, so replacing the configured runtime pathname
    /// cannot redirect a transfer that is already in flight.
    ///
    /// The destination is a configured payload pathname rather than a
    /// descriptor name, because a backend adapter may hand the payload
    /// directory to a process that cannot resolve this daemon's
    /// `/proc/self/fd` entries. Integrity does not rest on that path:
    /// publication reopens and hashes the payload subtree through the retained
    /// stage descriptors, so a redirected destination leaves that subtree empty
    /// and fails the capture instead of committing foreign bytes.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn snapshot_transfer_reads_through_the_inherited_run_descriptor() {
        let temp = tempfile::tempdir().expect("temp");
        let id = Uuid::new_v4();
        let configured = temp.path().join("run");
        let run_dir = OwnedRunDir::for_test(id, configured.clone());
        let retained = temp.path().join("retained-run");
        std::fs::rename(&configured, &retained).expect("retain original run directory");
        std::fs::create_dir(&configured).expect("replace configured runtime path");

        let scratch = SnapshotScratch::new(&run_dir).expect("scratch namespace");
        let snapshot_child_path = scratch.snapshot_path.clone();
        let memory_child_path = scratch.memory_path.clone();
        let mut child = Command::new("sh");
        run_dir.inherit_into(&mut child);
        let status = child
            .arg("-c")
            .arg("printf vmstate > \"$1\"; printf memory > \"$2\"")
            .arg("sh")
            .arg(&snapshot_child_path)
            .arg(&memory_child_path)
            .status()
            .await
            .expect("run child");
        assert!(status.success());

        let payload_dir = temp.path().join("payload");
        std::fs::create_dir(&payload_dir).expect("payload subtree");
        let snapshot_target = payload_dir.join(PAYLOAD_VM_STATE_FILE);
        let memory_target = payload_dir.join(PAYLOAD_MEMORY_FILE);
        scratch
            .transfer_into(&snapshot_target, &memory_target)
            .expect("transfer from retained run directory");

        assert_eq!(
            std::fs::read(&snapshot_target).expect("snapshot"),
            b"vmstate"
        );
        assert_eq!(std::fs::read(&memory_target).expect("memory"), b"memory");
        assert!(
            std::fs::read_dir(&configured)
                .expect("replacement")
                .next()
                .is_none()
        );
        assert!(retained.is_dir());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unknown_snapshot_scratch_is_removed_only_after_backend_stop() {
        let temp = tempfile::tempdir().expect("temp");
        let instance_id = Uuid::new_v4();
        let run_dir = OwnedRunDir::for_test(instance_id, temp.path().join("run"));
        let api_socket = temp.path().join("api.sock");
        let server = spawn_snapshot_api_disconnect(&api_socket);
        let child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn child");
        let instance = FirecrackerInstance::new(
            instance_id,
            Some(child),
            Some(FirecrackerCapture::new(
                api_socket.clone(),
                Duration::from_secs(1),
                "Firecracker v1.16.0".to_string(),
                512 * 1024 * 1024,
            )),
            runtime_files(
                api_socket,
                PathBuf::new(),
                temp.path().join("firecracker.pid"),
                stopped_marker(temp.path()),
                temp.path().join("network.json"),
            ),
            None,
            Arc::new(NetworkManager::default()),
            false,
        )
        .with_run_dir(run_dir);
        let target = temp.path().join("checkpoint");
        std::fs::create_dir(&target).expect("checkpoint target");

        let error = instance
            .snapshot(SnapshotRequest {
                payload_dir: target.clone(),
                kind: SnapshotKind::Full,
            })
            .await
            .expect_err("response loss makes delivery unknown");
        let scratch = server.await.expect("server");

        assert!(
            error.to_string().contains("unknown"),
            "unknown outcomes must say so: {error}"
        );
        assert_eq!(
            std::fs::read(scratch.join("vmstate.snap")).expect("snapshot"),
            b"vmstate"
        );
        assert_eq!(
            std::fs::read(scratch.join("memory.snap")).expect("memory"),
            b"memory"
        );
        assert!(!target.join("vmstate.snap").exists());
        assert!(!target.join("memory.snap").exists());

        instance
            .kill()
            .await
            .expect("stop backend and clean scratch");
        assert!(!scratch.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn rejected_snapshot_with_uncleanable_scratch_is_unknown() {
        let temp = tempfile::tempdir().expect("temp");
        let api_socket = temp.path().join("api.sock");
        let server = spawn_snapshot_api_with_unexpected_scratch_entry(
            &api_socket,
            hyper::StatusCode::BAD_REQUEST,
        );
        let instance = capture_instance(&temp, api_socket);
        let target = temp.path().join("checkpoint");
        std::fs::create_dir(&target).expect("checkpoint target");

        let error = instance
            .snapshot(SnapshotRequest {
                payload_dir: target.clone(),
                kind: SnapshotKind::Full,
            })
            .await
            .expect_err("scratch cleanup failure must be unsafe to compensate");
        let scratch = server.await.expect("server");

        assert!(
            error.to_string().contains("unknown"),
            "unknown outcomes must say so: {error}"
        );
        let message = error.to_string();
        assert!(message.contains("400 Bad Request"));
        assert!(message.contains("could not clean its checkpoint scratch"));
        assert!(scratch.join("unexpected").is_file());
        assert!(!target.join("vmstate.snap").exists());
        assert!(!target.join("memory.snap").exists());

        std::fs::remove_file(scratch.join("unexpected")).expect("remove unexpected entry");
        instance.kill().await.expect("clean retained scratch");
        assert!(!scratch.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn completed_transfer_with_uncleanable_scratch_is_unknown() {
        let temp = tempfile::tempdir().expect("temp");
        let api_socket = temp.path().join("api.sock");
        let server =
            spawn_snapshot_api_with_unexpected_scratch_entry(&api_socket, hyper::StatusCode::OK);
        let instance = capture_instance(&temp, api_socket);
        let target = temp.path().join("checkpoint");
        std::fs::create_dir(&target).expect("checkpoint target");

        let error = instance
            .snapshot(SnapshotRequest {
                payload_dir: target.clone(),
                kind: SnapshotKind::Full,
            })
            .await
            .expect_err("completed transfer cannot hide scratch cleanup failure");
        let scratch = server.await.expect("server");

        assert!(
            error.to_string().contains("unknown"),
            "unknown outcomes must say so: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("completed snapshot transfer could not clean its checkpoint scratch")
        );
        assert_eq!(
            std::fs::read(target.join("vmstate.snap")).expect("snapshot target"),
            b"vmstate"
        );
        assert_eq!(
            std::fs::read(target.join("memory.snap")).expect("memory target"),
            b"memory"
        );
        assert!(scratch.join("vmstate.snap").is_file());
        assert!(scratch.join("memory.snap").is_file());
        assert!(scratch.join("unexpected").is_file());

        std::fs::remove_file(scratch.join("unexpected")).expect("remove unexpected entry");
        instance.kill().await.expect("clean retained scratch");
        assert!(!scratch.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn api_client_reports_non_success_response_body() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("api.sock");
        let server = spawn_api_response(
            &socket,
            hyper::StatusCode::BAD_REQUEST,
            b"invalid VM state".to_vec(),
            Duration::ZERO,
        );
        let client = FirecrackerApiClient::new(socket, Duration::from_secs(1));

        let error = client
            .call_json(Method::PATCH, "/vm", None)
            .await
            .expect_err("non-success response");
        server.await.expect("server");

        assert!(matches!(&error, FirecrackerApiError::Known(_)));
        let message = error.into_error().to_string();
        assert!(message.contains("400 Bad Request"));
        assert!(message.contains("invalid VM state"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn api_client_classifies_disconnect_after_delivery_as_unknown() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("api.sock");
        let server = spawn_api_disconnect(&socket);
        let client = FirecrackerApiClient::new(socket, Duration::from_secs(1));

        let error = client
            .call_json(
                Method::PATCH,
                "/vm",
                Some(serde_json::json!({"state": "Paused"})),
            )
            .await
            .expect_err("disconnect after delivery");
        server.await.expect("server");

        assert!(matches!(error, FirecrackerApiError::Unknown(_)));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn api_client_rejects_an_oversized_response() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("api.sock");
        let server = spawn_api_response(
            &socket,
            hyper::StatusCode::OK,
            vec![b'x'; MAX_API_RESPONSE_BYTES + 1],
            Duration::ZERO,
        );
        let client = FirecrackerApiClient::new(socket, Duration::from_secs(1));

        let error = client
            .call_json(Method::GET, "/vm", None)
            .await
            .expect_err("oversized response");
        server.await.expect("server");

        assert!(matches!(&error, FirecrackerApiError::Unknown(_)));
        assert!(
            error
                .into_error()
                .to_string()
                .contains("response exceeded 65536 bytes")
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn api_client_times_out_a_stalled_response() {
        let temp = tempfile::tempdir().expect("temp");
        let socket = temp.path().join("api.sock");
        let server = spawn_api_response(
            &socket,
            hyper::StatusCode::OK,
            Vec::new(),
            Duration::from_millis(200),
        );
        let client = FirecrackerApiClient::new(socket, Duration::from_millis(20));

        let error = client
            .call_json(Method::GET, "/vm", None)
            .await
            .expect_err("stalled response");
        server.await.expect("server");

        assert!(matches!(&error, FirecrackerApiError::Unknown(_)));
        assert!(
            error
                .into_error()
                .to_string()
                .contains("timed out after 20ms")
        );
    }
}
