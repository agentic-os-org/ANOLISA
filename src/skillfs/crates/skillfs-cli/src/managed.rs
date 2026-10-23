//! Managed mount supervisor for SkillFS.
//!
//! `skillfs mount --managed <SOURCE> <MOUNTPOINT>` keeps the mount desired
//! state as "mounted" until an explicit `skillfs stop <MOUNTPOINT>` clears
//! it. This survives the caller's process group being torn down (for
//! example when the OpenClaw gateway that launched SkillFS restarts).
//!
//! Three roles share the `skillfs` binary:
//!
//! * **client** — `skillfs mount --managed ...`: writes managed state,
//!   spawns a detached supervisor in its own session (`setsid`), waits for
//!   the mount to become ready, then returns.
//! * **supervisor** — `skillfs supervise --instance <id>` (hidden): runs the
//!   foreground FUSE worker and remounts it after a bounded backoff whenever
//!   it exits while the desired state is still "mounted". A `kill -9`ed worker
//!   leaves a dead FUSE endpoint (mounted but `ENOTCONN`); the supervisor
//!   detects and unmounts that stale endpoint before starting a replacement.
//! * **worker** — `skillfs mount --foreground ...`: the ordinary foreground
//!   mount path, unchanged.
//!
//! State lives under `$XDG_RUNTIME_DIR/skillfs/` (or `/run/user/<uid>/skillfs/`,
//! falling back to `/tmp/skillfs-<uid>/`). The instance id is derived from the
//! canonical mountpoint so `mount` and `stop` agree on the same instance.

use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Schema version for the on-disk managed state file.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// Initial remount backoff after an unexpected worker exit.
const INITIAL_BACKOFF_MS: u64 = 200;
/// Upper bound on the remount backoff.
const MAX_BACKOFF_MS: u64 = 5_000;
/// A worker that ran at least this long is treated as a healthy mount whose
/// later exit is not part of a crash loop, so the backoff resets.
const STABLE_RUN_SECS: u64 = 10;
/// Maximum consecutive sub-`STABLE_RUN_SECS` worker failures before the
/// supervisor gives up remounting. Bounds a crash loop caused by a persistently
/// failing worker (bad config, FUSE unavailable, EBUSY, ...).
const MAX_FAST_FAILURES: u32 = 5;
/// Poll interval while waiting on the worker child.
const WORKER_POLL_MS: u64 = 200;
/// How long the client waits for the mount to become ready before failing.
///
/// The worker pays the full mount startup cost before the FUSE session
/// serves: the 24-command environment probe (`EnvironmentProfile::detect`
/// spawns one `which` per whitelisted command) plus store discovery and the
/// session start. On a host with slow process spawns (WSL, network home
/// directories) that probe alone was measured at ~10 s, so a 10 s budget
/// tore down a worker that was still probing — the readiness failure
/// surfaced an empty worker log tail, the classic still-starting shape.
/// 30 s keeps the same slow-startup regime inside the budget while the
/// failure path (supervisor died, worker crashed) still reports quickly.
const READY_TIMEOUT_MS: u64 = 30_000;
/// How long `stop` waits for processes to exit / the mount to disappear.
const STOP_TIMEOUT_MS: u64 = 10_000;

/// Desired lifecycle state for a managed mount.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DesiredState {
    /// The supervisor should keep the mount alive, remounting on exit.
    Mounted,
    /// An explicit stop cleared the desired state; do not remount.
    Stopped,
}

/// Validated config-mode roots and effective behavior fixed at managed start.
/// Absence in old state retains positional-mode request matching.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManagedMountConfig {
    /// Canonical roots, deduplicated in precedence order.
    pub sources: Vec<PathBuf>,
    /// Effective permission for users other than the mounter.
    pub allow_other: bool,
    /// Effective read-only setting, including the multi-source requirement.
    pub read_only: bool,
}

/// On-disk record describing a managed mount instance.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ManagedState {
    pub schema_version: u32,
    pub instance_id: String,
    pub mountpoint: String,
    pub source: String,
    /// Config-mode identity; absent for legacy positional managed mounts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_config: Option<ManagedMountConfig>,
    pub worker_program: String,
    pub worker_args: Vec<String>,
    pub desired_state: DesiredState,
}

/// Sequence that makes every staging path unique inside this process.
static STATE_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// How many staging names one save may try before giving up. A name can only
/// be taken by an entry stranded by a crashed save under the same pid.
const MAX_STAGING_ATTEMPTS: usize = 16;

/// Create the exclusive staging file for one state save, next to `path`.
///
/// The name carries the target's file name plus the pid (separating
/// processes) and a fresh sequence number from `counter` (separating saves
/// inside one process, threads included), so no two saves ever share a path
/// and no call can unlink, write through, or publish another call's staging
/// entry. `create_new` refuses to write through an entry already sitting at
/// a candidate name — a planted symlink included — and the next name is used
/// instead; only the writer that created a staging file may remove it.
///
/// `counter` is a parameter (and production passes the process-global
/// [`STATE_STAGING_SEQUENCE`]) so the security tests can inject a private
/// counter and know the exact candidate names their save will try,
/// independently of what other tests running in parallel do to the global
/// one — a pre-planted entry is then guaranteed to be hit, not merely
/// likely.
fn create_staging_file(
    path: &Path,
    counter: &AtomicU64,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "state path has no parent directory to stage in",
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "state path has no file name to stage under",
        )
    })?;
    let prefix = file_name.to_string_lossy();
    for _ in 0..MAX_STAGING_ATTEMPTS {
        let candidate = dir.join(format!(
            ".{prefix}.{}.{}.tmp",
            std::process::id(),
            counter.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "every candidate staging path for the managed state file is taken",
    ))
}

impl ManagedState {
    fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let raw = std::fs::read_to_string(path)?;
        let state: ManagedState = serde_json::from_str(&raw)?;
        Ok(state)
    }

    fn save(&self, path: &Path) -> Result<(), Box<dyn Error>> {
        self.save_with_counter(path, &STATE_STAGING_SEQUENCE)
    }

    /// [`Self::save`] with the staging-sequence source injected. Production
    /// always saves through [`Self::save`] and the process-global counter;
    /// tests pass a private counter so the candidate names their save will
    /// try are fully deterministic under default parallel `cargo test`.
    fn save_with_counter(&self, path: &Path, counter: &AtomicU64) -> Result<(), Box<dyn Error>> {
        let raw = serde_json::to_string_pretty(self)?;
        write_staged(path, &raw, counter)
    }
}

