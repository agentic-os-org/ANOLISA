// SPDX-License-Identifier: Apache-2.0
//! Backend process ownership and runtime lifecycle abstraction.

pub mod firecracker;
mod netns;

use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use async_trait::async_trait;
use blaze_core::backend::{
    BackendKind, RestoreCapability, RestoreRequest, SnapshotRequest, SpawnRequest,
};
#[cfg(test)]
use blaze_core::guest_protocol::DEFAULT_MAX_RESPONSE_BYTES;
use blaze_core::{BlazeError, Result};
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(test)]
use tokio::net::UnixListener;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::state_store::OwnedRunDir;

pub use firecracker::FirecrackerSpawner;

const TERMINATION_GRACE: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const PID_HANDOFF_GRACE: Duration = Duration::from_secs(1);
const STOPPED_MARKER: &str = "backend.stopped";
/// Fixed host-wide lock used to serialize network slot allocation.
pub(crate) const HOST_NETWORK_COORDINATION_PATH: &str = "/run/lock/blaze-network.lock";
/// Conventional host directories containing named network namespace objects.
///
/// Upstream iproute2 defaults to `/var/run/netns`, while distributions may
/// compile the same facility to use `/run/netns` directly.
pub(crate) const HOST_NAMED_NETWORK_NAMESPACE_PATHS: [&str; 2] = ["/var/run/netns", "/run/netns"];

/// Result reported when a backend process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnResult {
    /// Sandbox whose process exited.
    pub instance_id: Uuid,
    /// Normal process exit status.
    pub exit_code: Option<i32>,
    /// Terminating signal on Unix.
    pub signal: Option<i32>,
}

/// Owned runtime instance returned by a backend spawner.
#[async_trait]
pub trait BackendInstance: Send + Sync {
    /// Stable sandbox identifier.
    ///
    /// The nil default prevents legacy or test-only owners from claiming a
    /// real sandbox identity until they explicitly implement this contract.
    fn instance_id(&self) -> Uuid {
        Uuid::nil()
    }
    /// Concrete backend implementation.
    fn backend(&self) -> BackendKind;
    /// Backend version frozen into checkpoint metadata when available.
    fn version(&self) -> Option<&str> {
        None
    }
    /// Whether this backend can produce a full checkpoint snapshot.
    ///
    /// Returning `true` promises full-snapshot capture and that the
    /// quiesce/unquiesce-for-capture hooks bring the workload to a consistent
    /// stop and back. It does not require [`Self::pause`] and [`Self::resume`]
    /// to be implemented: a VM backend keeps them and inherits the default
    /// hooks, while a self-freezing backend overrides the hooks and may leave
    /// pause/resume unsupported.
    fn supports_checkpoint_capture(&self) -> bool {
        false
    }
    /// Guest transport endpoint, or an empty path for guestless backends.
    fn guest_socket_path(&self) -> &Path {
        Path::new("")
    }
    /// Whether this owner holds a per-sandbox host network slot.
    ///
    /// A restore must recreate the same shape: a snapshot references its network
    /// device, and the previous owner's cleanup removed the host device that
    /// device named, so the replacement needs a fresh slot to rebind to.
    fn holds_network_slot(&self) -> bool {
        false
    }
    /// Whether this owner records guest console output to the runtime directory.
    ///
    /// A restore that dropped this would silently stop recording console output
    /// for a sandbox whose operator asked for it, so the replacement keeps the
    /// same setting.
    fn records_console_log(&self) -> bool {
        false
    }
    /// Report an observed backend exit without waiting.
    ///
    /// `None` means the owned process or task was running when checked.
    /// Once an exit is observed, later calls continue to report a completed
    /// result even though the underlying handle has already been consumed.
    async fn try_wait(&self) -> Result<Option<SpawnResult>>;
    /// Pause guest execution for a consistent snapshot.
    async fn pause(&self) -> Result<()> {
        Err(BlazeError::BackendError {
            msg: format!("{} does not support checkpoint pause", self.backend()),
        })
    }
    /// Resume guest execution after snapshot capture.
    async fn resume(&self) -> Result<()> {
        Err(BlazeError::BackendError {
            msg: format!("{} does not support checkpoint resume", self.backend()),
        })
    }
    /// Bring the workload to a consistent stop before a snapshot is taken.
    ///
    /// VM backends must be paused externally before their state is readable,
    /// so the default delegates to [`Self::pause`]. Backends whose capture
    /// primitive freezes the workload itself (for example `runsc checkpoint`)
    /// override this as a no-op instead of tolerating a redundant pause.
    ///
    /// The quiesce must hold until [`Self::unquiesce_after_capture`]: storage
    /// synchronization and the rootfs capture run after [`Self::snapshot`]
    /// returns, so a workload that resumes earlier can write into the rootfs
    /// image without appearing in the captured state. A capture primitive
    /// that restarts the workload itself (for example a leave-running
    /// checkpoint) therefore must not pair with no-op hooks.
    async fn quiesce_for_capture(&self) -> Result<()> {
        self.pause().await
    }
    /// Return the workload to execution after a capture attempt.
    ///
    /// Called on both the publication and the compensation path. Backends
    /// whose capture leaves the workload running override this as a no-op,
    /// mirroring their [`Self::quiesce_for_capture`].
    async fn unquiesce_after_capture(&self) -> Result<()> {
        self.resume().await
    }
    /// Write a self-contained snapshot.
    async fn snapshot(&self, _request: SnapshotRequest) -> Result<()> {
        Err(BlazeError::BackendError {
            msg: format!("{} does not support checkpoint capture", self.backend()),
        })
    }
    /// Terminate the process and release all backend-owned resources.
    async fn kill(&self) -> Result<()>;
}

/// Shared backend instance handle stored in the daemon runtime map.
pub type DynBackendInstance = Arc<dyn BackendInstance>;

/// Backend launch inputs paired with the opened runtime-directory owner.
///
/// The portable request remains in `blaze-core`; this daemon-local wrapper
/// prevents backend implementations from reconstructing the runtime directory
/// from a configured pathname.
#[derive(Debug, Clone)]
pub struct BackendSpawnRequest {
    request: SpawnRequest,
    /// Opened directory used for all backend runtime artifacts.
    pub run_dir: OwnedRunDir,
}

impl BackendSpawnRequest {
    pub(crate) fn new(request: SpawnRequest, run_dir: OwnedRunDir) -> Result<Self> {
        if request.instance_id != run_dir.instance_id() {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "backend request for {} does not match runtime-directory owner for {}",
                    request.instance_id,
                    run_dir.instance_id()
                ),
            });
        }
        Ok(Self { request, run_dir })
    }
}

impl Deref for BackendSpawnRequest {
    type Target = SpawnRequest;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

/// Runtime owner that keeps the opened sandbox directory alive for as long as
/// any backend handle can still use paths derived from it.
struct RuntimeOwnedBackend {
    inner: DynBackendInstance,
    _run_dir: OwnedRunDir,
}

#[async_trait]
impl BackendInstance for RuntimeOwnedBackend {
    fn instance_id(&self) -> Uuid {
        self.inner.instance_id()
    }

    fn backend(&self) -> BackendKind {
        self.inner.backend()
    }

    fn version(&self) -> Option<&str> {
        self.inner.version()
    }

    fn supports_checkpoint_capture(&self) -> bool {
        self.inner.supports_checkpoint_capture()
    }

    fn guest_socket_path(&self) -> &Path {
        self.inner.guest_socket_path()
    }

    fn holds_network_slot(&self) -> bool {
        self.inner.holds_network_slot()
    }

    fn records_console_log(&self) -> bool {
        self.inner.records_console_log()
    }

    async fn try_wait(&self) -> Result<Option<SpawnResult>> {
        self.inner.try_wait().await
    }

    async fn pause(&self) -> Result<()> {
        self.inner.pause().await
    }

    async fn resume(&self) -> Result<()> {
        self.inner.resume().await
    }

    async fn quiesce_for_capture(&self) -> Result<()> {
        self.inner.quiesce_for_capture().await
    }

    async fn unquiesce_after_capture(&self) -> Result<()> {
        self.inner.unquiesce_after_capture().await
    }

    async fn snapshot(&self, request: SnapshotRequest) -> Result<()> {
        self.inner.snapshot(request).await
    }

    async fn kill(&self) -> Result<()> {
        self.inner.kill().await
    }
}

/// Attach runtime-directory ownership to a successful or partially started
/// backend handle before it enters lifecycle management.
pub(crate) fn bind_runtime_directory(
    inner: DynBackendInstance,
    run_dir: OwnedRunDir,
) -> DynBackendInstance {
    Arc::new(RuntimeOwnedBackend {
        inner,
        _run_dir: run_dir,
    })
}

/// Start one backend while attaching the runtime-directory owner to every
/// returned process owner, including a partial owner carried by a failure.
pub(crate) async fn spawn_with_runtime_directory(
    spawner: &dyn BackendSpawner,
    request: BackendSpawnRequest,
) -> std::result::Result<DynBackendInstance, SpawnFailure> {
    let run_dir = request.run_dir.clone();
    match spawner.spawn(request).await {
        Ok(owner) => Ok(bind_runtime_directory(owner, run_dir)),
        Err(error) => {
            let (source, owner) = error.into_parts();
            Err(match owner {
                Some(owner) => {
                    SpawnFailure::with_owner(source, bind_runtime_directory(owner, run_dir))
                }
                None => SpawnFailure::clean(source),
            })
        }
    }
}

/// A backend executable pinned by descriptor.
///
/// Preflight reads a backend's version from the file at a configured path, and
/// the launch that follows must run that same file. A restore makes the gap
/// consequential: it stops the running sandbox before it launches the
/// replacement, so an executable replaced in between would only be noticed
/// afterwards, once the original was already gone. That turns a restore that
/// preflight could have refused without harm into a sandbox left needing
/// recovery. Holding the descriptor open makes both steps name the same file,
/// even if the configured path is later pointed elsewhere.
#[derive(Debug)]
pub struct PinnedExecutable {
    /// Path the operator configured, kept for diagnostics.
    configured_path: PathBuf,
    #[cfg(target_os = "linux")]
    file: std::os::fd::OwnedFd,
}

impl PinnedExecutable {
    /// Pin the executable currently at `path`.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use rustix::fs::{MemfdFlags, Mode, OFlags, SealFlags};
            use std::io::{Read, Write};
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::MetadataExt;

