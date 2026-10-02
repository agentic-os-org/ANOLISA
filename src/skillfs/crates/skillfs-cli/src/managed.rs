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
use std::sync::atomic::{AtomicBool, Ordering};
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
const READY_TIMEOUT_MS: u64 = 10_000;
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

/// On-disk record describing a managed mount instance.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ManagedState {
    pub schema_version: u32,
    pub instance_id: String,
    pub mountpoint: String,
    pub source: String,
    pub worker_program: String,
    pub worker_args: Vec<String>,
    pub desired_state: DesiredState,
}

impl ManagedState {
    fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let raw = std::fs::read_to_string(path)?;
        let state: ManagedState = serde_json::from_str(&raw)?;
        Ok(state)
    }

    fn save(&self, path: &Path) -> Result<(), Box<dyn Error>> {
        let raw = serde_json::to_string_pretty(self)?;
        // Write-and-rename for atomicity so a concurrent reader never sees a
        // half-written file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Filesystem paths for a managed mount instance.
pub struct ManagedPaths {
    pub state: PathBuf,
    pub supervisor_pid: PathBuf,
    pub worker_pid: PathBuf,
    pub supervisor_log: PathBuf,
    pub worker_log: PathBuf,
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
/// full path (for uniqueness).
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

/// Whether the mountpoint currently appears in `/proc/mounts`.
pub fn is_mounted(mountpoint: &Path) -> bool {
    let target = mountpoint.to_string_lossy();
    match std::fs::read_to_string("/proc/mounts") {
        Ok(info) => info
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(&*target)),
        Err(_) => false,
    }
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
fn unmount_once(mountpoint: &Path) -> bool {
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.to_string_lossy()])
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

/// Entry point for a managed mount request. Validates the source, writes the
/// managed state, spawns a detached supervisor, and waits for readiness.
pub fn run_client(
    raw_args: &[String],
    source: &Path,
    mountpoint: &Path,
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
            send_signal(pid, libc::SIGTERM);
            let deadline = Instant::now() + Duration::from_millis(STOP_TIMEOUT_MS);
            while pid_alive(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if pid_alive(pid) {
                send_signal(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(&paths.worker_pid);
    }
    if is_mounted(&normalized) {
        clear_mount(&normalized)?;
    }
    let _ = std::fs::remove_file(&paths.supervisor_pid);

    let worker_program = std::env::current_exe()
        .map_err(|e| format!("failed to resolve current executable: {e}"))?;
    let worker_args = build_worker_args(raw_args);

    let state = ManagedState {
        schema_version: STATE_SCHEMA_VERSION,
        instance_id: instance_id.clone(),
        mountpoint: normalized.to_string_lossy().to_string(),
        source: source_norm.to_string_lossy().to_string(),
        worker_program: worker_program.to_string_lossy().to_string(),
        worker_args,
        desired_state: DesiredState::Mounted,
    };
    state.save(&paths.state)?;

    spawn_supervisor(&worker_program, &instance_id, &paths)?;

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

    write_pid(&paths.supervisor_pid, std::process::id())?;
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
            .map_err(|e| format!("failed to open worker log: {e}"))?;
        let worker_err = worker_out.try_clone()?;

        let start = Instant::now();
        let mut child = std::process::Command::new(&state.worker_program)
            .args(&state.worker_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(worker_out))
            .stderr(std::process::Stdio::from(worker_err))
            .spawn()
            .map_err(|e| format!("failed to spawn worker: {e}"))?;
        let worker_pid = child.id();
        let _ = write_pid(&paths.worker_pid, worker_pid);
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
                let _ = wait_child_timeout(&mut child, Duration::from_millis(STOP_TIMEOUT_MS));
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
                // Mark the instance stopped so nothing resurrects it, then fall
                // through to finish(): unmount and clear residual state.
                if let Ok(mut state) = ManagedState::load(&paths.state) {
                    state.desired_state = DesiredState::Stopped;
                    let _ = state.save(&paths.state);
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

fn wait_child_timeout(child: &mut std::process::Child, timeout: Duration) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(_) => return Ok(()),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(WORKER_POLL_MS));
            }
        }
    }
}