/// Publish a complete state or resolved mount file using exclusive staging.
fn write_staged(path: &Path, raw: &str, counter: &AtomicU64) -> Result<(), Box<dyn Error>> {
    let (tmp, mut file) = create_staging_file(path, counter)?;
    let result = (|| {
        use std::io::Write;
        file.write_all(raw.as_bytes())?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        // Remove only the staging entry created by this call.
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(Into::into)
}

fn save_mount_config(
    config: &ManagedMountConfig,
    mountpoint: &Path,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let file = super::mount_file::MountFile {
        mountpoint: mountpoint.to_path_buf(),
        sources: config.sources.clone(),
    };
    write_staged(path, &toml::to_string(&file)?, &STATE_STAGING_SEQUENCE)
}

/// Filesystem paths for a managed mount instance.
pub struct ManagedPaths {
    pub state: PathBuf,
    pub supervisor_pid: PathBuf,
    pub worker_pid: PathBuf,
    pub supervisor_log: PathBuf,
    pub worker_log: PathBuf,
    /// Owned resolved mount specification replayed by config-mode workers.
    pub mount_config: PathBuf,
}

impl ManagedPaths {
    fn new(instance_id: &str) -> Self {
        let dir = runtime_dir();
        ManagedPaths {
            state: dir.join(format!("{instance_id}.state.json")),
            supervisor_pid: dir.join(format!("{instance_id}.supervisor.pid")),
            worker_pid: dir.join(format!("{instance_id}.worker.pid")),
            supervisor_log: dir.join(format!("{instance_id}.supervisor.log")),
            worker_log: dir.join(format!("{instance_id}.worker.log")),
            mount_config: dir.join(format!("{instance_id}.mount.toml")),
        }
    }
}

/// Resolve the managed-state runtime directory.
///
/// Prefers `$XDG_RUNTIME_DIR/skillfs`, then `/run/user/<uid>/skillfs`, and
/// falls back to `/tmp/skillfs-<uid>` only if neither is usable.
pub fn runtime_dir() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    if let Some(base) = std::env::var_os("XDG_RUNTIME_DIR") {
        let base = PathBuf::from(base);
        if !base.as_os_str().is_empty() {
            return base.join("skillfs");
        }
    }
    let run_user = PathBuf::from(format!("/run/user/{uid}"));
    if run_user.is_dir() {
        return run_user.join("skillfs");
    }
    PathBuf::from(format!("/tmp/skillfs-{uid}"))
}

/// Resolve and secure the managed-state runtime directory.
///
/// The state and pid files under this directory drive process signaling and
/// remount behavior, so the directory must be private to the current user.
/// The directory is created `0700` if missing; if it already exists it must be
/// a real directory (not a symlink) owned by the current uid, and any
/// group/other permission bits are stripped. This matters most for the
/// `/tmp/skillfs-<uid>` fallback, where a hostile actor could pre-create the
/// path.
pub fn secure_runtime_dir() -> Result<PathBuf, Box<dyn Error>> {
    let dir = runtime_dir();
    secure_dir(&dir)?;
    Ok(dir)
}

/// Create `dir` `0700` if missing, or validate + tighten it if it exists.
fn secure_dir(dir: &Path) -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let uid = unsafe { libc::getuid() };

    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing to use runtime dir '{}': it is a symlink",
                    dir.display()
                )
                .into());
            }
            if !meta.is_dir() {
                return Err(format!(
                    "runtime path '{}' exists but is not a directory",
                    dir.display()
                )
                .into());
            }
            if meta.uid() != uid {
                return Err(format!(
                    "refusing to use runtime dir '{}': owned by uid {}, expected {}",
                    dir.display(),
                    meta.uid(),
                    uid
                )
                .into());
            }
            // Strip any group/other access bits so pid/state files stay private.
            if meta.mode() & 0o077 != 0 {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(
                    |e| format!("failed to tighten runtime dir '{}': {e}", dir.display()),
                )?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| format!("failed to create runtime dir '{}': {e}", dir.display()))?;
        }
        Err(e) => {
            return Err(format!("failed to inspect runtime dir '{}': {e}", dir.display()).into());
        }
    }
    Ok(())
}

/// Normalize a mountpoint path for identity purposes: canonicalize if it
/// exists, otherwise fall back to an absolute (lexical) path so `mount` and
/// `stop` derive the same instance id before/after the directory exists.
pub fn normalize_mountpoint(mountpoint: &Path) -> PathBuf {
    if let Ok(canon) = mountpoint.canonicalize() {
        return canon;
    }
    std::path::absolute(mountpoint).unwrap_or_else(|_| mountpoint.to_path_buf())
}

/// Stable, collision-resistant instance id derived from a normalized path.
///
/// Combines a sanitized basename (for human readability) with a hash of the
/// full path (for uniqueness). The id must never start with a dash: it is
/// passed as the value of `--instance` (a leading `-` makes the argument
/// parser read it as an option, so the supervisor dies before writing its
/// pid file) and it prefixes every runtime file name. A dot-prefixed
/// mountpoint — the shape every test harness's tempdir has — sanitizes to a
/// leading dash, so leading dashes are stripped before the readable prefix
/// is cut.
///
/// Only dashes are stripped. Every other id shape the sanitizer can emit —
/// in particular a leading underscore — was already a valid `--instance`
/// value and file-name prefix, so a live supervisor may be sitting on those
/// exact runtime files when the binary is upgraded. Stripping a leading
/// underscore too would silently rename such an instance (`/mnt/_skills`
/// used `_skills-<hash>`): `stop` would miss the incumbent's state and pid
/// files and report success while the old supervisor kept running — free to
/// remount after any later unmount — and a new `mount --managed` would race
/// a competing supervisor for the mountpoint. Preserving every non-dash
/// prefix keeps pre-existing ids byte-identical, and dash-leading ids never
/// worked (the supervisor died in argument parsing, before writing any
/// state), so there is nothing to migrate.
pub fn instance_id_for(normalized: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    normalized.as_os_str().hash(&mut hasher);
    let hash = hasher.finish();

    let base: String = normalized
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .skip_while(|c| *c == '-')
        .take(24)
        .collect();
    let base = if base.is_empty() {
        "root".to_string()
    } else {
        base
    };
    format!("{base}-{hash:016x}")
}

/// Build the worker argument vector from the client's raw arguments.
///
/// The worker runs the ordinary foreground mount path, so we drop `--managed`
/// and ensure `--foreground` is present. `raw_args` excludes the program name.
pub fn build_worker_args(raw_args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = raw_args
        .iter()
        .filter(|a| a.as_str() != "--managed")
        .cloned()
        .collect();
    if !out.iter().any(|a| a == "--foreground") {
        // Insert right after the `mount` subcommand token so it lands in the
        // subcommand's argument list rather than being read as a global.
        if let Some(pos) = out.iter().position(|a| a == "mount") {
            out.insert(pos + 1, "--foreground".to_string());
        } else {
            out.insert(0, "--foreground".to_string());
        }
    }
    out
}