            let mut source = std::fs::File::from(
                rustix::fs::open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()).map_err(
                    |error| BlazeError::BackendError {
                        msg: format!("cannot open backend executable {}: {error}", path.display()),
                    },
                )?,
            );
            let metadata = source
                .metadata()
                .map_err(|error| BlazeError::BackendError {
                    msg: format!(
                        "cannot inspect backend executable {}: {error}",
                        path.display()
                    ),
                })?;
            // Check the opened file itself, so a path repointed between the
            // check and the copy cannot substitute something that is not a
            // program.
            if !metadata.is_file() {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "backend executable {} is not a regular file",
                        path.display()
                    ),
                });
            }

            // The sealed copy below is executable in its own right, so without
            // this a restore could run a backend a cold start would refuse,
            // quietly overriding an operator who withdrew execute permission.
            //
            // Ask about the file already opened above, not the path: the path can
            // be repointed in between, which would check a replacement while the
            // copy below still reads this descriptor. Naming the descriptor
            // through `/proc` resolves to the same file whatever the path now
            // says, and `EACCESS` asks for the identity that will run it.
            let source_fd = format!("/proc/self/fd/{}", source.as_raw_fd());
            if rustix::fs::accessat(
                rustix::fs::CWD,
                source_fd.as_str(),
                rustix::fs::Access::EXEC_OK,
                rustix::fs::AtFlags::EACCESS,
            )
            .is_err()
            {
                return Err(BlazeError::BackendError {
                    msg: format!("backend executable {} is not executable", path.display()),
                });
            }

            // Name the copy after the file it was read from. A process running
            // from a sealed copy reports `/memfd:<name> (deleted)` as its
            // executable and the descriptor as `argv[0]`, so without this an
            // operator inspecting a running backend could no longer tell which
            // program it is.
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("backend");
            // `MFD_EXEC` is required on kernels that default to sealing memory
            // files against execution, and is rejected as unknown by kernels
            // that predate the flag, so fall back for those.
            let sealable = MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING;
            let copy =
                match rustix::fs::memfd_create(name, sealable | MemfdFlags::EXEC) {
                    Ok(file) => file,
                    Err(rustix::io::Errno::INVAL) => rustix::fs::memfd_create(name, sealable)
                        .map_err(|error| BlazeError::BackendError {
                            msg: format!("cannot pin backend executable: {error}"),
                        })?,
                    Err(error) => {
                        return Err(BlazeError::BackendError {
                            msg: format!("cannot pin backend executable: {error}"),
                        });
                    }
                };
            let mut writer = std::fs::File::from(copy);
            let mut buffer = [0_u8; 64 * 1024];
            let mut copied = 0_u64;
            loop {
                let read = source
                    .read(&mut buffer)
                    .map_err(|error| BlazeError::BackendError {
                        msg: format!("cannot read backend executable {}: {error}", path.display()),
                    })?;
                if read == 0 {
                    break;
                }
                writer
                    .write_all(&buffer[..read])
                    .map_err(|error| BlazeError::BackendError {
                        msg: format!("cannot pin backend executable: {error}"),
                    })?;
                copied += read as u64;
            }

            // Sealing protects the copy, not the reading of the source. An
            // in-place rewrite during the loop above would splice an old prefix
            // onto new bytes, or truncate the tail, and that mixed image would
            // then be sealed permanently. It might still answer `--version`
            // while failing the real startup path, which is the worst shape to
            // discover after the running sandbox is already stopped. Refuse a
            // source that did not hold still: refusing during preflight leaves
            // the sandbox untouched, so it costs nothing to be strict here.
            //
            // Length and modification time are not sufficient on their own,
            // because a same-length rewrite followed by `utimensat` restores
            // both. Inode change time is the indicator a writer cannot put back:
            // a write advances it, and restoring the modification time advances
            // it again.
            let after = source
                .metadata()
                .map_err(|error| BlazeError::BackendError {
                    msg: format!(
                        "cannot re-inspect backend executable {}: {error}",
                        path.display()
                    ),
                })?;
            let stable = copied == metadata.len()
                && after.len() == metadata.len()
                && after.modified().ok() == metadata.modified().ok()
                && (after.ctime(), after.ctime_nsec()) == (metadata.ctime(), metadata.ctime_nsec());
            if !stable {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "backend executable {} changed while it was being read, so the \
                         pinned copy cannot be trusted",
                        path.display()
                    ),
                });
            }

            let copy: std::os::fd::OwnedFd = writer.into();
            // Seal the copy so the bytes preflight measured cannot change, and
            // seal sealing itself so nothing can loosen that afterwards.
            rustix::fs::fcntl_add_seals(
                &copy,
                SealFlags::WRITE | SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL,
            )
            .map_err(|error| BlazeError::BackendError {
                msg: format!("cannot seal pinned backend executable: {error}"),
            })?;

            Ok(Self {
                configured_path: path.to_path_buf(),
                file: copy,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Without `/proc` there is nothing to pin to, so this platform keeps
            // the configured path and only reproduces the same error contract.
            let metadata = std::fs::metadata(path).map_err(|error| BlazeError::BackendError {
                msg: format!("cannot open backend executable {}: {error}", path.display()),
            })?;
            if !metadata.is_file() {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "backend executable {} is not a regular file",
                        path.display()
                    ),
                });
            }
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(BlazeError::BackendError {
                    msg: format!("backend executable {} is not executable", path.display()),
                });
            }
            Ok(Self {
                configured_path: path.to_path_buf(),
            })
        }
    }

    /// Program to execute, naming the pinned file rather than the path.
    pub(crate) fn program(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;

            PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.configured_path.clone()
        }
    }

    /// Path the operator configured. Only for messages, never for execution.
    pub(crate) fn configured_path(&self) -> &Path {
        &self.configured_path
    }

    /// Keep the pinned descriptor open across the child's exec chain, so
    /// [`Self::program`] still resolves once the child replaces its image.
    #[cfg(target_os = "linux")]
    pub(crate) fn inherit_into(&self, command: &mut tokio::process::Command) {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let descriptor = self.file.as_raw_fd();
        // SAFETY: `fcntl` is async-signal-safe. The closure only changes the
        // child-side copy of a descriptor this owner keeps alive across the
        // spawn call.
        unsafe {
            command.as_std_mut().pre_exec(move || {
                if libc::fcntl(descriptor, libc::F_SETFD, 0) == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn inherit_into(&self, _command: &mut tokio::process::Command) {}
}

/// Backend restore inputs paired with the opened runtime-directory owner.
#[derive(Debug, Clone)]
pub struct BackendRestoreRequest {
    request: RestoreRequest,
    /// Opened directory used for all replacement runtime artifacts.
    pub run_dir: OwnedRunDir,
    /// Backend executable pinned during preflight.
    ///
    /// `None` when no executable is configured for this backend, which is the
    /// shape of a backend that runs no separate program of its own.
    pub executable: Option<Arc<PinnedExecutable>>,
}

impl BackendRestoreRequest {
    pub(crate) fn new(
        request: RestoreRequest,
        run_dir: OwnedRunDir,
        executable: Option<Arc<PinnedExecutable>>,
    ) -> Result<Self> {
        if request.instance_id != run_dir.instance_id() {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "restore request for {} does not match runtime-directory owner for {}",
                    request.instance_id,
                    run_dir.instance_id()
                ),
            });
        }
        Ok(Self {
            request,
            run_dir,
            executable,
        })
    }
}

impl Deref for BackendRestoreRequest {
    type Target = RestoreRequest;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

/// Restore outcome that preserves ownership when cleanup cannot be confirmed.
pub type RestoreResult = std::result::Result<DynBackendInstance, SpawnFailure>;

/// Restore one backend while attaching the runtime-directory owner to every
/// returned process owner, including a partial owner carried by a failure.
pub(crate) async fn restore_with_runtime_directory(
    spawner: &dyn BackendSpawner,
    request: BackendRestoreRequest,
) -> RestoreResult {
    let run_dir = request.run_dir.clone();
    match spawner.restore(request).await {
        Ok(owner) => Ok(bind_runtime_directory(owner, run_dir)),
        Err(error) => {
            let (source, owner) = error.into_parts();
            Err(match owner {
                Some(owner) => {
                    SpawnFailure::with_owner(source, bind_runtime_directory(owner, run_dir))
                }
                None => SpawnFailure::clean(source),
            })
        }
    }
}

/// Backend start failure that may retain ownership of a started process.
pub struct SpawnFailure {
    source: BlazeError,
    owner: Option<DynBackendInstance>,
}

impl SpawnFailure {
    /// Build a failure after confirming that no backend process remains.
    pub fn clean(source: BlazeError) -> Self {
        Self {
            source,
            owner: None,
        }
    }

    /// Build a failure that transfers a partially started backend owner.
    pub fn with_owner(source: BlazeError, owner: DynBackendInstance) -> Self {
        Self {
            source,
            owner: Some(owner),
        }
    }

    /// Retry termination and retain the owner when cleanup cannot be confirmed.
    async fn compensate_started(source: BlazeError, owner: DynBackendInstance) -> Self {
        match owner.kill().await {
            Ok(()) => Self::clean(source),
            Err(cleanup) => Self::with_owner(
                BlazeError::BackendError {
                    msg: format!("{source}; started backend cleanup failed: {cleanup}"),
                },
                owner,
            ),
        }
    }

    /// Split the original failure from any retained backend owner.
    pub fn into_parts(self) -> (BlazeError, Option<DynBackendInstance>) {
        (self.source, self.owner)
    }
}

impl fmt::Debug for SpawnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpawnFailure")
            .field("source", &self.source)
            .field("owner_retained", &self.owner.is_some())
            .finish()
    }
}