/// Final cleanup: ensure the mount is gone and remove instance state.
fn finish(paths: &ManagedPaths, mountpoint: &Path) {
    if is_mounted(mountpoint) {
        if let Err(e) = clear_mount(mountpoint) {
            warn!(error = %e, "failed to unmount during supervisor cleanup");
        }
    }
    let _ = std::fs::remove_file(&paths.worker_pid);
    let _ = std::fs::remove_file(&paths.supervisor_pid);
    let _ = std::fs::remove_file(&paths.state);
    info!(mountpoint = %mountpoint.display(), "managed supervisor exiting");
}

// ---------------------------------------------------------------------------
// Stop: `skillfs stop <MOUNTPOINT>`
// ---------------------------------------------------------------------------

/// Tear down a managed instance: mark it stopped, signal the supervisor
/// (falling back to the worker), wait for exit, unmount, and remove residual
/// pid/state files. Returns the unmount result so callers can surface a cleanup
/// failure; every other step is best-effort.
///
/// Shared by `stop` and by the client's startup-abort path so neither has to
/// duplicate the signal/wait/unmount sequence.
fn teardown_instance(paths: &ManagedPaths, mountpoint: &Path) -> Result<(), Box<dyn Error>> {
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

    // Wait for the managed processes to exit. The supervisor unmounts cleanly
    // on SIGTERM, so once it is gone the mount is normally already down; any
    // remaining endpoint is cleared explicitly below.
    let deadline = Instant::now() + Duration::from_millis(STOP_TIMEOUT_MS);
    loop {
        let sup_gone = sup_pid.is_none_or(|p| !pid_alive(p));
        let worker_gone = worker_pid.is_none_or(|p| !pid_alive(p));
        if sup_gone && worker_gone {
            break;
        }
        if Instant::now() >= deadline {
            warn!("teardown timed out waiting for clean shutdown; forcing");
            if let Some(p) = sup_pid {
                if pid_alive(p) {
                    send_signal(p, libc::SIGKILL);
                }
            }
            if let Some(p) = worker_pid {
                if pid_alive(p) {
                    send_signal(p, libc::SIGKILL);
                }
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Ensure the mount is gone, clearing any stale/dead endpoint a killed
    // worker left behind. The error is surfaced after state cleanup.
    let unmount_result = if is_mounted(mountpoint) {
        clear_mount(mountpoint)
    } else {
        Ok(())
    };

    // Best-effort final cleanup of any residual state files.
    let _ = std::fs::remove_file(&paths.worker_pid);
    let _ = std::fs::remove_file(&paths.supervisor_pid);
    let _ = std::fs::remove_file(&paths.state);

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
        let _ = std::fs::remove_file(&paths.worker_pid);
        let _ = std::fs::remove_file(&paths.supervisor_pid);
        match classify_mount(&normalized) {
            MountState::NotMounted => {
                println!(
                    "skillfs: no managed mount at {} (already stopped)",
                    normalized.display()
                );
            }
            // Present (ready or a stale/dead endpoint): unmount it, surfacing
            // an error if cleanup cannot complete.
            _ => {
                clear_mount(&normalized)?;
                println!("skillfs: unmounted {}", normalized.display());
            }
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

    #[test]
    fn instance_id_differs_for_different_paths() {
        let a = instance_id_for(Path::new("/tmp/mount-a"));
        let b = instance_id_for(Path::new("/tmp/mount-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn instance_id_includes_sanitized_basename() {
        let id = instance_id_for(Path::new("/var/run/my.mount point"));
        // Non-alphanumerics in the basename become dashes.
        assert!(id.starts_with("my-mount-point-"), "got: {id}");
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
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = ManagedState {
            schema_version: STATE_SCHEMA_VERSION,
            instance_id: "mnt-000000000000dead".to_string(),
            mountpoint: "/mnt/skillfs".to_string(),
            source: "/srv/skills".to_string(),
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