/// Replay the frozen mount file and effective options, preserving other args.
fn build_config_worker_args(
    raw_args: &[String],
    config: &ManagedMountConfig,
    path: &Path,
) -> Result<Vec<String>, Box<dyn Error>> {
    let path = path
        .to_str()
        .ok_or("managed mount config path is not valid UTF-8")?;
    let mut out = build_worker_args(raw_args);
    let config_index = out
        .iter()
        .position(|arg| arg == "--config" || arg.starts_with("--config="))
        .ok_or("managed mount configuration requires --config")?;
    if out[config_index] == "--config" {
        let value = out
            .get_mut(config_index + 1)
            .ok_or("managed --config requires a path")?;
        *value = path.to_string();
    } else {
        out[config_index] = format!("--config={path}");
    }
    for (enabled, flag) in [
        (config.allow_other, "--allow-other"),
        (config.read_only, "--read-only"),
    ] {
        if enabled && !out.iter().any(|arg| arg == flag) {
            let option_end = out.iter().position(|arg| arg == "--").unwrap_or(out.len());
            out.insert(option_end, flag.to_string());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Process / mount helpers
// ---------------------------------------------------------------------------

fn pid_alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

fn send_signal(pid: i32, sig: i32) {
    if pid > 0 {
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

fn read_pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

fn write_pid(path: &Path, pid: u32) -> Result<(), Box<dyn Error>> {
    std::fs::write(path, format!("{pid}\n"))?;
    Ok(())
}

/// Observed state of a managed mountpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountState {
    /// Present in `/proc/mounts` and a minimal access succeeds.
    Ready,
    /// Not present in `/proc/mounts`.
    NotMounted,
    /// Present in `/proc/mounts` but access fails with `ENOTCONN`
    /// ("Transport endpoint is not connected") — a dead FUSE endpoint, e.g.
    /// after the worker was `kill -9`ed.
    Stale,
    /// Present in `/proc/mounts` but access fails for some other reason.
    UnknownError,
}

/// How long to keep retrying unmount of a stale endpoint before giving up.
const UNMOUNT_TIMEOUT_MS: u64 = 3_000;

/// Whether the mountpoint currently appears in `/proc/mounts`. Matching is
/// byte-exact against the escape-decoded mount field (see
/// [`skillfs_fuse::proc_mounts`]): a lossy UTF-8 view would conflate a
/// mounted invalid-byte path with a different queried U+FFFD path.
pub fn is_mounted(mountpoint: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let mounts = std::fs::read("/proc/mounts").unwrap_or_default();
    skillfs_fuse::proc_mounts::mounts_contain_target(&mounts, mountpoint.as_os_str().as_bytes())
}

/// Classify the mountpoint: distinguish a healthy mount from a dead FUSE
/// endpoint so recovery can unmount stale mounts before remounting.
pub fn classify_mount(mountpoint: &Path) -> MountState {
    if !is_mounted(mountpoint) {
        return MountState::NotMounted;
    }
    // Minimal access. A dead FUSE endpoint fails opendir with ENOTCONN.
    match std::fs::read_dir(mountpoint) {
        Ok(_) => MountState::Ready,
        Err(e) if e.raw_os_error() == Some(libc::ENOTCONN) => MountState::Stale,
        Err(_) => MountState::UnknownError,
    }
}

/// Readiness = mounted and a minimal readdir succeeds.
fn is_mount_ready(mountpoint: &Path) -> bool {
    classify_mount(mountpoint) == MountState::Ready
}

/// Attempt a single unmount pass: `fusermount3 -u`, then `umount` as a
/// fallback. Returns `true` if the mountpoint is gone afterward.
///
/// The mountpoint is passed as the raw OS string, matching the byte-exact
/// `is_mounted` probe: `to_string_lossy()` would turn an invalid-byte
/// mountpoint into a different (nonexistent) path, so fusermount3 would
/// fail and only the `umount` fallback would address the real mount.
fn unmount_once(mountpoint: &Path) -> bool {
    let _ = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(mountpoint.as_os_str())
        .output();
    if !is_mounted(mountpoint) {
        return true;
    }
    let _ = std::process::Command::new("umount")
        .arg(mountpoint.as_os_str())
        .output();
    !is_mounted(mountpoint)
}

/// Unmount a (possibly stale/dead) FUSE endpoint, retrying within a bounded
/// window. Succeeds immediately if nothing is mounted. Errors only if the
/// endpoint is still present after both `fusermount3 -u` and `umount` over the
/// retry window.
fn clear_mount(mountpoint: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_millis(UNMOUNT_TIMEOUT_MS);
    loop {
        if !is_mounted(mountpoint) {
            return Ok(());
        }
        if unmount_once(mountpoint) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "failed to unmount '{}' via fusermount3 and umount",
                mountpoint.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Client: `skillfs mount --managed ...`
// ---------------------------------------------------------------------------

/// Whether the recorded state of a live managed instance matches the
/// requested mountpoint, input mode, roots and config-mode effective options.
///
/// Pure so the refusal can be tested without a live supervisor.
fn active_instance_matches(
    state: &ManagedState,
    source_norm: &Path,
    mountpoint_norm: &Path,
    mount_config: Option<&ManagedMountConfig>,
) -> bool {
    state.source == source_norm.to_string_lossy()
        && state.mountpoint == mountpoint_norm.to_string_lossy()
        && state.mount_config.as_ref() == mount_config
}

/// Entry point for a managed mount request. Validates the source, writes the
/// managed state, spawns a detached supervisor, and waits for readiness.
pub fn run_client(
    raw_args: &[String],
    source: &Path,
    mountpoint: &Path,
    mount_config: Option<ManagedMountConfig>,
) -> Result<(), Box<dyn Error>> {
    // Fast, client-side validation so operators get an immediate error
    // instead of a readiness timeout when the source is wrong. Deeper
    // validation still happens in the worker.
    if !source.exists() {
        return Err(format!("Source directory does not exist: {}", source.display()).into());
    }
    if !source.is_dir() {
        return Err(format!("Source is not a directory: {}", source.display()).into());
    }

    // Create the mountpoint up front (mirrors the compat-mode mount UX) so it
    // can be canonicalized into a stable instance id.
    if !mountpoint.exists() {
        std::fs::create_dir_all(mountpoint).map_err(|e| {
            format!(
                "failed to create mount point '{}': {e}",
                mountpoint.display()
            )
        })?;
    }

    let normalized = normalize_mountpoint(mountpoint);
    let source_norm = normalize_mountpoint(source);
    let instance_id = instance_id_for(&normalized);
    let paths = ManagedPaths::new(&instance_id);

    // Create/validate the private runtime dir before reading or writing any
    // pid/state files it holds.
    secure_runtime_dir()?;

    // A live supervisor PID owns this instance, even if the mount is
    // momentarily down during remount/backoff recovery. Never spawn a second
    // supervisor for the same instance: it would overwrite state and race the
    // incumbent over the mountpoint.
    if let Some(pid) = read_pid(&paths.supervisor_pid) {
        if pid_alive(pid) {
            // The instance id is derived from the mountpoint, so a second
            // `--managed` request for the same mountpoint but a different
            // source resolves to this instance. Reporting it as "already
            // active" would leave the caller believing the new source is
            // served; refuse instead.
            //
            // The recorded state is the only proof of what the incumbent
            // serves, so a missing or corrupt state file must not be
            // skipped either: fail explicitly and leave the incumbent
            // running rather than vouching for an unverified source.
            let active = ManagedState::load(&paths.state).map_err(|e| {
                format!(
                    "managed supervisor (pid {pid}) is active for {} but its state \
                     cannot be read ({e}); run `skillfs stop {}` first",
                    normalized.display(),
                    normalized.display()
                )
            })?;
            if !active_instance_matches(&active, &source_norm, &normalized, mount_config.as_ref()) {
                return Err(format!(
                    "managed mount '{}' is already active from source '{}' (requested '{}'); \
                     source order, input mode, or effective options may differ; \
                     run `skillfs stop {}` first",
                    active.mountpoint,
                    active.source,
                    source_norm.display(),
                    normalized.display()
                )
                .into());
            }
            if is_mount_ready(&normalized) {
                info!(
                    mountpoint = %normalized.display(),
                    supervisor_pid = pid,
                    "managed mount already active; nothing to do"
                );
                println!(
                    "skillfs: managed mount already active at {}",
                    normalized.display()
                );
                return Ok(());
            }
            // Supervisor alive but mount not ready yet — wait for it to
            // converge rather than starting a competing supervisor.
            let deadline = Instant::now() + Duration::from_millis(READY_TIMEOUT_MS);
            while Instant::now() < deadline {
                if !pid_alive(pid) {
                    break; // incumbent died; fall through to (re)start below
                }
                if is_mount_ready(&normalized) {
                    println!(
                        "skillfs: managed mount ready at {} (stop with: skillfs stop {})",
                        normalized.display(),
                        normalized.display()
                    );
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if pid_alive(pid) {
                return Err(format!(
                    "managed supervisor (pid {pid}) is active for {} but the mount is not ready; \
                     run 'skillfs stop {}' to recover before remounting",
                    normalized.display(),
                    normalized.display()
                )
                .into());
            }
            // Incumbent supervisor exited while we waited — safe to start fresh.
        }
    }

    // A killed supervisor can leave behind an orphan worker that still serves
    // the mount. Do not start a competing worker over it: terminate the orphan
    // and clear the mountpoint, then create a fresh supervised pair below.
    if let Some(pid) = read_pid(&paths.worker_pid) {
        if pid_alive(pid) {
            warn!(
                worker_pid = pid,
                mountpoint = %normalized.display(),
                "found orphan managed worker without a live supervisor; replacing it"
            );
        }
    }
    teardown_instance(&paths, &normalized)?;

    let worker_program = std::env::current_exe()
        .map_err(|e| format!("failed to resolve current executable: {e}"))?;
    let worker_args = match &mount_config {
        Some(config) => build_config_worker_args(raw_args, config, &paths.mount_config)?,
        None => build_worker_args(raw_args),
    };

    if let Some(config) = &mount_config {
        save_mount_config(config, &normalized, &paths.mount_config)?;
    }

    let state = ManagedState {
        schema_version: STATE_SCHEMA_VERSION,
        instance_id: instance_id.clone(),
        mountpoint: normalized.to_string_lossy().to_string(),
        source: source_norm.to_string_lossy().to_string(),
        mount_config,
        worker_program: worker_program.to_string_lossy().to_string(),
        worker_args,
        desired_state: DesiredState::Mounted,
    };
    if let Err(error) = state.save(&paths.state) {
        let _ = std::fs::remove_file(&paths.mount_config);
        return Err(error);
    }

    if let Err(error) = spawn_supervisor(&worker_program, &instance_id, &paths) {
        let _ = std::fs::remove_file(&paths.state);
        let _ = std::fs::remove_file(&paths.mount_config);
        return Err(error);
    }

    // Wait for readiness.
    let deadline = Instant::now() + Duration::from_millis(READY_TIMEOUT_MS);
    loop {
        if is_mount_ready(&normalized) {
            info!(
                mountpoint = %normalized.display(),
                instance = %instance_id,
                "managed mount ready"
            );
            println!(
                "skillfs: managed mount ready at {} (stop with: skillfs stop {})",
                normalized.display(),
                normalized.display()
            );
            return Ok(());
        }
        // If the supervisor died before the mount came up, surface the
        // worker log so the failure is diagnosable.
        if let Some(pid) = read_pid(&paths.supervisor_pid) {
            if !pid_alive(pid) {
                break;
            }
        }
        // State is published before spawning the supervisor, so both files
        // disappearing means cleanup, not the initial window before PID write.
        if matches!(paths.supervisor_pid.try_exists(), Ok(false))
            && matches!(paths.state.try_exists(), Ok(false))
        {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Readiness failed (timeout, or the supervisor died before the mount came
    // up). Capture the worker log before cleanup, then tear down the supervisor
    // we just spawned so it does not keep retrying in the background after we
    // return an error.
    let worker_log = std::fs::read_to_string(&paths.worker_log).unwrap_or_default();
    let mut tail_lines: Vec<&str> = worker_log.lines().rev().take(20).collect();
    tail_lines.reverse();
    let tail = tail_lines.join("\n");

    if let Err(e) = teardown_instance(&paths, &normalized) {
        warn!(error = %e, "failed to clean up after managed mount startup failure");
    }

    Err(format!(
        "managed mount did not become ready within {}ms; supervisor stopped.\n\
         --- worker log tail ---\n{}",
        READY_TIMEOUT_MS, tail
    )
    .into())
}

/// Spawn the supervisor detached in its own session so a restart of the
/// caller's process group does not tear it down.
fn spawn_supervisor(
    program: &Path,
    instance_id: &str,
    paths: &ManagedPaths,
) -> Result<(), Box<dyn Error>> {
    use std::os::unix::process::CommandExt;

    let sup_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.supervisor_log)
        .map_err(|e| {
            format!(
                "failed to open supervisor log '{}': {e}",
                paths.supervisor_log.display()
            )
        })?;
    let sup_log_err = sup_log.try_clone()?;

    let mut cmd = std::process::Command::new(program);
    cmd.arg("supervise")
        .arg("--instance")
        .arg(instance_id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(sup_log))
        .stderr(std::process::Stdio::from(sup_log_err));

    // Detach: new session + new process group, no controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn supervisor: {e}"))?;
    info!(
        supervisor_pid = child.id(),
        instance = %instance_id,
        "spawned detached managed supervisor"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Supervisor: `skillfs supervise --instance <id>`
// ---------------------------------------------------------------------------

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_term(_sig: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn install_term_handler() {
    let handler = handle_term as extern "C" fn(i32) as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
}

/// Entry point for the detached supervisor process.
pub fn run_supervisor(instance_id: &str) -> Result<(), Box<dyn Error>> {
    let paths = ManagedPaths::new(instance_id);
    secure_runtime_dir()?;
    let state = ManagedState::load(&paths.state)
        .map_err(|e| format!("failed to load managed state for '{instance_id}': {e}"))?;
    let mountpoint = PathBuf::from(&state.mountpoint);

    write_pid(&paths.supervisor_pid, std::process::id())
        .inspect_err(|_| finish(&paths, &mountpoint))?;
    install_term_handler();
    info!(
        instance = %instance_id,
        mountpoint = %mountpoint.display(),
        "managed supervisor started"
    );

    let mut backoff_ms = INITIAL_BACKOFF_MS;
    let mut fast_failures: u32 = 0;

    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }
        if current_desired_state(&paths.state) == DesiredState::Stopped {
            break;
        }

        let worker_out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.worker_log)
            .map_err(|e| format!("failed to open worker log: {e}"))
            .inspect_err(|_| finish(&paths, &mountpoint))?;
        let worker_err = worker_out
            .try_clone()
            .inspect_err(|_| finish(&paths, &mountpoint))?;

        let start = Instant::now();
        let mut child = std::process::Command::new(&state.worker_program)
            .args(&state.worker_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(worker_out))
            .stderr(std::process::Stdio::from(worker_err))
            .spawn()
            .map_err(|e| format!("failed to spawn worker: {e}"))
            .inspect_err(|_| finish(&paths, &mountpoint))?;
        let worker_pid = child.id();
        // teardown_instance uses this file to signal the worker directly when
        // the supervisor is gone. A missing file loses only that fallback:
        // clear_mount() in teardown still unmounts, and a successful unmount
        // makes the FUSE loop return and the worker exit — so the worker is
        // orphaned only when unmount also fails or the worker is stuck. The
        // warn makes the lost fallback visible for diagnosing that case.
        if let Err(error) = write_pid(&paths.worker_pid, worker_pid) {
            warn!(
                error = %error,
                "failed to write worker pid file; stop cannot signal this worker directly if the supervisor dies"
            );
        }
        info!(worker_pid, "managed worker started");

        // Wait for the worker, watching for shutdown.
        loop {
            if SHUTDOWN.load(Ordering::SeqCst)
                || current_desired_state(&paths.state) == DesiredState::Stopped
            {
                info!(
                    worker_pid,
                    "stopping managed worker (desired state cleared)"
                );
                send_signal(worker_pid as i32, libc::SIGTERM);
                if let Err(e) = wait_for_shutdown(&mountpoint, || {
                    child.try_wait().map(|status| status.is_some())
                }) {
                    warn!(error = %e, "managed worker shutdown failed");
                }
                if matches!(child.try_wait(), Ok(None)) {
                    warn!(worker_pid, "managed worker shutdown timed out; forcing");
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let _ = std::fs::remove_file(&paths.worker_pid);
                finish(&paths, &mountpoint);
                return Ok(());
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(worker_pid, ?status, "managed worker exited");
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(WORKER_POLL_MS)),
                Err(e) => {
                    warn!(error = %e, "error waiting on managed worker");
                    break;
                }
            }
        }
        let _ = std::fs::remove_file(&paths.worker_pid);

        // Worker exited on its own. Decide whether to remount.
        if SHUTDOWN.load(Ordering::SeqCst)
            || current_desired_state(&paths.state) == DesiredState::Stopped
        {
            break;
        }

        // A `kill -9`ed worker leaves a dead FUSE endpoint behind: the
        // mountpoint is still listed in /proc/mounts but every access returns
        // ENOTCONN ("Transport endpoint is not connected"). The replacement
        // worker cannot mount over it, so unmount the stale endpoint before
        // remounting.
        match classify_mount(&mountpoint) {
            MountState::Stale | MountState::UnknownError => {
                warn!(
                    mountpoint = %mountpoint.display(),
                    "detected stale FUSE mount after worker exit; clearing before remount"
                );
                if let Err(e) = clear_mount(&mountpoint) {
                    warn!(
                        error = %e,
                        "failed to clear stale mount; remount may fail this cycle"
                    );
                }
            }
            MountState::Ready | MountState::NotMounted => {}
        }

        if start.elapsed() >= Duration::from_secs(STABLE_RUN_SECS) {
            // The worker ran long enough to be a healthy mount; this exit is not
            // part of a crash loop, so reset both the backoff and the counter.
            backoff_ms = INITIAL_BACKOFF_MS;
            fast_failures = 0;
        } else {
            fast_failures += 1;
            if fast_failures >= MAX_FAST_FAILURES {
                warn!(
                    fast_failures,
                    stable_run_secs = STABLE_RUN_SECS,
                    mountpoint = %mountpoint.display(),
                    "managed worker keeps failing fast; giving up crash-loop remount"
                );
                // Best-effort persistence of the stopped marker. A failure
                // here is not itself a resurrection: finish() below deletes
                // the state file, and current_desired_state() treats a
                // missing or unreadable state as Stopped, so the common
                // path still converges. The log makes the swallowed error
                // visible so operators can see when the marker was not
                // written; only a stale pre-Mounted state that survives
                // cleanup could mislead a later start.
                match ManagedState::load(&paths.state) {
                    Ok(mut state) => {
                        state.desired_state = DesiredState::Stopped;
                        if let Err(error) = state.save(&paths.state) {
                            warn!(
                                error = %error,
                                "failed to persist stopped marker; the state file may be stale"
                            );
                        }
                    }
                    Err(error) => {
                        warn!(
                            error = %error,
                            "failed to load state for stopped marker; treating as stopped"
                        );
                    }
                }
                break;
            }
        }
        info!(
            backoff_ms,
            fast_failures, "managed worker exited unexpectedly; remounting after backoff"
        );
        // Sleep in small slices so shutdown is responsive during backoff.
        let backoff_deadline = Instant::now() + Duration::from_millis(backoff_ms);
        while Instant::now() < backoff_deadline {
            if SHUTDOWN.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        backoff_ms = (backoff_ms.saturating_mul(2)).min(MAX_BACKOFF_MS);
    }

    // No worker is alive on this path (shutdown during backoff or crash-loop
    // exhaustion), so only a stale endpoint can need cleanup.
    if let Err(e) = clear_mount(&mountpoint) {
        warn!(error = %e, "failed to unmount during supervisor cleanup");
    }
    finish(&paths, &mountpoint);
    Ok(())
}

fn current_desired_state(state_path: &Path) -> DesiredState {
    // A missing/unreadable state file means the instance was torn down; treat
    // it as stopped so the supervisor exits rather than remounting forever.
    ManagedState::load(state_path)
        .map(|s| s.desired_state)
        .unwrap_or(DesiredState::Stopped)
}

// Keep retrying while the worker owns backing cleanup. After a successful
// unmount, wait only for its exit; never unmount after forcing that owner out.
fn wait_for_shutdown(
    mountpoint: &Path,
    mut exited: impl FnMut() -> std::io::Result<bool>,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_millis(STOP_TIMEOUT_MS);
    let mut unmounted = false;
    loop {
        let processes_gone = exited()?;
        if !unmounted {
            unmounted = !is_mounted(mountpoint) || unmount_once(mountpoint);
        }
        if (unmounted && processes_gone) || Instant::now() >= deadline {
            return if unmounted {
                Ok(())
            } else {
                Err(format!(
                    "failed to unmount '{}' via fusermount3 and umount",
                    mountpoint.display()
                )
                .into())
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Remove runtime files only when config-mode unmount is confirmed.
/// The mount-table read is explicit so unreadable tables cannot mean absence.
fn remove_instance_files(
    paths: &ManagedPaths,
    mountpoint: &Path,
    mounts: std::io::Result<Vec<u8>>,
) -> Result<(), Box<dyn Error>> {
    use std::os::unix::ffi::OsStrExt;
    let state = ManagedState::load(&paths.state).ok();
    let config_mode = paths.mount_config.try_exists().unwrap_or(true)
        || state
            .as_ref()
            .is_some_and(|state| state.mount_config.is_some());
    if config_mode {
        let unmounted = mounts.map(|mounts| {
            !skillfs_fuse::proc_mounts::mounts_contain_target(
                &mounts,
                mountpoint.as_os_str().as_bytes(),
            )
        });
        if !matches!(unmounted, Ok(true)) {
            // Retain a stopped record and its frozen specification so a later
            // stop can retry; an uncertain mount must never lose its snapshot.
            if let Some(mut state) = state {
                state.desired_state = DesiredState::Stopped;
                let _ = state.save(&paths.state);
            }
            return Err(format!(
                "cannot confirm unmount of '{}'; managed config state retained",
                mountpoint.display()
            )
            .into());
        }
    }
    let _ = std::fs::remove_file(&paths.worker_pid);
    let _ = std::fs::remove_file(&paths.supervisor_pid);
    let _ = std::fs::remove_file(&paths.state);
    let _ = std::fs::remove_file(&paths.mount_config);
    Ok(())
}

/// Remove instance state after worker shutdown or crash-loop exhaustion.
fn finish(paths: &ManagedPaths, mountpoint: &Path) {
    if let Err(error) = remove_instance_files(paths, mountpoint, std::fs::read("/proc/mounts")) {
        warn!(%error, "managed supervisor retained state for a later stop");
    }
    info!(mountpoint = %mountpoint.display(), "managed supervisor exiting");
}

// ---------------------------------------------------------------------------
// Stop: `skillfs stop <MOUNTPOINT>`
// ---------------------------------------------------------------------------

/// Tear down a managed instance: mark it stopped, signal the supervisor
/// (falling back to the worker), unmount, wait for exit, and remove residual
/// pid/state files. Returns the unmount result so callers can surface a cleanup
/// failure; every other step is best-effort.
///
/// Shared by `stop` and by the client's startup-abort path so neither has to
/// duplicate the signal/unmount/wait sequence.
fn teardown_instance(paths: &ManagedPaths, mountpoint: &Path) -> Result<(), Box<dyn Error>> {
    teardown_instance_with_shutdown(
        paths,
        mountpoint,
        |processes_gone| wait_for_shutdown(mountpoint, || Ok(processes_gone())),
        || std::fs::read("/proc/mounts"),
    )
}

// Inject only the shutdown outcome and final mount-table read so tests exercise
// the same stopped marker and cleanup gates without a live FUSE mount.
fn teardown_instance_with_shutdown(
    paths: &ManagedPaths,
    mountpoint: &Path,
    shutdown: impl FnOnce(&dyn Fn() -> bool) -> Result<(), Box<dyn Error>>,
    read_mounts: impl FnOnce() -> std::io::Result<Vec<u8>>,
) -> Result<(), Box<dyn Error>> {
    // Clear desired state first so the supervisor will not remount even if a
    // remount races with our signal.
    if let Ok(mut state) = ManagedState::load(&paths.state) {
        state.desired_state = DesiredState::Stopped;
        let _ = state.save(&paths.state);
    }

    // Prefer signaling the supervisor: it owns the worker and unmounts it
    // cleanly. Fall back to signaling the worker directly.
    let sup_pid = read_pid(&paths.supervisor_pid);
    let worker_pid = read_pid(&paths.worker_pid);

    if let Some(pid) = sup_pid {
        if pid_alive(pid) {
            info!(supervisor_pid = pid, "stopping managed supervisor");
            send_signal(pid, libc::SIGTERM);
        }
    }
    if sup_pid.is_none_or(|p| !pid_alive(p)) {
        if let Some(pid) = worker_pid {
            if pid_alive(pid) {
                info!(worker_pid = pid, "stopping managed worker directly");
                send_signal(pid, libc::SIGTERM);
            }
        }
    }

    let processes_gone =
        || sup_pid.is_none_or(|p| !pid_alive(p)) && worker_pid.is_none_or(|p| !pid_alive(p));
    let unmount_result = shutdown(&processes_gone);
    if !processes_gone() {
        warn!("teardown timed out waiting for clean shutdown; forcing");
        for p in [sup_pid, worker_pid].into_iter().flatten() {
            if pid_alive(p) {
                send_signal(p, libc::SIGKILL);
            }
        }
    }

    // Config state must survive a failed stop, including an unreadable mount
    // table. Positional instances retain their existing best-effort cleanup.
    let config_mode = paths.mount_config.try_exists().unwrap_or(true)
        || ManagedState::load(&paths.state).is_ok_and(|state| state.mount_config.is_some());
    if config_mode && unmount_result.is_err() {
        return unmount_result;
    }
    remove_instance_files(paths, mountpoint, read_mounts())?;

    unmount_result
}

/// Entry point for `skillfs stop`. Clears the desired state, terminates the
/// managed supervisor/worker, and unmounts. Idempotent: tolerates an
/// already-unmounted mount and a missing managed instance.
pub fn run_stop(mountpoint: &Path) -> Result<(), Box<dyn Error>> {
    let normalized = normalize_mountpoint(mountpoint);
    let instance_id = instance_id_for(&normalized);
    let paths = ManagedPaths::new(&instance_id);
    secure_runtime_dir()?;

    let had_state = paths.state.exists();

    // No managed instance recorded for this mountpoint. `stop` is still a
    // reliable teardown, so unmount immediately instead of waiting the full
    // stop timeout on processes that do not exist (handles stale or
    // non-managed dead mounts).
    if !had_state {
        if matches!(paths.mount_config.try_exists(), Ok(false)) {
            // Preserve the legacy no-state PID cleanup even if unmount fails.
            let _ = std::fs::remove_file(&paths.worker_pid);
            let _ = std::fs::remove_file(&paths.supervisor_pid);
        }
        let was_mounted = classify_mount(&normalized) != MountState::NotMounted;
        if was_mounted {
            clear_mount(&normalized)?;
        }
        remove_instance_files(&paths, &normalized, std::fs::read("/proc/mounts"))?;
        if was_mounted {
            println!("skillfs: unmounted {}", normalized.display());
        } else {
            println!(
                "skillfs: no managed mount at {} (already stopped)",
                normalized.display()
            );
        }
        return Ok(());
    }

    teardown_instance(&paths, &normalized)?;
    println!("skillfs: stopped managed mount at {}", normalized.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_is_stable_for_same_path() {
        let p = Path::new("/tmp/skillfs-test-mount");
        let a = instance_id_for(p);
        let b = instance_id_for(p);
        assert_eq!(a, b, "instance id must be deterministic");
    }

    /// A live supervisor pid with a missing or corrupt state file must
    /// fail the request with the explicit state error instead of falling
    /// through to an "already active" success: the state is the only
    /// proof of which source the incumbent serves. (Previously a ready
    /// mount with unreadable state reported success.)
    #[test]
    fn unreadable_incumbent_state_fails_instead_of_already_active() {
        let source = tempfile::tempdir().expect("source tempdir");
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let normalized = normalize_mountpoint(mountpoint.path());
        let paths = ManagedPaths::new(&instance_id_for(&normalized));
        secure_runtime_dir().expect("runtime dir");
        // The test process itself plays the live supervisor: its pid is
        // alive by definition.
        std::fs::write(&paths.supervisor_pid, std::process::id().to_string())
            .expect("write supervisor pid");

        let err = run_client(&[], source.path(), mountpoint.path(), None)
            .expect_err("a missing state file must fail the request");
        let msg = err.to_string();
        assert!(
            msg.contains("state cannot be read") && msg.contains("skillfs stop"),
            "expected the explicit state-read failure, got: {msg}"
        );

        // Corrupt state fails the same way, without touching the
        // incumbent.
        std::fs::write(&paths.state, "{not json").expect("write corrupt state");
        let err = run_client(&[], source.path(), mountpoint.path(), None)
            .expect_err("a corrupt state file must fail the request");
        assert!(
            err.to_string().contains("state cannot be read"),
            "expected the explicit state-read failure, got: {err}"
        );

        std::fs::remove_file(&paths.supervisor_pid).ok();
        std::fs::remove_file(&paths.state).ok();
    }

    #[test]
    fn instance_id_differs_for_different_paths() {
        let a = instance_id_for(Path::new("/tmp/mount-a"));
        let b = instance_id_for(Path::new("/tmp/mount-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn is_mounted_matches_escaped_and_invalid_byte_paths() {
        // The matcher lives in skillfs_fuse::proc_mounts (unit-tested there);
        // this pins the wiring: escaped mountpoints match their real path and
        // an invalid-byte mount never collides with a U+FFFD query.
        let mounts = b"fuse.skillfs /mnt/my\\040skills fuse.skillfs rw 0 0\nfuse.skillfs /mnt/\xff fuse.skillfs rw 0 0\n";
        assert!(skillfs_fuse::proc_mounts::mounts_contain_target(
            mounts,
            b"/mnt/my skills"
        ));
        assert!(!skillfs_fuse::proc_mounts::mounts_contain_target(
            mounts,
            "/mnt/\u{FFFD}".as_bytes()
        ));
        assert!(skillfs_fuse::proc_mounts::mounts_contain_target(
            mounts,
            b"/mnt/\xff"
        ));
    }

    #[test]
    fn instance_id_includes_sanitized_basename() {
        let id = instance_id_for(Path::new("/var/run/my.mount point"));
        // Non-alphanumerics in the basename become dashes.
        assert!(id.starts_with("my-mount-point-"), "got: {id}");
    }

    #[test]
    fn instance_id_never_leads_with_a_dash() {
        // A dot-prefixed mountpoint — the shape every test harness tempdir
        // has — used to sanitize to a leading dash, which the supervisor's
        // `--instance` argument rejected as an option-looking value: the
        // supervisor died before writing its pid file and the client
        // reported a readiness timeout with an empty worker log.
        let id = instance_id_for(Path::new("/tmp/.tmpAbCdEf"));
        assert!(
            id.starts_with("tmpAbCdEf-"),
            "the dot must not become a leading dash, got: {id}"
        );
        assert!(
            id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()),
            "ids are argv values and file-name prefixes; they must start alphanumeric, got: {id}"
        );
        // A basename of only non-alphanumerics still yields the root
        // fallback, and uniqueness rides on the path hash either way.
        let dots = instance_id_for(Path::new("/tmp/..."));
        assert!(dots.starts_with("root-"), "got: {dots}");
    }

    #[test]
    fn instance_id_preserves_the_legacy_underscore_prefix() {
        // Ids that never started with a dash were always valid
        // `--instance` values and runtime file-name prefixes, so a live
        // supervisor may be sitting on their state/pid files when the
        // binary is upgraded. Stripping the leading underscore as well
        // would rename the instance across the upgrade: `stop` would miss
        // the incumbent's files and report success while the old
        // supervisor kept running (free to remount later), and a new
        // `mount --managed` would race a competing supervisor. Only the
        // leading dash — the one shape that never worked — is stripped.
        let id = instance_id_for(Path::new("/mnt/_skills"));
        assert!(id.starts_with("_skills-"), "got: {id}");
        // A legacy-valid id that already mixes allowed non-alphanumerics
        // must survive verbatim too.
        let mixed = instance_id_for(Path::new("/mnt/_-skills"));
        assert!(mixed.starts_with("_-skills-"), "got: {mixed}");
    }

    #[test]
    fn build_worker_args_drops_managed_and_adds_foreground() {
        let raw = vec![
            "mount".to_string(),
            "/src".to_string(),
            "/mnt".to_string(),
            "--managed".to_string(),
        ];
        let out = build_worker_args(&raw);
        assert!(!out.iter().any(|a| a == "--managed"));
        assert_eq!(out, vec!["mount", "--foreground", "/src", "/mnt"]);
    }

    #[test]
    fn build_worker_args_keeps_existing_foreground_without_duplicating() {
        let raw = vec![
            "mount".to_string(),
            "--foreground".to_string(),
            "/src".to_string(),
            "/mnt".to_string(),
            "--managed".to_string(),
        ];
        let out = build_worker_args(&raw);
        assert_eq!(out.iter().filter(|a| *a == "--foreground").count(), 1);
        assert!(!out.iter().any(|a| a == "--managed"));
    }

    #[test]
    fn build_worker_args_preserves_other_flags() {
        let raw = vec![
            "mount".to_string(),
            "/src".to_string(),
            "/mnt".to_string(),
            "--managed".to_string(),
            "--security-mode".to_string(),
            "--audit-log".to_string(),
            "/var/log/a.jsonl".to_string(),
        ];
        let out = build_worker_args(&raw);
        assert!(out.iter().any(|a| a == "--security-mode"));
        assert!(out.iter().any(|a| a == "--audit-log"));
        assert!(out.iter().any(|a| a == "/var/log/a.jsonl"));
    }

    #[test]
    fn config_worker_args_freeze_both_config_forms_and_effective_options() {
        let config = ManagedMountConfig {
            sources: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            allow_other: true,
            read_only: true,
        };
        for config_args in [
            vec!["--config", "/original.toml"],
            vec!["--config=/original.toml"],
        ] {
            let raw: Vec<String> = [
                vec!["--verbose", "mount", "--managed", "--allow-other"],
                config_args,
                vec!["--log-file", "/logs/worker-{pid}.log", "--"],
            ]
            .concat()
            .into_iter()
            .map(String::from)
            .collect();
            let args = build_config_worker_args(&raw, &config, Path::new("/resolved.toml"))
                .expect("resolved worker args");
            assert!(!args.iter().any(|arg| arg.contains("original.toml")));
            assert!(args.iter().any(|arg| arg.contains("/resolved.toml")));
            for flag in ["--foreground", "--allow-other", "--read-only", "--verbose"] {
                assert_eq!(args.iter().filter(|arg| arg.as_str() == flag).count(), 1);
            }
            assert_eq!(args.last().map(String::as_str), Some("--"));
            assert!(
                args.windows(2)
                    .any(|args| { args == ["--log-file", "/logs/worker-{pid}.log"] })
            );
        }
    }

    #[test]
    fn resolved_mount_config_survives_original_edits_and_deletion() {
        let dir = tempfile::tempdir().expect("config tempdir");
        let source = dir.path().join("source");
        std::fs::create_dir(&source).expect("source directory");
        let original = dir.path().join("original.toml");
        let resolved = dir.path().join("resolved.toml");
        let input = super::super::mount_file::MountFile {
            mountpoint: dir.path().join("mount"),
            sources: vec![source.clone(), source],
        };
        std::fs::write(&original, toml::to_string(&input).expect("input TOML"))
            .expect("write original");
        let file = super::super::mount_file::load(&original)
            .expect("load original")
            .expect("mount config")
            .validate()
            .expect("validated config");
        assert_eq!(file.sources.len(), 1, "canonical duplicates collapse");
        let config = ManagedMountConfig {
            sources: file.sources.clone(),
            allow_other: false,
            read_only: false,
        };
        save_mount_config(&config, &file.mountpoint, &resolved).expect("save resolved config");
        for delete in [false, true] {
            if delete {
                std::fs::remove_file(&original).expect("delete original config");
            } else {
                std::fs::write(&original, "invalid replacement config").expect("edit original");
            }
            let replay = super::super::mount_file::load(&resolved)
                .expect("read frozen config")
                .expect("frozen mount config");
            assert_eq!(replay.mountpoint, file.mountpoint);
            assert_eq!(replay.sources, file.sources);
        }
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = ManagedState {
            schema_version: STATE_SCHEMA_VERSION,
            instance_id: "mnt-000000000000dead".to_string(),
            mountpoint: "/mnt/skillfs".to_string(),
            source: "/srv/skills".to_string(),
            mount_config: None,
            worker_program: "/usr/bin/skillfs".to_string(),
            worker_args: vec![
                "mount".to_string(),
                "--foreground".to_string(),
                "/srv/skills".to_string(),
                "/mnt/skillfs".to_string(),
            ],
            desired_state: DesiredState::Mounted,
        };
        state.save(&path).unwrap();
        let loaded = ManagedState::load(&path).unwrap();
        assert_eq!(loaded.instance_id, state.instance_id);
        assert_eq!(loaded.desired_state, DesiredState::Mounted);
        assert_eq!(loaded.worker_args, state.worker_args);
    }

    #[test]
    fn desired_state_serializes_lowercase() {
        let json = serde_json::to_string(&DesiredState::Stopped).unwrap();
        assert_eq!(json, "\"stopped\"");
        let parsed: DesiredState = serde_json::from_str("\"mounted\"").unwrap();
        assert_eq!(parsed, DesiredState::Mounted);
    }

    fn sample_state(instance_id: &str) -> ManagedState {
        ManagedState {
            schema_version: STATE_SCHEMA_VERSION,
            instance_id: instance_id.to_string(),
            mountpoint: "/mnt/skillfs".to_string(),
            source: "/srv/skills".to_string(),
            mount_config: None,
            worker_program: "/usr/bin/skillfs".to_string(),
            worker_args: vec![
                "mount".to_string(),
                "--foreground".to_string(),
                "/srv/skills".to_string(),
                "/mnt/skillfs".to_string(),
            ],
            desired_state: DesiredState::Mounted,
        }
    }

    #[test]
    fn legacy_state_without_config_identity_remains_positional() {
        let mut legacy = sample_state("legacy");
        let json = serde_json::to_string(&legacy).expect("legacy JSON");
        assert!(!json.contains("mount_config"));
        legacy = serde_json::from_str(&json).expect("read old state shape");
        assert!(legacy.mount_config.is_none());
        // Legacy request matching intentionally does not compare worker options.
        legacy.worker_args.push("--allow-other".to_string());
        legacy.worker_args.push("--read-only".to_string());
        assert!(active_instance_matches(
            &legacy,
            Path::new("/srv/skills"),
            Path::new("/mnt/skillfs"),
            None,
        ));
        let config = ManagedMountConfig {
            sources: vec![PathBuf::from("/srv/skills")],
            allow_other: false,
            read_only: false,
        };
        assert!(!active_instance_matches(
            &legacy,
            Path::new("/srv/skills"),
            Path::new("/mnt/skillfs"),
            Some(&config),
        ));
    }

    #[test]
    fn config_identity_includes_order_mode_and_effective_options() {
        let mut state = sample_state("config");
        let config = ManagedMountConfig {
            sources: vec![PathBuf::from("/srv/skills"), PathBuf::from("/srv/extra")],
            allow_other: false,
            read_only: true,
        };
        state.mount_config = Some(config.clone());
        let matches = |request: Option<&ManagedMountConfig>| {
            active_instance_matches(
                &state,
                Path::new("/srv/skills"),
                Path::new("/mnt/skillfs"),
                request,
            )
        };
        assert!(matches(Some(&config)));
        assert!(!matches(None));
        let mut changed = config.clone();
        changed.sources.reverse();
        assert!(!matches(Some(&changed)));
        changed = config.clone();
        changed.sources[1] = PathBuf::from("/srv/other");
        assert!(!matches(Some(&changed)));
        changed = config.clone();
        changed.allow_other = true;
        assert!(!matches(Some(&changed)));
        changed = config;
        changed.read_only = false;
        assert!(!matches(Some(&changed)));
    }

    #[test]
    fn managed_cleanup_removes_only_owned_resolved_config() {
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        let original = tempfile::NamedTempFile::new().expect("user config");
        std::fs::write(original.path(), "user config unchanged").expect("user config bytes");
        let normalized = normalize_mountpoint(mountpoint.path());
        let paths = ManagedPaths {
            state: runtime.path().join("instance.state.json"),
            supervisor_pid: runtime.path().join("instance.supervisor.pid"),
            worker_pid: runtime.path().join("instance.worker.pid"),
            supervisor_log: runtime.path().join("instance.supervisor.log"),
            worker_log: runtime.path().join("instance.worker.log"),
            mount_config: runtime.path().join("instance.mount.toml"),
        };
        let mut state = sample_state("instance");
        state.mountpoint = normalized.to_string_lossy().to_string();
        state.mount_config = Some(ManagedMountConfig {
            sources: vec![PathBuf::from("/srv/skills")],
            allow_other: false,
            read_only: false,
        });
        std::fs::write(&paths.mount_config, "owned frozen spec").expect("owned config");
        for mounts in [
            Ok(format!("skillfs {} fuse rw 0 0\n", normalized.display()).into_bytes()),
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unreadable mount table",
            )),
        ] {
            state.save(&paths.state).expect("mounted config state");
            std::fs::write(&paths.supervisor_pid, "supervisor pid fixture")
                .expect("supervisor pid");
            std::fs::write(&paths.worker_pid, "worker pid fixture").expect("worker pid");
            remove_instance_files(&paths, &normalized, mounts)
                .expect_err("busy or unknown mount must preserve config state");
            assert!(paths.mount_config.exists());
            assert!(paths.supervisor_pid.exists());
            assert!(paths.worker_pid.exists());
            let retained = ManagedState::load(&paths.state).expect("retained state");
            assert_eq!(retained.desired_state, DesiredState::Stopped);
            assert!(retained.mount_config.is_some());
        }
        for cleanup in 0..2 {
            std::fs::write(&paths.mount_config, "owned frozen spec").expect("owned config");
            match cleanup {
                0 => finish(&paths, &normalized),
                _ => teardown_instance(&paths, &normalized).expect("teardown"),
            }
            assert!(!paths.mount_config.exists());
            assert_eq!(
                std::fs::read_to_string(original.path()).expect("original config survives"),
                "user config unchanged"
            );
        }
    }

    fn assert_config_stop_retained_then_retry(
        unmount_result: Result<(), Box<dyn Error>>,
        mounts: impl FnOnce(&Path) -> std::io::Result<Vec<u8>>,
    ) {
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        let original = tempfile::NamedTempFile::new().expect("user config");
        std::fs::write(original.path(), "user config unchanged").expect("user config bytes");
        let mountpoint = Path::new("/mnt/skillfs");
        let paths = ManagedPaths {
            state: runtime.path().join("instance.state.json"),
            supervisor_pid: runtime.path().join("instance.supervisor.pid"),
            worker_pid: runtime.path().join("instance.worker.pid"),
            supervisor_log: runtime.path().join("instance.supervisor.log"),
            worker_log: runtime.path().join("instance.worker.log"),
            mount_config: runtime.path().join("instance.mount.toml"),
        };
        let mut state = sample_state("instance");
        state.mount_config = Some(ManagedMountConfig {
            sources: vec![PathBuf::from("/srv/skills")],
            allow_other: false,
            read_only: false,
        });
        state.save(&paths.state).expect("mounted config state");
        std::fs::write(&paths.mount_config, "owned frozen spec").expect("owned config");
        // Non-numeric fixtures exercise PID-file retention without signaling.
        std::fs::write(&paths.supervisor_pid, "supervisor pid fixture").expect("supervisor pid");
        std::fs::write(&paths.worker_pid, "worker pid fixture").expect("worker pid");

        teardown_instance_with_shutdown(
            &paths,
            mountpoint,
            |processes_gone| {
                assert!(processes_gone());
                assert_eq!(
                    ManagedState::load(&paths.state)
                        .expect("stopped before shutdown")
                        .desired_state,
                    DesiredState::Stopped
                );
                unmount_result
            },
            || mounts(mountpoint),
        )
        .expect_err("failed or unconfirmed unmount must retain managed state");

        let retained = ManagedState::load(&paths.state).expect("retained state");
        assert_eq!(retained.desired_state, DesiredState::Stopped);
        assert_eq!(retained.mount_config, state.mount_config);
        assert_eq!(retained.worker_args, state.worker_args);
        assert!(paths.supervisor_pid.exists());
        assert!(paths.worker_pid.exists());
        assert_eq!(
            std::fs::read_to_string(&paths.mount_config).expect("retained frozen config"),
            "owned frozen spec"
        );

        // Retry the same production teardown path against the retained record.
        teardown_instance_with_shutdown(
            &paths,
            mountpoint,
            |processes_gone| {
                assert!(processes_gone());
                Ok(())
            },
            || Ok(Vec::new()),
        )
        .expect("successful stop retry");
        assert!(!paths.state.exists());
        assert!(!paths.mount_config.exists());
        assert!(!paths.supervisor_pid.exists());
        assert!(!paths.worker_pid.exists());
        assert_eq!(
            std::fs::read_to_string(original.path()).expect("original config survives"),
            "user config unchanged"
        );
    }

    #[test]
    fn managed_stop_keeps_config_after_busy_unmount_and_retries() {
        // Even an absent final mount-table entry must not override failure.
        assert_config_stop_retained_then_retry(
            Err(std::io::Error::from_raw_os_error(libc::EBUSY).into()),
            |_| Ok(Vec::new()),
        );
    }

    #[test]
    fn managed_stop_keeps_config_when_mount_remains_and_retries() {
        assert_config_stop_retained_then_retry(Ok(()), |mountpoint| {
            Ok(format!("skillfs {} fuse rw 0 0\n", mountpoint.display()).into_bytes())
        });
    }

    #[test]
    fn managed_stop_keeps_config_when_mount_table_is_unreadable_and_retries() {
        assert_config_stop_retained_then_retry(Ok(()), |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unreadable mount table",
            ))
        });
    }

    #[cfg(unix)]
    #[test]
    fn save_does_not_publish_through_a_planted_staging_entry() {
        // A fixed staging path makes any entry already sitting there the
        // publication vehicle: writing through a symlink would clobber its
        // target and rename the link itself onto the state path. Plant
        // entries at the legacy fixed name and at the first candidates of a
        // private counter (injected via `save_with_counter`), so the save
        // deterministically hits every planted name — parallel tests can no
        // longer advance the sequence between the plant and the save and
        // leave the collision unexercised.
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("mnt-0000dead.state.json");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "untouched").unwrap();
        // Legacy fixed staging name (`with_extension("json.tmp")`).
        std::os::unix::fs::symlink(&victim, state_path.with_extension("json.tmp")).unwrap();
        let counter = AtomicU64::new(0);
        // The first two candidates this save must refuse and skip.
        let planted: Vec<PathBuf> = (0..2)
            .map(|seq| {
                dir.path().join(format!(
                    ".mnt-0000dead.state.json.{}.{}.tmp",
                    std::process::id(),
                    seq
                ))
            })
            .collect();
        for candidate in &planted {
            std::os::unix::fs::symlink(&victim, candidate).unwrap();
        }

        sample_state("mnt-0000dead")
            .save_with_counter(&state_path, &counter)
            .unwrap();

        // The save really collided with both planted candidates: a private
        // counter starting at 0 makes the save consume 0, 1 (refused) and
        // then 2 (created), and exactly that.
        assert_eq!(
            counter.load(Ordering::Relaxed),
            3,
            "the save must have refused both planted candidates and \
             published on the third name"
        );
        // The skipped entries are still the planted symlinks, untouched.
        for candidate in &planted {
            assert!(
                std::fs::symlink_metadata(candidate)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "a refused candidate must be left as-is, not removed: {candidate:?}"
            );
        }
        assert!(
            !std::fs::symlink_metadata(&state_path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the published state must be a regular file"
        );
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "untouched",
            "no planted staging entry may be written through"
        );
        let loaded = ManagedState::load(&state_path).unwrap();
        assert_eq!(loaded.instance_id, "mnt-0000dead");
        assert_eq!(loaded.desired_state, DesiredState::Mounted);
    }

    #[test]
    fn concurrent_saves_in_one_process_all_publish() {
        // Every save must own its staging path. With one path shared by all
        // saves in the process, concurrent callers truncate each other's
        // staging bytes and the last rename can publish a torn file.
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("mnt-0000beef.state.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|worker| {
                let state_path = state_path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut state = sample_state("mnt-0000beef");
                    state.mountpoint = format!("/mnt/skillfs-{worker}");
                    barrier.wait();
                    (0..40).filter(|_| state.save(&state_path).is_ok()).count()
                })
            })
            .collect();
        let published: usize = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum();
        assert_eq!(published, 320, "every concurrent save must publish");
        // Whichever save won, the published state is complete, never torn.
        let loaded = ManagedState::load(&state_path).unwrap();
        assert_eq!(loaded.instance_id, "mnt-0000beef");
        assert_eq!(loaded.desired_state, DesiredState::Mounted);
    }

    #[test]
    fn save_leaves_a_foreign_staging_entry_alone() {
        // Only the call that created a staging file may remove it: an entry
        // left by a crashed save (or a concurrent writer) must survive a
        // failing save's cleanup. The entry is planted at the first
        // candidate of a private counter (injected via `save_with_counter`),
        // so this save deterministically collides with it regardless of
        // parallel tests advancing the process-global sequence.
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("mnt-0000cafe.state.json");
        let counter = AtomicU64::new(0);
        let foreign = dir.path().join(format!(
            ".mnt-0000cafe.state.json.{}.0.tmp",
            std::process::id()
        ));
        std::fs::write(&foreign, "foreign leftover").unwrap();

        sample_state("mnt-0000cafe")
            .save_with_counter(&state_path, &counter)
            .unwrap();

        // The save collided with the foreign entry (candidate 0 refused)
        // and published on the next name — exactly two counter draws.
        assert_eq!(
            counter.load(Ordering::Relaxed),
            2,
            "the save must have refused the foreign candidate and \
             published on the next name"
        );
        assert_eq!(
            std::fs::read_to_string(&foreign).unwrap(),
            "foreign leftover",
            "a foreign staging entry must be left alone"
        );
        assert!(ManagedState::load(&state_path).is_ok());
    }

    #[test]
    fn current_desired_state_missing_file_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.json");
        assert_eq!(current_desired_state(&missing), DesiredState::Stopped);
    }

    #[test]
    fn classify_mount_absent_path_is_not_mounted() {
        let dir = tempfile::tempdir().unwrap();
        let never_mounted = dir.path().join("not-a-mount");
        assert_eq!(classify_mount(&never_mounted), MountState::NotMounted);
    }

    #[test]
    fn classify_mount_plain_dir_is_not_mounted() {
        // A real, accessible directory that is not a mountpoint classifies as
        // NotMounted (it is not present in /proc/mounts), never Ready.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(classify_mount(dir.path()), MountState::NotMounted);
    }

    #[test]
    fn is_mount_ready_false_for_plain_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_mount_ready(dir.path()));
    }

    #[test]
    fn clear_mount_is_ok_when_nothing_mounted() {
        let dir = tempfile::tempdir().unwrap();
        // Not a mountpoint, so clear_mount must succeed immediately without
        // invoking any unmount tooling.
        clear_mount(dir.path()).unwrap();
    }

    #[test]
    fn secure_dir_creates_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("skillfs");
        secure_dir(&dir).unwrap();
        assert!(dir.is_dir());
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "runtime dir must be created 0700, got {mode:o}"
        );
    }

    #[test]
    fn secure_dir_tightens_loose_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("skillfs");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        secure_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "group/other bits must be stripped");
    }

    #[test]
    fn secure_dir_rejects_symlink() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("real");
        std::fs::create_dir(&target).unwrap();
        let link = parent.path().join("skillfs");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = secure_dir(&link).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "expected symlink rejection, got: {err}"
        );
    }

    #[test]
    fn active_instance_matches_only_the_recorded_source_and_mountpoint() {
        let state = ManagedState {
            schema_version: STATE_SCHEMA_VERSION,
            instance_id: "abc".to_string(),
            mountpoint: "/mnt/skills".to_string(),
            source: "/srv/src-a".to_string(),
            mount_config: None,
            worker_program: "/usr/bin/skillfs".to_string(),
            worker_args: vec![],
            desired_state: DesiredState::Mounted,
        };
        let mnt = Path::new("/mnt/skills");
        assert!(active_instance_matches(
            &state,
            Path::new("/srv/src-a"),
            mnt,
            None
        ));
        // Same mountpoint, different source: the instance id collides, so the
        // request must be refused rather than reported as already active.
        assert!(!active_instance_matches(
            &state,
            Path::new("/srv/src-b"),
            mnt,
            None
        ));
        assert!(!active_instance_matches(
            &state,
            Path::new("/srv/src-a"),
            Path::new("/mnt/other"),
            None
        ));
    }

    #[test]
    fn runtime_dir_prefers_xdg_runtime_dir() {
        // Safe: single-threaded unit test process; we restore afterward.
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/12345");
        }
        assert_eq!(runtime_dir(), PathBuf::from("/run/user/12345/skillfs"));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }
}