impl fmt::Display for SpawnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for SpawnFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<BlazeError> for SpawnFailure {
    fn from(source: BlazeError) -> Self {
        Self::clean(source)
    }
}

impl From<std::io::Error> for SpawnFailure {
    fn from(source: std::io::Error) -> Self {
        Self::clean(source.into())
    }
}

/// Factory for owned backend runtime instances.
#[async_trait]
pub trait BackendSpawner: Send + Sync {
    /// Persist backend-specific pre-spawn ownership metadata.
    async fn prepare_spawn(&self, _run_dir: &OwnedRunDir) -> Result<()> {
        Ok(())
    }

    /// Start a new sandbox.
    async fn spawn(
        &self,
        request: BackendSpawnRequest,
    ) -> std::result::Result<DynBackendInstance, SpawnFailure>;

    /// Report the restore identity of the requested backend executable.
    ///
    /// `None` means restore is unsupported. Implementations that return a
    /// version must read it from `executable` for every call, rather than
    /// reusing mutable process-wide state or reopening the configured path:
    /// the launch that follows runs that same pinned file.
    async fn restore_capability(
        &self,
        _executable: Option<&PinnedExecutable>,
    ) -> Result<Option<RestoreCapability>> {
        Ok(None)
    }

    /// Start an owned backend from committed checkpoint artifacts.
    ///
    /// Callers prepare the PID handoff through [`Self::prepare_spawn`] first.
    /// Failures transfer any owner whose cleanup could not be confirmed.
    async fn restore(&self, request: BackendRestoreRequest) -> RestoreResult {
        let _ = request;
        Err(SpawnFailure::clean(BlazeError::BackendError {
            msg: "checkpoint restore is not supported by this backend".to_string(),
        }))
    }

    /// Probe whether the configured backend executable is usable.
    async fn probe(&self, binary_path: &Path) -> Result<bool>;

    /// Clean up a backend process and resources whose in-memory handle was
    /// lost across daemon restart.
    async fn cleanup_orphan(&self, instance_id: Uuid, run_dir: &OwnedRunDir) -> Result<()>;
}

/// Shared backend spawner selected during daemon startup.
pub type DynSpawner = Arc<dyn BackendSpawner>;

/// Backend implementations retained for kind-aware restart recovery.
#[derive(Default)]
pub struct SpawnerRegistry {
    spawners: HashMap<BackendKind, DynSpawner>,
}

impl SpawnerRegistry {
    /// Create an empty backend registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the implementation responsible for one backend kind.
    pub fn insert(&mut self, kind: BackendKind, spawner: DynSpawner) {
        self.spawners.insert(kind, spawner);
    }

    /// Return the implementation for a persisted backend kind.
    pub fn get(&self, kind: BackendKind) -> Option<DynSpawner> {
        self.spawners.get(&kind).cloned()
    }
}

/// Bubblewrap process owner used when a VM backend is not selected.
pub struct BubblewrapSpawner;

#[async_trait]
impl BackendSpawner for BubblewrapSpawner {
    async fn prepare_spawn(&self, run_dir: &OwnedRunDir) -> Result<()> {
        prepare_pid_handoff(&run_dir.path().join("backend.pid"))
    }

    async fn spawn(
        &self,
        request: BackendSpawnRequest,
    ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
        remove_file_if_exists(&request.run_dir.path().join(STOPPED_MARKER)).await?;
        let pid_file = request.run_dir.path().join("backend.pid");
        let mut command = Command::new(&request.binary_path);
        command
            .args([
                "--ro-bind",
                "/",
                "/",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--unshare-pid",
                "--unshare-net",
                "--die-with-parent",
                "--",
                "/bin/sleep",
                "3600",
            ])
            .env("BLAZE_INSTANCE_ID", request.instance_id.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let pid_handoff = configure_pid_handoff(&mut command, &pid_file)?;
        let child = command.spawn();
        drop(pid_handoff);
        let child = child?;
        let stopped_marker = request.run_dir.path().join(STOPPED_MARKER);
        let instance = ProcessInstance::new(
            request.instance_id,
            BackendKind::Bubblewrap,
            child,
            pid_file,
            stopped_marker,
        );
        Ok(Arc::new(instance))
    }

    async fn probe(&self, binary_path: &Path) -> Result<bool> {
        Ok(binary_path.is_file())
    }

    async fn cleanup_orphan(&self, instance_id: Uuid, run_dir: &OwnedRunDir) -> Result<()> {
        cleanup_process_run_dir(instance_id, run_dir.path(), "bubblewrap").await
    }
}

struct ProcessInstance {
    instance_id: Uuid,
    backend: BackendKind,
    child: Mutex<Option<Child>>,
    pid_file: PathBuf,
    stopped_marker: PathBuf,
    killed: AtomicBool,
}

impl ProcessInstance {
    fn new(
        instance_id: Uuid,
        backend: BackendKind,
        child: Child,
        pid_file: PathBuf,
        stopped_marker: PathBuf,
    ) -> Self {
        Self {
            instance_id,
            backend,
            child: Mutex::new(Some(child)),
            pid_file,
            stopped_marker,
            killed: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl BackendInstance for ProcessInstance {
    fn backend(&self) -> BackendKind {
        self.backend
    }

    async fn try_wait(&self) -> Result<Option<SpawnResult>> {
        let mut guard = self.child.lock().await;
        let Some(child) = guard.as_mut() else {
            return Ok(Some(SpawnResult {
                instance_id: self.instance_id,
                exit_code: None,
                signal: None,
            }));
        };
        let Some(status) = child.try_wait()? else {
            return Ok(None);
        };
        record_backend_stopped(&self.stopped_marker).await?;
        *guard = None;
        Ok(Some(spawn_result(self.instance_id, status)))
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
            terminate_child(child, self.backend.as_str()).await?;
        }
        record_backend_stopped(&self.stopped_marker).await?;
        *guard = None;
        remove_file_if_exists(&self.pid_file).await?;
        self.killed.store(true, Ordering::Release);
        Ok(())
    }
}

/// Portable backend used for API and lifecycle integration tests.
pub struct MockSpawner;

#[async_trait]
impl BackendSpawner for MockSpawner {
    async fn spawn(
        &self,
        request: BackendSpawnRequest,
    ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
        spawn_mock_instance(request.instance_id)
            .await
            .map_err(SpawnFailure::from)
    }

    async fn restore_capability(
        &self,
        _executable: Option<&PinnedExecutable>,
    ) -> Result<Option<RestoreCapability>> {
        Ok(Some(RestoreCapability {
            backend: BackendKind::Mock,
            version: Some("mock-v1".to_string()),
            snapshot_kind: blaze_core::backend::SnapshotKind::Full,
        }))
    }

    async fn restore(&self, request: BackendRestoreRequest) -> RestoreResult {
        let RestoreRequest {
            instance_id,
            payload_dir,
            checkpoint_backend,
            expected_version,
            snapshot_kind,
            snapshot_from_other_sandbox,
            ..
        } = request.request;
        if checkpoint_backend != BackendKind::Mock
            || expected_version.as_deref() != Some("mock-v1")
            || snapshot_kind != blaze_core::backend::SnapshotKind::Full
        {
            return Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "mock checkpoint identity is incompatible with the restore adapter"
                    .to_string(),
            }));
        }
        let vmstate: serde_json::Value = match tokio::fs::read(payload_dir.join("vmstate.snap"))
            .await
            .map_err(BlazeError::from)
            .and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|error| BlazeError::BackendError {
                    msg: format!("decode mock VM state: {error}"),
                })
            }) {
            Ok(vmstate) => vmstate,
            Err(error) => return Err(SpawnFailure::clean(error)),
        };
        // A template capture belongs to its source sandbox, so it must carry a
        // valid, non-nil identity that differs from the new owner. A rollback
        // must instead name the sandbox being restored. The cross-sandbox flag
        // relaxes equality only; it must not make a missing or malformed
        // identity acceptable.
        let recorded_identity = vmstate
            .get("instance_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .filter(|value| !value.is_nil());
        let identity_matches = match recorded_identity {
            Some(recorded) if snapshot_from_other_sandbox => recorded != instance_id,
            Some(recorded) => recorded == instance_id,
            None => false,
        };
        if vmstate.get("format").and_then(serde_json::Value::as_str) != Some("blaze-mock-v1")
            || !identity_matches
            || vmstate.get("kind").and_then(serde_json::Value::as_str) != Some("full")
        {
            return Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "mock VM state does not match the requested sandbox".to_string(),
            }));
        }
        match tokio::fs::read(payload_dir.join("memory.snap")).await {
            Ok(bytes) if bytes == b"blaze-mock-memory-v1" => spawn_mock_instance(instance_id)
                .await
                .map_err(SpawnFailure::from),
            Ok(_) => Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "mock checkpoint memory does not match the requested sandbox".to_string(),
            })),
            Err(error) => Err(SpawnFailure::clean(error.into())),
        }
    }

    async fn probe(&self, _binary_path: &Path) -> Result<bool> {
        Ok(true)
    }

    async fn cleanup_orphan(&self, _instance_id: Uuid, _run_dir: &OwnedRunDir) -> Result<()> {
        // Mock owners are in-process tasks and cannot survive daemon exit.
        Ok(())
    }
}

struct MockInstance {
    instance_id: Uuid,
    cancellation: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
    killed: AtomicBool,
}

async fn spawn_mock_instance(instance_id: Uuid) -> Result<DynBackendInstance> {
    let cancellation = CancellationToken::new();
    let task_token = cancellation.clone();
    let task = tokio::spawn(async move { task_token.cancelled().await });
    Ok(Arc::new(MockInstance {
        instance_id,
        cancellation,
        task: Mutex::new(Some(task)),
        killed: AtomicBool::new(false),
    }))
}

#[async_trait]
impl BackendInstance for MockInstance {
    fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    fn backend(&self) -> BackendKind {
        BackendKind::Mock
    }

    fn version(&self) -> Option<&str> {
        Some("mock-v1")
    }

    fn supports_checkpoint_capture(&self) -> bool {
        true
    }

    async fn try_wait(&self) -> Result<Option<SpawnResult>> {
        let task = {
            let mut task = self.task.lock().await;
            match task.as_ref() {
                Some(handle) if !handle.is_finished() => return Ok(None),
                Some(_) => task.take(),
                None => {
                    return Ok(Some(SpawnResult {
                        instance_id: self.instance_id,
                        exit_code: Some(0),
                        signal: None,
                    }));
                }
            }
        };
        if let Some(task) = task {
            let _ = task.await;
        }
        Ok(Some(SpawnResult {
            instance_id: self.instance_id,
            exit_code: Some(0),
            signal: None,
        }))
    }

    async fn pause(&self) -> Result<()> {
        Ok(())
    }

    async fn resume(&self) -> Result<()> {
        Ok(())
    }

    async fn snapshot(&self, request: SnapshotRequest) -> Result<()> {
        // The mock keeps the classic VM shape — two named files in the root
        // of its payload subtree — so flat payloads stay covered next to the
        // directory-shaped guest mock.
        tokio::fs::create_dir_all(&request.payload_dir).await?;
        let vmstate = serde_json::to_vec(&serde_json::json!({
            "format": "blaze-mock-v1",
            "instance_id": self.instance_id,
            "kind": request.kind,
        }))
        .map_err(|error| BlazeError::BackendError {
            msg: format!("serialize mock VM state: {error}"),
        })?;
        tokio::fs::write(request.payload_dir.join("vmstate.snap"), vmstate).await?;
        tokio::fs::write(
            request.payload_dir.join("memory.snap"),
            b"blaze-mock-memory-v1",
        )
        .await?;
        Ok(())
    }

    async fn kill(&self) -> Result<()> {
        if self.killed.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut task = self.task.lock().await;
        if self.killed.load(Ordering::Acquire) {
            return Ok(());
        }
        self.cancellation.cancel();
        if let Some(task) = task.take() {
            let _ = task.await;
        }
        self.killed.store(true, Ordering::Release);
        Ok(())
    }
}

/// Guest-capable mock used only by unit and integration tests.
#[cfg(test)]
pub(crate) struct GuestMockSpawner;

#[cfg(test)]
#[async_trait]
impl BackendSpawner for GuestMockSpawner {
    async fn spawn(
        &self,
        request: BackendSpawnRequest,
    ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
        spawn_guest_mock_instance(request.instance_id, &request.run_dir, HashMap::new())
            .await
            .map_err(SpawnFailure::from)
    }

    async fn restore_capability(
        &self,
        _executable: Option<&PinnedExecutable>,
    ) -> Result<Option<RestoreCapability>> {
        Ok(Some(RestoreCapability {
            backend: BackendKind::Mock,
            version: Some("guest-mock-v1".to_string()),
            snapshot_kind: blaze_core::backend::SnapshotKind::Full,
        }))
    }

    /// Restore from the directory-shaped payload written by
    /// [`GuestMockInstance::snapshot`], proving the payload contract carries
    /// a container-backend layout end to end.
    async fn restore(&self, request: BackendRestoreRequest) -> RestoreResult {
        let run_dir = request.run_dir.clone();
        let RestoreRequest {
            instance_id,
            payload_dir,
            checkpoint_backend,
            expected_version,
            snapshot_kind,
            ..
        } = request.request;
        if checkpoint_backend != BackendKind::Mock
            || expected_version.as_deref() != Some("guest-mock-v1")
            || snapshot_kind != blaze_core::backend::SnapshotKind::Full
        {
            return Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "guest mock checkpoint identity is incompatible with the restore adapter"
                    .to_string(),
            }));
        }
        let read_json = |path: PathBuf| async move {
            tokio::fs::read(&path)
                .await
                .map_err(BlazeError::from)
                .and_then(|bytes| {
                    serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
                        BlazeError::BackendError {
                            msg: format!("decode guest mock payload {}: {error}", path.display()),
                        }
                    })
                })
        };
        let vmstate = match read_json(payload_dir.join("image/checkpoint.img")).await {
            Ok(vmstate) => vmstate,
            Err(error) => return Err(SpawnFailure::clean(error)),
        };
        if vmstate.get("format").and_then(serde_json::Value::as_str) != Some("blaze-guest-mock-v1")
            || vmstate
                .get("instance_id")
                .and_then(serde_json::Value::as_str)
                != Some(instance_id.to_string().as_str())
        {
            return Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "guest mock VM state does not match the requested sandbox".to_string(),
            }));
        }
        let spec = match read_json(payload_dir.join("bundle/config.json")).await {
            Ok(spec) => spec,
            Err(error) => return Err(SpawnFailure::clean(error)),
        };
        if spec.get("instance_id").and_then(serde_json::Value::as_str)
            != Some(instance_id.to_string().as_str())
        {
            return Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "guest mock spec does not match the requested sandbox".to_string(),
            }));
        }
        let files: HashMap<String, Vec<u8>> =
            match tokio::fs::read(payload_dir.join("image/pages.bin"))
                .await
                .map_err(BlazeError::from)
                .and_then(|bytes| {
                    serde_json::from_slice(&bytes).map_err(|error| BlazeError::BackendError {
                        msg: format!("decode guest mock memory: {error}"),
                    })
                }) {
                Ok(files) => files,
                Err(error) => return Err(SpawnFailure::clean(error)),
            };
        spawn_guest_mock_instance(instance_id, &run_dir, files)
            .await
            .map_err(SpawnFailure::from)
    }

    async fn probe(&self, _binary_path: &Path) -> Result<bool> {
        Ok(true)
    }

    async fn cleanup_orphan(&self, _instance_id: Uuid, _run_dir: &OwnedRunDir) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
struct GuestMockInstance {
    instance_id: Uuid,
    guest_socket_path: PathBuf,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    cancellation: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
    killed: AtomicBool,
}

#[cfg(test)]
async fn spawn_guest_mock_instance(
    instance_id: Uuid,
    run_dir: &OwnedRunDir,
    files: HashMap<String, Vec<u8>>,
) -> Result<DynBackendInstance> {
    let socket = run_dir.path().join("vsock.uds");
    if socket.exists() {
        tokio::fs::remove_file(&socket).await?;
    }
    let listener = UnixListener::bind(&socket)?;
    let cancellation = CancellationToken::new();
    let task_token = cancellation.clone();
    let files = Arc::new(Mutex::new(files));
    let task_files = files.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = task_token.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else {
                        break;
                    };
                    let files = task_files.clone();
                    tokio::spawn(async move {
                        if let Err(error) = serve_mock_guest(stream, files).await {
                            tracing::debug!(%error, "test guest connection ended");
                        }
                    });
                }
            }
        }
    });
    Ok(Arc::new(GuestMockInstance {
        instance_id,
        guest_socket_path: socket,
        files,
        cancellation,
        task: Mutex::new(Some(task)),
        killed: AtomicBool::new(false),
    }))
}

#[cfg(test)]
#[async_trait]
impl BackendInstance for GuestMockInstance {
    fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    fn backend(&self) -> BackendKind {
        BackendKind::Mock
    }

    fn version(&self) -> Option<&str> {
        Some("guest-mock-v1")
    }

    fn supports_checkpoint_capture(&self) -> bool {
        true
    }

    fn guest_socket_path(&self) -> &Path {
        &self.guest_socket_path
    }

    async fn try_wait(&self) -> Result<Option<SpawnResult>> {
        let mut task = self.task.lock().await;
        match task.as_ref() {
            Some(handle) if !handle.is_finished() => Ok(None),
            Some(_) => {
                if let Some(handle) = task.take() {
                    let _ = handle.await;
                }
                Ok(Some(SpawnResult {
                    instance_id: self.instance_id,
                    exit_code: Some(0),
                    signal: None,
                }))
            }
            None => Ok(Some(SpawnResult {
                instance_id: self.instance_id,
                exit_code: Some(0),
                signal: None,
            })),
        }
    }

    async fn pause(&self) -> Result<()> {
        Ok(())
    }

    async fn resume(&self) -> Result<()> {
        Ok(())
    }

    async fn snapshot(&self, request: SnapshotRequest) -> Result<()> {
        // Directory-shaped payload mirroring a runsc checkpoint: an image
        // directory plus the spec copied beside it. This is the layout the
        // payload contract must be able to carry for container backends.
        let image_dir = request.payload_dir.join("image");
        let bundle_dir = request.payload_dir.join("bundle");
        tokio::fs::create_dir_all(&image_dir).await?;
        tokio::fs::create_dir_all(&bundle_dir).await?;
        let vmstate = serde_json::to_vec(&serde_json::json!({
            "format": "blaze-guest-mock-v1",
            "instance_id": self.instance_id,
            "kind": request.kind,
        }))
        .map_err(|error| BlazeError::BackendError {
            msg: format!("serialize guest mock VM state: {error}"),
        })?;
        let memory = serde_json::to_vec(&*self.files.lock().await).map_err(|error| {
            BlazeError::BackendError {
                msg: format!("serialize guest mock memory: {error}"),
            }
        })?;
        let spec = serde_json::to_vec(&serde_json::json!({
            "format": "blaze-guest-mock-spec-v1",
            "instance_id": self.instance_id,
        }))
        .map_err(|error| BlazeError::BackendError {
            msg: format!("serialize guest mock spec: {error}"),
        })?;
        tokio::fs::write(image_dir.join("checkpoint.img"), vmstate).await?;
        tokio::fs::write(image_dir.join("pages.bin"), memory).await?;
        tokio::fs::write(bundle_dir.join("config.json"), spec).await?;
        Ok(())
    }

    async fn kill(&self) -> Result<()> {
        if self.killed.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut task = self.task.lock().await;
        if self.killed.load(Ordering::Acquire) {
            return Ok(());
        }
        self.cancellation.cancel();
        if let Some(task) = task.take() {
            let _ = task.await;
        }
        if self.guest_socket_path.exists() {
            tokio::fs::remove_file(&self.guest_socket_path).await?;
        }
        self.killed.store(true, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
async fn serve_mock_guest(
    mut stream: tokio::net::UnixStream,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
) -> std::io::Result<()> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;

    let connect = read_mock_line(&mut stream, 128).await?;
    if !connect.starts_with(b"CONNECT ") {
        return Ok(());
    }
    stream.write_all(b"OK 5000\n").await?;
    let request = read_mock_line(&mut stream, DEFAULT_MAX_RESPONSE_BYTES).await?;
    let request: serde_json::Value = match serde_json::from_slice(&request) {
        Ok(request) => request,
        Err(_) => return Ok(()),
    };
    let id = request.get("id").cloned().unwrap_or_default();
    let response = match request.get("op").and_then(serde_json::Value::as_str) {
        Some("exec") => {
            let command = request
                .get("cmd")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            serde_json::json!({
                "id": id,
                "ok": true,
                "rc": 0,
                "stdout_b64": BASE64.encode(command.as_bytes()),
                "stderr_b64": ""
            })
        }
        Some("read") => {
            let path = request
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let data = files.lock().await.get(path).cloned().unwrap_or_default();
            serde_json::json!({"id": id, "ok": true, "data_b64": BASE64.encode(data)})
        }
        Some("write") => {
            let path = request
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let data = request
                .get("data_b64")
                .and_then(serde_json::Value::as_str)
                .and_then(|encoded| BASE64.decode(encoded).ok())
                .unwrap_or_default();
            files.lock().await.insert(path, data);
            serde_json::json!({"id": id, "ok": true})
        }
        _ => serde_json::json!({"id": id, "ok": true}),
    };
    let mut encoded = serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
    encoded.push(b'\n');
    stream.write_all(&encoded).await
}

#[cfg(test)]
async fn read_mock_line<R>(stream: &mut R, limit: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stream).take(limit.saturating_add(1) as u64);
    let mut output = Vec::with_capacity(limit.min(8192));
    reader.read_until(b'\n', &mut output).await?;
    if output.last() == Some(&b'\n') {
        output.pop();
        if output.len() <= limit {
            return Ok(output);
        }
    }
    if output.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "mock guest line too long",
        ));
    }
    Ok(output)
}

pub(super) async fn terminate_child(child: &mut Child, backend: &str) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Err(error) = signal_process(child.id(), "-TERM").await {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        tracing::warn!(backend, %error, "SIGTERM request failed; sending SIGKILL");
        child.start_kill()?;
        child.wait().await?;
        return Ok(());
    }
    match tokio::time::timeout(TERMINATION_GRACE, child.wait()).await {
        Ok(status) => {
            status?;
        }
        Err(_) => {
            tracing::warn!(backend, "graceful termination timed out; sending SIGKILL");
            child.start_kill()?;
            child.wait().await?;
        }
    }
    Ok(())
}

pub(super) async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn record_backend_stopped(marker: &Path) -> Result<()> {
    tokio::fs::write(marker, b"stopped\n").await?;
    Ok(())
}

pub(super) fn stopped_marker(run_dir: &Path) -> PathBuf {
    run_dir.join(STOPPED_MARKER)
}

#[cfg(unix)]
pub(super) struct PidHandoff {
    _file: std::fs::File,
}

#[cfg(not(unix))]
pub(super) struct PidHandoff;

#[cfg(unix)]
pub(super) fn prepare_pid_handoff(pid_file: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let pid_path =
        CString::new(pid_file.as_os_str().as_bytes()).map_err(|_| BlazeError::BackendError {
            msg: format!("PID file path contains a NUL byte: {}", pid_file.display()),
        })?;
    let fd = unsafe {
        libc::open(
            pid_path.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.sync_all()?;
    if let Some(parent) = pid_file.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn prepare_pid_handoff(pid_file: &Path) -> Result<()> {
    let file = std::fs::File::create(pid_file)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn configure_pid_handoff(command: &mut Command, pid_file: &Path) -> Result<PidHandoff> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    let pid_path =
        CString::new(pid_file.as_os_str().as_bytes()).map_err(|_| BlazeError::BackendError {
            msg: format!("PID file path contains a NUL byte: {}", pid_file.display()),
        })?;
    let fd = unsafe {
        libc::open(
            pid_path.as_ptr(),
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let child_fd = file.as_raw_fd();
    // SAFETY: the closure calls only async-signal-safe libc functions and does
    // not allocate after fork. The returned guard keeps `child_fd` open and
    // locked until `Command::spawn` completes.
    unsafe {
        command
            .as_std_mut()
            .pre_exec(move || write_current_pid(child_fd));
    }
    Ok(PidHandoff { _file: file })
}

#[cfg(not(unix))]
pub(super) fn configure_pid_handoff(
    _command: &mut Command,
    _pid_file: &Path,
) -> Result<PidHandoff> {
    Ok(PidHandoff)
}

#[cfg(unix)]
fn write_current_pid(fd: libc::c_int) -> std::io::Result<()> {
    if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::ftruncate(fd, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    write_pid_and_sync(fd)
}

#[cfg(unix)]
fn write_pid_and_sync(fd: libc::c_int) -> std::io::Result<()> {
    let mut buffer = [0_u8; 16];
    let mut cursor = buffer.len();
    cursor -= 1;
    buffer[cursor] = b'\n';
    let mut pid = unsafe { libc::getpid() } as u32;
    loop {
        cursor -= 1;
        buffer[cursor] = b'0' + (pid % 10) as u8;
        pid /= 10;
        if pid == 0 {
            break;
        }
    }

    let mut remaining = &buffer[cursor..];
    while !remaining.is_empty() {
        let written = unsafe {
            libc::write(
                fd,
                remaining.as_ptr().cast::<libc::c_void>(),
                remaining.len(),
            )
        };
        if written < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        remaining = &remaining[written as usize..];
    }
    if unsafe { libc::fsync(fd) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

async fn cleanup_process_run_dir(instance_id: Uuid, run_dir: &Path, backend: &str) -> Result<()> {
    let stopped_marker = stopped_marker(run_dir);
    if stopped_marker.is_file() {
        return Ok(());
    }
    let pid_file = run_dir.join("backend.pid");
    #[cfg(target_os = "linux")]
    terminate_recorded_process(instance_id, &pid_file, backend).await?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = instance_id;
        if pid_file.exists() {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "cannot validate {backend} orphan {} outside Linux",
                    pid_file.display()
                ),
            });
        }
    }
    record_backend_stopped(&stopped_marker).await?;
    remove_file_if_exists(&pid_file).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) async fn terminate_recorded_process(
    instance_id: Uuid,
    pid_file: &Path,
    backend: &str,
) -> Result<()> {
    let raw = match wait_for_pid_handoff(pid_file).await? {
        Some(raw) => raw,
        None => return Ok(()),
    };
    let pid: u32 = raw
        .trim()
        .parse()
        .map_err(|error| BlazeError::BackendError {
            msg: format!("invalid {backend} pid file {}: {error}", pid_file.display()),
        })?;
    let process_dir = PathBuf::from(format!("/proc/{pid}"));
    let environ = match tokio::fs::read(process_dir.join("environ")).await {
        Ok(environ) => environ,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let expected = format!("BLAZE_INSTANCE_ID={instance_id}");
    if !environ
        .split(|byte| *byte == 0)
        .any(|entry| entry == expected.as_bytes())
    {
        return Err(BlazeError::BackendError {
            msg: format!(
                "refusing to terminate {backend} pid {pid}: BLAZE_INSTANCE_ID does not match {instance_id}"
            ),
        });
    }

    if let Err(error) = signal_process(Some(pid), "-TERM").await {
        if !process_is_running(&process_dir)? {
            return Ok(());
        }
        return Err(error);
    }
    if wait_for_process_exit(&process_dir, TERMINATION_GRACE).await? {
        return Ok(());
    }
    tracing::warn!(backend, pid, "orphan ignored SIGTERM; sending SIGKILL");
    if let Err(error) = signal_process(Some(pid), "-KILL").await {
        if !process_is_running(&process_dir)? {
            return Ok(());
        }
        return Err(error);
    }
    if !wait_for_process_exit(&process_dir, TERMINATION_GRACE).await? {
        return Err(BlazeError::BackendError {
            msg: format!("{backend} orphan pid {pid} did not exit"),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn wait_for_pid_handoff(pid_file: &Path) -> Result<Option<String>> {
    let deadline = Instant::now() + PID_HANDOFF_GRACE;
    loop {
        match read_pid_handoff(pid_file)? {
            PidHandoffState::NotStarted => return Ok(None),
            PidHandoffState::Missing => {
                return Err(BlazeError::BackendError {
                    msg: format!(
                        "cannot confirm backend process ownership: missing PID handoff {}",
                        pid_file.display()
                    ),
                });
            }
            PidHandoffState::Ready(raw) => return Ok(Some(raw)),
            PidHandoffState::InProgress => {}
        }
        if Instant::now() >= deadline {
            return Err(BlazeError::BackendError {
                msg: format!(
                    "cannot confirm backend process ownership: PID handoff is still in progress at {}",
                    pid_file.display()
                ),
            });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(target_os = "linux")]
enum PidHandoffState {
    Missing,
    NotStarted,
    InProgress,
    Ready(String),
}

#[cfg(target_os = "linux")]
fn read_pid_handoff(pid_file: &Path) -> Result<PidHandoffState> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::AsRawFd;

    let mut file = match std::fs::OpenOptions::new().read(true).open(pid_file) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PidHandoffState::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EAGAIN)
            || error.raw_os_error() == Some(libc::EWOULDBLOCK)
        {
            return Ok(PidHandoffState::InProgress);
        }
        return Err(error.into());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    if raw.trim().is_empty() {
        Ok(PidHandoffState::NotStarted)
    } else {
        Ok(PidHandoffState::Ready(raw))
    }
}

#[cfg(target_os = "linux")]
async fn wait_for_process_exit(process_dir: &Path, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    while process_is_running(process_dir)? && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(!process_is_running(process_dir)?)
}

#[cfg(target_os = "linux")]
fn process_is_running(process_dir: &Path) -> Result<bool> {
    let stat = match std::fs::read_to_string(process_dir.join("stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let state = stat
        .rsplit_once(") ")
        .and_then(|(_, fields)| fields.chars().next())
        .ok_or_else(|| BlazeError::BackendError {
            msg: format!("invalid process status in {}", process_dir.display()),
        })?;
    Ok(state != 'Z')
}

async fn signal_process(pid: Option<u32>, signal: &str) -> Result<()> {
    let Some(pid) = pid else {
        return Ok(());
    };
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("kill")
            .arg(signal)
            .arg(pid.to_string())
            .env("LC_ALL", "C")
            .status(),
    )
    .await
    .map_err(|_| BlazeError::BackendError {
        msg: format!("kill {signal} {pid} timed out"),
    })??;
    if !status.success() {
        return Err(BlazeError::BackendError {
            msg: format!("kill {signal} {pid} exited with {status}"),
        });
    }
    Ok(())
}

fn spawn_result(instance_id: Uuid, status: std::process::ExitStatus) -> SpawnResult {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        SpawnResult {
            instance_id,
            exit_code: status.code(),
            signal: status.signal(),
        }
    }
    #[cfg(not(unix))]
    {
        SpawnResult {
            instance_id,
            exit_code: status.code(),
            signal: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pinned_executable_survives_an_in_place_rewrite() {
        use std::io::Write;

        // A descriptor pins the inode, not its contents, so an in-place rewrite
        // would still be visible through it. The kernel refuses that write only
        // while some process executes the inode, which stops holding once a
        // restore kills the runtime it is replacing.
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("backend");
        std::fs::write(&path, b"original-backend-bytes").expect("write original");
        make_executable(&path);

        let pinned = PinnedExecutable::open(&path).expect("pin the original");

        // Rewrite in place, keeping the same inode.
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("reopen for rewrite")
            .write_all(b"rewritten-bytes")
            .expect("rewrite in place");

        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                std::fs::read(pinned.program()).expect("read through the pin"),
                b"original-backend-bytes",
                "the sealed copy must keep the bytes preflight accepted"
            );
        }
        assert_eq!(pinned.configured_path(), path.as_path());
    }

    #[test]
    fn pinned_executable_survives_a_replaced_path() {
        // The other direction: the path is repointed at a different file, the
        // way an atomic package upgrade does it.
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("backend");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write original");
        make_executable(&path);

        let pinned = PinnedExecutable::open(&path).expect("pin the original");

        // Replace the path the way a package upgrade would.
        std::fs::remove_file(&path).expect("remove original");
        std::fs::write(&path, b"#!/bin/sh\nexit 1\n").expect("write replacement");

        assert_eq!(
            pinned.configured_path(),
            path.as_path(),
            "the configured path stays available for diagnostics"
        );
        #[cfg(target_os = "linux")]
        {
            let program = pinned.program();
            assert!(
                program.starts_with("/proc/self/fd/"),
                "a restore must execute the pinned descriptor, got {}",
                program.display()
            );
            assert_eq!(
                std::fs::read(&program).expect("read through the pin"),
                b"#!/bin/sh\nexit 0\n",
                "the pin must still resolve to the file preflight accepted"
            );
        }
    }

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("mark executable");
    }

    #[test]
    fn pinning_rejects_an_executable_an_operator_disabled() {
        // The sealed copy is executable in its own right, so a restore must not
        // run a backend that a cold start would refuse.
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("backend");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write backend");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("withdraw execute permission");

        let error = PinnedExecutable::open(&path).expect_err("must refuse a non-executable file");
        assert!(
            format!("{error}").contains("not executable"),
            "unexpected error: {error}"
        );

        make_executable(&path);
        PinnedExecutable::open(&path).expect("an executable file is accepted");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinning_checks_executability_of_the_file_it_copies() {
        // The permission check must describe the file that gets sealed. Checking
        // the path instead would let a replacement vouch for a source that is
        // not executable, reinstating the bypass through the back door.
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("backend");
        std::fs::write(&path, b"non-executable-source").expect("write source");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("withdraw execute permission");

        // Hold the non-executable file open, then repoint the path at an
        // executable one, which is what a replacement racing preflight looks
        // like from here.
        let held = std::fs::File::open(&path).expect("hold the source open");
        std::fs::remove_file(&path).expect("unlink the source");
        std::fs::write(&path, b"executable-replacement").expect("write replacement");
        make_executable(&path);

        // Opening now legitimately succeeds: it reads the replacement, which is
        // executable. The point of the test is the inverse case below.
        PinnedExecutable::open(&path).expect("the replacement is executable");
        drop(held);

        // With the path still naming a non-executable file, the check must fail
        // rather than consult anything else.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("withdraw execute permission again");
        let error = PinnedExecutable::open(&path).expect_err("must refuse what it would copy");
        assert!(
            format!("{error}").contains("not executable"),
            "unexpected error: {error}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timestamp_preserving_rewrites_are_still_detectable() {
        // A same-length rewrite followed by `utimensat` puts length and
        // modification time back, so neither can carry the check on its own. This
        // pins the property the check relies on: inode change time still moves,
        // because the write advances it and restoring the timestamps advances it
        // again, and no ordinary writer can put it back.
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("backend");
        std::fs::write(&path, b"AAAA").expect("write source");
        let before = std::fs::metadata(&path).expect("stat source");

        // Same length, then both timestamps restored.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"BBBB").expect("rewrite in place");
        let times = [
            rustix::fs::Timespec {
                tv_sec: before.atime(),
                tv_nsec: before.atime_nsec() as _,
            },
            rustix::fs::Timespec {
                tv_sec: before.mtime(),
                tv_nsec: before.mtime_nsec() as _,
            },
        ];
        rustix::fs::utimensat(
            rustix::fs::CWD,
            &path,
            &rustix::fs::Timestamps {
                last_access: times[0],
                last_modification: times[1],
            },
            rustix::fs::AtFlags::empty(),
        )
        .expect("restore timestamps");

        let after = std::fs::metadata(&path).expect("re-stat source");
        assert_eq!(
            after.len(),
            before.len(),
            "length is restored by construction"
        );
        assert_eq!(
            after.modified().ok(),
            before.modified().ok(),
            "modification time is restored, so it cannot carry the check"
        );
        assert_ne!(
            (after.ctime(), after.ctime_nsec()),
            (before.ctime(), before.ctime_nsec()),
            "inode change time must still reveal the rewrite"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_copy_holds_the_whole_source() {
        // The stability check rejects a pin whose copied length disagrees with
        // the size measured before the copy, so a successful pin must carry the
        // source in full — a short copy would otherwise be sealed as a truncated
        // program and only fail once the sandbox it replaces is already stopped.
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("backend");
        // Larger than the copy buffer, so the loop takes several reads.
        let contents: Vec<u8> = (0..300 * 1024).map(|index| index as u8).collect();
        std::fs::write(&path, &contents).expect("write source");
        make_executable(&path);

        let pinned = PinnedExecutable::open(&path).expect("pin a stable source");
        assert_eq!(
            std::fs::read(pinned.program()).expect("read the sealed copy"),
            contents,
            "the sealed copy must hold every byte of the source"
        );
    }

    #[test]
    fn pinning_rejects_a_path_that_is_not_a_regular_file() {
        let temp = tempfile::tempdir().expect("temp");
        let error = PinnedExecutable::open(temp.path()).expect_err("a directory is not a program");
        assert!(
            format!("{error}").contains("regular file"),
            "unexpected error: {error}"
        );
    }

    #[cfg(target_os = "linux")]
    use std::time::Duration;

    use blaze_core::backend::{RestoreRequest, SnapshotKind, SnapshotRequest, SpawnRequest};
    use blaze_core::policy::BackendConfigs;
    use blaze_core::storage::StorageSlot;

    use crate::guest::GuestClient;

    use super::*;

    struct UnsupportedInstance;

    #[async_trait]
    impl BackendInstance for UnsupportedInstance {
        fn backend(&self) -> BackendKind {
            BackendKind::Bubblewrap
        }

        async fn try_wait(&self) -> Result<Option<SpawnResult>> {
            Ok(None)
        }

        async fn kill(&self) -> Result<()> {
            Ok(())
        }
    }

    fn request(root: &Path) -> BackendSpawnRequest {
        let id = Uuid::new_v4();
        let slot_dir = root.join("slot");
        BackendSpawnRequest::new(
            SpawnRequest {
                instance_id: id,
                binary_path: PathBuf::new(),
                storage: StorageSlot {
                    id: id.to_string(),
                    rootfs_path: slot_dir.join("rootfs.ext4"),
                    mem_path: slot_dir.join("mem.bin"),
                    mem_diff_path: slot_dir.join("mem.diff"),
                    rootfs_diff_path: slot_dir.join("rootfs.diff"),
                    instance_dir: slot_dir,
                },
                backend: BackendConfigs::default(),
                vm: None,
            },
            OwnedRunDir::for_test(id, root.join("run")),
        )
        .expect("matching backend request")
    }

    #[test]
    fn backend_request_rejects_a_mismatched_runtime_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let mut request = request(temp.path());
        request.request.instance_id = Uuid::new_v4();
        assert!(
            BackendSpawnRequest::new(request.request.clone(), request.run_dir.clone()).is_err()
        );
    }

    #[cfg(target_os = "linux")]
    struct PartialPathOwner {
        marker: PathBuf,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl BackendInstance for PartialPathOwner {
        fn backend(&self) -> BackendKind {
            BackendKind::Mock
        }

        async fn try_wait(&self) -> Result<Option<SpawnResult>> {
            Ok(None)
        }

        async fn kill(&self) -> Result<()> {
            std::fs::write(&self.marker, b"partial owner")?;
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    struct PartialPathSpawner;

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl BackendSpawner for PartialPathSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            let owner: DynBackendInstance = Arc::new(PartialPathOwner {
                marker: request.run_dir.path().join("partial-owner-marker"),
            });
            Err(SpawnFailure::with_owner(
                BlazeError::BackendError {
                    msg: "injected partial start".into(),
                },
                owner,
            ))
        }

        async fn probe(&self, _binary_path: &Path) -> Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(&self, _instance_id: Uuid, _run_dir: &OwnedRunDir) -> Result<()> {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn partial_spawn_failure_retains_the_runtime_directory_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let request = request(temp.path());
        let failure = match spawn_with_runtime_directory(&PartialPathSpawner, request).await {
            Ok(_) => panic!("partial spawn must fail"),
            Err(failure) => failure,
        };
        let (source, owner) = failure.into_parts();
        assert!(source.to_string().contains("injected partial start"));
        let owner = owner.expect("partial backend owner");

        let configured_run_dir = temp.path().join("run");
        let owned_run_dir = temp.path().join("owned-partial-run");
        std::fs::rename(&configured_run_dir, &owned_run_dir).expect("move owned runtime directory");
        std::fs::create_dir(&configured_run_dir).expect("replacement runtime directory");

        owner.kill().await.expect("use retained runtime owner");

        assert_eq!(
            std::fs::read(owned_run_dir.join("partial-owner-marker")).expect("owned marker"),
            b"partial owner"
        );
        assert!(!configured_run_dir.join("partial-owner-marker").exists());
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_instance_marker(child: &Child, instance_id: Uuid) {
        let pid = child.id().expect("child pid");
        let expected = format!("BLAZE_INSTANCE_ID={instance_id}");
        let environ_path = PathBuf::from(format!("/proc/{pid}/environ"));
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Ok(environ) = tokio::fs::read(&environ_path).await
                && environ
                    .split(|byte| *byte == 0)
                    .any(|entry| entry == expected.as_bytes())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "child environment marker did not become visible"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn production_mock_does_not_advertise_guest_transport() {
        let temp = tempfile::tempdir().expect("temp");
        let instance = MockSpawner
            .spawn(request(temp.path()))
            .await
            .expect("spawn");

        assert!(instance.guest_socket_path().as_os_str().is_empty());
        assert!(!temp.path().join("run/vsock.uds").exists());
        instance.kill().await.expect("kill");
    }

    #[tokio::test]
    async fn test_guest_instance_supports_io_and_idempotent_kill() {
        let temp = tempfile::tempdir().expect("temp");
        let request = request(temp.path());
        let run_dir = request.run_dir.clone();
        let instance = spawn_with_runtime_directory(&GuestMockSpawner, request)
            .await
            .expect("spawn");
        drop(run_dir);
        let configured_run_dir = temp.path().join("run");
        let owned_run_dir = temp.path().join("owned-run");
        std::fs::rename(&configured_run_dir, &owned_run_dir).expect("move owned runtime directory");
        std::fs::create_dir(&configured_run_dir).expect("replacement runtime directory");
        assert_eq!(instance.backend(), BackendKind::Mock);
        let client = GuestClient::new(
            instance.guest_socket_path().to_path_buf(),
            Duration::from_secs(1),
            1024,
        );
        client
            .write_file("/tmp/value".into(), b"hello")
            .await
            .expect("write");
        assert_eq!(
            client.read_file("/tmp/value".into()).await.expect("read"),
            b"hello"
        );
        assert!(owned_run_dir.join("vsock.uds").exists());
        assert!(!configured_run_dir.join("vsock.uds").exists());
        assert_eq!(instance.try_wait().await.expect("try wait"), None);
        instance.kill().await.expect("kill");
        assert!(instance.try_wait().await.expect("try wait").is_some());
        instance.kill().await.expect("idempotent kill");
    }

    #[tokio::test]
    async fn guest_mock_directory_payload_round_trips() {
        let temp = tempfile::tempdir().expect("temp");
        let request = request(temp.path());
        let instance_id = request.instance_id;
        let binary_path = request.binary_path.clone();
        let storage = request.storage.clone();
        let run_dir = request.run_dir.clone();
        let instance = spawn_with_runtime_directory(&GuestMockSpawner, request)
            .await
            .expect("spawn");
        let client = GuestClient::new(
            instance.guest_socket_path().to_path_buf(),
            Duration::from_secs(1),
            1024,
        );
        client
            .write_file("/tmp/mark".into(), b"survives the payload")
            .await
            .expect("write guest state");

        let payload_dir = temp.path().join("payload");
        instance
            .snapshot(SnapshotRequest {
                payload_dir: payload_dir.clone(),
                kind: SnapshotKind::Full,
            })
            .await
            .expect("snapshot");
        // The payload is a subtree, not a fixed file pair: the contract must
        // carry a container-backend layout without renaming anything.
        assert!(payload_dir.join("image/checkpoint.img").is_file());
        assert!(payload_dir.join("image/pages.bin").is_file());
        assert!(payload_dir.join("bundle/config.json").is_file());
        // The captured host shape is probed while the owner is still alive,
        // matching the generic restore transaction.
        let preserve_network = instance.holds_network_slot();
        let record_console_log = instance.records_console_log();
        instance.kill().await.expect("kill");

        let restore = RestoreRequest {
            instance_id,
            binary_path,
            storage,
            payload_dir,
            checkpoint_backend: BackendKind::Mock,
            expected_version: Some("guest-mock-v1".to_string()),
            snapshot_kind: SnapshotKind::Full,
            expose_guest_socket: true,
            preserve_network,
            record_console_log,
            snapshot_from_other_sandbox: false,
        };
        // The guest mock runs no pinned executable: it owns an in-process task
        // rather than a backend binary.
        let restore = BackendRestoreRequest::new(restore, run_dir, None).expect("restore request");
        let restored = restore_with_runtime_directory(&GuestMockSpawner, restore)
            .await
            .expect("restore");
        let client = GuestClient::new(
            restored.guest_socket_path().to_path_buf(),
            Duration::from_secs(1),
            1024,
        );
        assert_eq!(
            client
                .read_file("/tmp/mark".into())
                .await
                .expect("read restored guest state"),
            b"survives the payload"
        );
        restored.kill().await.expect("kill restored");
    }

    #[tokio::test]
    async fn checkpoint_capture_defaults_fail_closed() {
        let temp = tempfile::tempdir().expect("temp");
        let instance = UnsupportedInstance;
        let request = SnapshotRequest {
            payload_dir: temp.path().join("payload"),
            kind: SnapshotKind::Full,
        };

        assert_eq!(instance.instance_id(), Uuid::nil());
        assert_eq!(instance.version(), None);
        assert!(!instance.supports_checkpoint_capture());
        assert!(instance.pause().await.is_err());
        assert!(instance.resume().await.is_err());
        // The quiesce hooks delegate to pause/resume by default, so a
        // backend without either capability fails closed on both.
        assert!(instance.quiesce_for_capture().await.is_err());
        assert!(instance.unquiesce_after_capture().await.is_err());
        assert!(instance.snapshot(request).await.is_err());
    }

    /// A template capture records its source sandbox, so the mock adapter must
    /// accept a differing identity for a template restore while still refusing
    /// one for a same-sandbox rollback.
    #[tokio::test]
    async fn mock_restore_accepts_a_foreign_identity_only_for_templates() {
        for from_other_sandbox in [false, true] {
            let temp = tempfile::tempdir().expect("temp");
            let spawn = request(temp.path());
            let run_dir = spawn.run_dir.clone();
            let payload_dir = temp.path().join("payload");
            std::fs::create_dir(&payload_dir).expect("payload directory");
            let snapshot_path = payload_dir.join("vmstate.snap");
            let mem_path = payload_dir.join("memory.snap");
            // Record a different sandbox, exactly as a published template does.
            std::fs::write(
                &snapshot_path,
                serde_json::to_vec(&serde_json::json!({
                    "format": "blaze-mock-v1",
                    "instance_id": Uuid::new_v4(),
                    "kind": "full",
                }))
                .expect("mock vmstate"),
            )
            .expect("write vmstate");
            std::fs::write(&mem_path, b"blaze-mock-memory-v1").expect("write memory");

            let restore = BackendRestoreRequest::new(
                RestoreRequest {
                    instance_id: spawn.instance_id,
                    binary_path: PathBuf::new(),
                    storage: spawn.storage.clone(),
                    payload_dir,
                    checkpoint_backend: BackendKind::Mock,
                    expected_version: Some("mock-v1".to_string()),
                    snapshot_kind: SnapshotKind::Full,
                    expose_guest_socket: false,
                    preserve_network: false,
                    record_console_log: false,
                    snapshot_from_other_sandbox: from_other_sandbox,
                },
                run_dir,
                None,
            )
            .expect("restore request");

            let restored = MockSpawner.restore(restore).await;
            if from_other_sandbox {
                let owner = restored.expect("template restore accepts a foreign identity");
                assert_eq!(owner.instance_id(), spawn.instance_id);
                owner.kill().await.expect("release mock owner");
            } else {
                let error = restored.err().expect("rollback rejects a foreign identity");
                assert!(
                    error
                        .to_string()
                        .contains("does not match the requested sandbox"),
                    "{error}"
                );
            }
        }
    }

    #[tokio::test]
    async fn mock_template_restore_requires_a_valid_foreign_identity() {
        for identity_case in ["missing", "malformed", "nil", "target"] {
            let temp = tempfile::tempdir().expect("temp");
            let spawn = request(temp.path());
            let payload_dir = temp.path().join("payload");
            std::fs::create_dir(&payload_dir).expect("payload directory");
            let snapshot_path = payload_dir.join("vmstate.snap");
            let mem_path = payload_dir.join("memory.snap");
            let mut vmstate = serde_json::json!({
                "format": "blaze-mock-v1",
                "kind": "full",
            });
            match identity_case {
                "missing" => {}
                "malformed" => vmstate["instance_id"] = serde_json::json!("not-a-uuid"),
                "nil" => vmstate["instance_id"] = serde_json::json!(Uuid::nil()),
                "target" => vmstate["instance_id"] = serde_json::json!(spawn.instance_id),
                _ => unreachable!("covered identity case"),
            }
            std::fs::write(
                &snapshot_path,
                serde_json::to_vec(&vmstate).expect("mock vmstate"),
            )
            .expect("write vmstate");
            std::fs::write(&mem_path, b"blaze-mock-memory-v1").expect("write memory");

            let restore = BackendRestoreRequest::new(
                RestoreRequest {
                    instance_id: spawn.instance_id,
                    binary_path: PathBuf::new(),
                    storage: spawn.storage.clone(),
                    payload_dir,
                    checkpoint_backend: BackendKind::Mock,
                    expected_version: Some("mock-v1".to_string()),
                    snapshot_kind: SnapshotKind::Full,
                    expose_guest_socket: false,
                    preserve_network: false,
                    record_console_log: false,
                    snapshot_from_other_sandbox: true,
                },
                spawn.run_dir.clone(),
                None,
            )
            .expect("restore request");

            let error = MockSpawner
                .restore(restore)
                .await
                .err()
                .expect("invalid template source identity must be rejected");
            assert!(
                error
                    .to_string()
                    .contains("does not match the requested sandbox"),
                "{identity_case}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn self_freezing_backends_can_bypass_pause_for_capture() {
        struct SelfFreezing;

        #[async_trait]
        impl BackendInstance for SelfFreezing {
            fn backend(&self) -> BackendKind {
                BackendKind::Mock
            }

            async fn try_wait(&self) -> Result<Option<SpawnResult>> {
                Ok(None)
            }

            // The capture primitive freezes the workload itself, so the
            // orchestration hooks are no-ops while the pause/resume
            // primitives stay unimplemented.
            async fn quiesce_for_capture(&self) -> Result<()> {
                Ok(())
            }

            async fn unquiesce_after_capture(&self) -> Result<()> {
                Ok(())
            }

            async fn kill(&self) -> Result<()> {
                Ok(())
            }
        }

        let instance = SelfFreezing;
        assert!(instance.pause().await.is_err());
        assert!(instance.resume().await.is_err());
        assert!(instance.quiesce_for_capture().await.is_ok());
        assert!(instance.unquiesce_after_capture().await.is_ok());
    }

    #[tokio::test]
    async fn checkpoint_restore_defaults_fail_closed_without_an_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let spawn = request(temp.path());
        let run_dir = spawn.run_dir.clone();
        let restore = RestoreRequest {
            instance_id: spawn.instance_id,
            binary_path: spawn.binary_path.clone(),
            storage: spawn.storage.clone(),
            payload_dir: temp.path().join("payload"),
            checkpoint_backend: BackendKind::Bubblewrap,
            expected_version: None,
            snapshot_kind: SnapshotKind::Full,
            expose_guest_socket: true,
            preserve_network: false,
            record_console_log: false,
            snapshot_from_other_sandbox: false,
        };
        let executable =
            Arc::new(PinnedExecutable::open(Path::new("/bin/sh")).expect("pin a real executable"));
        let restore = BackendRestoreRequest::new(restore, run_dir, Some(executable.clone()))
            .expect("restore request");

        assert!(
            BubblewrapSpawner
                .restore_capability(Some(&executable))
                .await
                .expect("capability")
                .is_none()
        );
        let failure = match BubblewrapSpawner.restore(restore).await {
            Ok(_) => panic!("restore must remain unsupported"),
            Err(failure) => failure,
        };
        let (source, owner) = failure.into_parts();
        assert!(source.to_string().contains("restore is not supported"));
        assert!(owner.is_none());
    }

    #[tokio::test]
    async fn mock_instance_captures_self_contained_state() {
        let temp = tempfile::tempdir().expect("temp");
        let spawn = request(temp.path());
        let instance_id = spawn.instance_id;
        let instance = MockSpawner.spawn(spawn).await.expect("spawn");
        let payload_dir = temp.path().join("checkpoint");
        let snapshot_path = payload_dir.join("vmstate.snap");
        let mem_path = payload_dir.join("memory.snap");

        assert_eq!(instance.instance_id(), instance_id);
        assert_eq!(instance.version(), Some("mock-v1"));
        assert!(instance.supports_checkpoint_capture());
        instance.pause().await.expect("pause");
        instance
            .snapshot(SnapshotRequest {
                payload_dir,
                kind: SnapshotKind::Full,
            })
            .await
            .expect("snapshot");
        instance.resume().await.expect("resume");

        let vmstate: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&snapshot_path).expect("VM state"))
                .expect("VM state JSON");
        assert_eq!(vmstate["instance_id"], instance_id.to_string());
        assert_eq!(vmstate["kind"], "full");
        assert_eq!(
            std::fs::read(&mem_path).expect("memory"),
            b"blaze-mock-memory-v1"
        );
        instance.kill().await.expect("kill");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn child_termination_requests_graceful_exit_first() {
        let temp = tempfile::tempdir().expect("temp");
        let marker = temp.path().join("terminated");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap 'printf term > \"$MARKER\"; exit 0' TERM; while :; do sleep 1; done")
            .env("MARKER", &marker)
            .spawn()
            .expect("spawn child");
        tokio::time::sleep(Duration::from_millis(50)).await;

        terminate_child(&mut child, "test")
            .await
            .expect("terminate child");

        assert_eq!(std::fs::read_to_string(marker).expect("marker"), "term");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphan_cleanup_requires_matching_instance_marker() {
        let temp = tempfile::tempdir().expect("temp");
        let expected_id = Uuid::new_v4();
        let actual_id = Uuid::new_v4();
        let pid_file = temp.path().join("backend.pid");
        let mut child = Command::new("sleep")
            .arg("60")
            .env("BLAZE_INSTANCE_ID", actual_id.to_string())
            .spawn()
            .expect("spawn child");
        wait_for_instance_marker(&child, actual_id).await;
        std::fs::write(&pid_file, format!("{}\n", child.id().expect("child pid")))
            .expect("write pid");

        let error = terminate_recorded_process(expected_id, &pid_file, "test")
            .await
            .expect_err("mismatched process must be retained");

        assert!(error.to_string().contains("does not match"));
        assert!(child.try_wait().expect("child status").is_none());
        child.start_kill().expect("kill child");
        child.wait().await.expect("wait child");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphan_cleanup_accepts_pre_spawn_handoff_without_pid() {
        let temp = tempfile::tempdir().expect("temp");
        prepare_pid_handoff(&temp.path().join("backend.pid")).expect("prepare handoff");
        cleanup_process_run_dir(Uuid::new_v4(), temp.path(), "test")
            .await
            .expect("an unlocked empty handoff proves the backend was not started");
        assert!(stopped_marker(temp.path()).is_file());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphan_cleanup_rejects_missing_pid_handoff() {
        let temp = tempfile::tempdir().expect("temp");
        let error = cleanup_process_run_dir(Uuid::new_v4(), temp.path(), "test")
            .await
            .expect_err("missing handoff cannot prove backend ownership");

        assert!(error.to_string().contains("missing PID handoff"));
        assert!(!stopped_marker(temp.path()).exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphan_cleanup_retains_an_active_pid_handoff() {
        let temp = tempfile::tempdir().expect("temp");
        let pid_file = temp.path().join("backend.pid");
        prepare_pid_handoff(&pid_file).expect("prepare handoff");
        let mut command = Command::new("sleep");
        let handoff = configure_pid_handoff(&mut command, &pid_file).expect("configure handoff");

        let error = cleanup_process_run_dir(Uuid::new_v4(), temp.path(), "test")
            .await
            .expect_err("an active handoff cannot prove the backend absent");

        assert!(
            error
                .to_string()
                .contains("PID handoff is still in progress")
        );
        assert!(!stopped_marker(temp.path()).exists());
        drop(handoff);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pid_handoff_is_visible_when_spawn_returns() {
        let temp = tempfile::tempdir().expect("temp");
        let instance_id = Uuid::new_v4();
        let pid_file = temp.path().join("backend.pid");
        prepare_pid_handoff(&pid_file).expect("prepare handoff");
        let mut command = Command::new("sleep");
        command
            .arg("60")
            .env("BLAZE_INSTANCE_ID", instance_id.to_string());
        let handoff = configure_pid_handoff(&mut command, &pid_file).expect("configure handoff");
        let mut child = command.spawn().expect("spawn child");
        drop(handoff);
        wait_for_instance_marker(&child, instance_id).await;

        assert_eq!(
            std::fs::read_to_string(&pid_file)
                .expect("pid handoff")
                .trim(),
            child.id().expect("child pid").to_string()
        );
        terminate_recorded_process(instance_id, &pid_file, "test")
            .await
            .expect("terminate handed-off process");
        child.wait().await.expect("reap child");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn failed_pid_handoff_preparation_does_not_start_backend() {
        let temp = tempfile::tempdir().expect("temp");
        let instance_id = Uuid::new_v4();
        let pid_file = temp.path().join("missing").join("backend.pid");
        let mut command = Command::new("sleep");
        command
            .arg("60")
            .env("BLAZE_INSTANCE_ID", instance_id.to_string());

        assert!(configure_pid_handoff(&mut command, &pid_file).is_err());
        assert!(!pid_file.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn orphan_cleanup_terminates_matching_instance() {
        let temp = tempfile::tempdir().expect("temp");
        let instance_id = Uuid::new_v4();
        let pid_file = temp.path().join("backend.pid");
        let mut child = Command::new("sleep")
            .arg("60")
            .env("BLAZE_INSTANCE_ID", instance_id.to_string())
            .spawn()
            .expect("spawn child");
        wait_for_instance_marker(&child, instance_id).await;
        std::fs::write(&pid_file, format!("{}\n", child.id().expect("child pid")))
            .expect("write pid");

        terminate_recorded_process(instance_id, &pid_file, "test")
            .await
            .expect("matching process is terminated");
        child.wait().await.expect("reap child");
    }
}
