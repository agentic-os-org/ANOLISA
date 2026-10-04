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
use std::sync::OnceLock;
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
const READY_TIMEOUT_MS: u64 = 10_000;
/// How long `stop` waits for processes to exit / the mount to disappear.
const STOP_TIMEOUT_MS: u64 = 10_000;
/// Grace window for a forced (SIGKILL) exit to land before teardown rules on
/// the final liveness verdict. Signals that were refused for safety (no
/// pidfd) stay refused through this window, so the verdict catches them.
const KILL_GRACE_MS: u64 = 2_000;
/// The fd the spawned supervisor reads its startup handshake from (see
/// [`spawn_supervisor`]). fd 3 is the first fd past the std streams; the
/// client always creates the pipe and dup'ed it onto this fd in the child,
/// and a hand-started supervisor has no fd 3 at all (see
/// [`await_publish_handshake`]).
const SUPERVISOR_HANDSHAKE_FD: libc::c_int = 3;
/// The startup handshake byte: the client writes it only after the
/// supervisor's identity is durably published (review of this PR).
const HANDSHAKE_GO_BYTE: u8 = b'g';
/// How long the client waits for the handshake-terminated supervisor child to
/// actually exit on the failed-publication path (best-effort reap).
const HANDSHAKE_EXIT_BUDGET_MS: u64 = 2_000;

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
        // Write-and-rename for atomicity so a concurrent reader never sees a
        // half-written file. Every save stages on its own exclusive path and
        // removes only the file it created, so concurrent savers — the
        // client, the supervisor's stopped marker, and a racing teardown —
        // never write through or publish another call's staging entry.
        let (tmp, mut file) = create_staging_file(path, counter)?;
        let result = (|| {
            use std::io::Write;
            file.write_all(raw.as_bytes())?;
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            // No other save ever stages at this path, so this removes only
            // the file this call created.
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(Into::into)
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

fn send_signal(pid: i32, sig: i32) {
    if pid > 0 {
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

fn write_pid(path: &Path, pid: u32) -> Result<(), Box<dyn Error>> {
    std::fs::write(path, format!("{pid}\n"))?;
    // A pid-only record is unverifiable by design; it must never stay
    // paired with a leftover identity sidecar of an earlier occupant.
    let _ = std::fs::remove_file(identity_sidecar(path));
    Ok(())
}

/// The identity of a managed process, captured from `/proc` at the moment
/// the process was created and recorded in an identity sidecar beside its
/// pid file (the pid file itself stays the bare pid every version
/// reads — see [`write_pid_identity`]).
///
/// A bare pid is not a safe addressing handle: pids are recycled, so the
/// process behind `<pid>` may be an unrelated reuser by the time `stop`
/// reads the file. The identity binds the pid to the exact process
/// creation — the kernel start time from `/proc/<pid>/stat` field 22
/// (`starttime`, stable for the process lifetime and different for any
/// later reuser of the pid) — and to the full executable path from
/// `/proc/<pid>/exe` (not just the basename, so a different binary with
/// the same name is rejected). The start tick is however only unique
/// *within one system boot*: it restarts from zero at reboot, and the
/// runtime directory can be persistent (`XDG_RUNTIME_DIR` is not required
/// to be tmpfs), so the identity is additionally bound to the boot it was
/// captured on (`boot_id`, from `/proc/sys/kernel/random/boot_id` —
/// review of this PR): a record from another boot never verifies, so a
/// reboot invalidates every leftover generation instead of letting a
/// coincidental pid + start tick + exe triple address the wrong new
/// process. The pid file's name binds the instance
/// (`<instance>.worker.pid` / `<instance>.supervisor.pid`), and the state
/// file's `worker_program` pins the expected program, so only this
/// instance's own supervisor/worker pair is ever a signal target.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessIdentity {
    pid: i32,
    /// `/proc/<pid>/stat` field 22: clock ticks since boot at process
    /// creation. `0` means "not recorded" (see
    /// [`ProcessIdentity::unverified`]).
    starttime: u64,
    /// Full `/proc/<pid>/exe` target path. Empty means "not recorded".
    exe: String,
    /// The system boot this record was captured on
    /// (`/proc/sys/kernel/random/boot_id`). Empty means "not recorded"; a
    /// record from any other boot never verifies.
    boot_id: String,
}

impl ProcessIdentity {
    /// An identity parsed from a legacy pid-only file. The process cannot
    /// be verified, so it must never be signaled.
    fn unverified(pid: i32) -> Self {
        Self {
            pid,
            starttime: 0,
            exe: String::new(),
            boot_id: String::new(),
        }
    }

    /// Whether this record carries enough information to verify a live
    /// process against it.
    fn is_verifiable(&self) -> bool {
        self.starttime != 0 && !self.exe.is_empty() && !self.boot_id.is_empty()
    }

    /// Capture the identity of `pid` from `/proc` right now. Only
    /// meaningful when the caller knows the process was just created (own
    /// pid, or a direct unreaped child), so the pid cannot already have
    /// been reused.
    fn capture(pid: i32) -> Option<Self> {
        let boot_id = current_boot_id()?.to_string();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let starttime = stat_starttime(&stat)?;
        let exe = read_proc_exe(pid)?;
        Some(Self {
            pid,
            starttime,
            exe,
            boot_id,
        })
    }
}

/// The id of the current system boot, from
/// `/proc/sys/kernel/random/boot_id`, read once per process and cached.
///
/// The boot id is what makes the recorded start tick globally meaningful
/// (see [`ProcessIdentity`]): every record is bound to the boot it was
/// captured on, and verification refuses when the machine has rebooted
/// since — start ticks reset to zero and inode numbers recycle, so all
/// pre-reboot generations are invalid at once. `None` (no `/proc`, or an
/// unreadable boot id) means identities can neither be captured nor
/// verified on this host: callers fail closed rather than signal on an
/// unbound record.
fn current_boot_id() -> Option<&'static str> {
    static BOOT_ID: OnceLock<Option<String>> = OnceLock::new();
    BOOT_ID
        .get_or_init(|| {
            std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .ok()
                .map(|raw| raw.trim().to_string())
                .filter(|id| !id.is_empty())
        })
        .as_deref()
}

/// Parse the process start time (field 22) out of a `/proc/<pid>/stat`
/// line.
///
/// Field 2 (`comm`, the executable basename) is parenthesized and may
/// itself contain spaces and parentheses, so fields are counted after the
/// LAST `)` — the same rule the other `/proc/<pid>/stat` parsers in this
/// repo follow. Field 22 is the 20th whitespace token after it (field 3,
/// the state, is the first).
fn stat_starttime(stat: &str) -> Option<u64> {
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// Read the full executable path of `pid` from `/proc/<pid>/exe`.
fn read_proc_exe(pid: i32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(exe.to_string_lossy().into_owned())
}

/// The suffix the kernel appends to a `/proc/<pid>/exe` readlink target
/// once the executable file was unlinked. For the process an identity was
/// recorded from, the live target is therefore always either exactly the
/// recorded path or the recorded path plus this marker — and nothing else
/// (a new file created at the same path does not remove the marker: the
/// link still points at the deleted original inode).
const DELETED_MARKER: &str = " (deleted)";

/// Whether a live process observed as (`live_starttime`, `live_exe`) is
/// the exact process `recorded` was captured from. Both the creation time
/// and the full executable path must match; a record without identity
/// never matches.
///
/// The exe comparison is exact first, and the live value may additionally
/// carry one kernel [`DELETED_MARKER`] — an unlinked binary keeps it on the
/// live side only. Stripping one marker from *each* side (the earlier
/// behavior) corrupts paths whose file names legitimately end in
/// ` (deleted)` (review of this PR): a recorded `/p/skillfs (deleted)`
/// reads back as `/p/skillfs (deleted) (deleted)` once unlinked, and
/// stripping one layer per side leaves two different paths, so the running
/// instance read as dead. The recorded value is therefore never mutated:
/// only `live == recorded` or `live == recorded + one marker` prove the
/// same file.
fn same_process(recorded: &ProcessIdentity, live_starttime: u64, live_exe: &str) -> bool {
    recorded.is_verifiable()
        && recorded.starttime == live_starttime
        && (live_exe == recorded.exe
            || live_exe
                .strip_suffix(DELETED_MARKER)
                .is_some_and(|without_marker| without_marker == recorded.exe))
}

/// Whether the process `recorded` was captured from is still alive at the
/// same pid with the same identity. A dead pid, an unreadable `/proc`
/// entry, or any identity mismatch (pid reused by another process —
/// including another skillfs instance — or by a same-named binary at a
/// different path) counts as gone. So does a record captured on another
/// system boot: its start tick addresses a process of that boot, not of
/// this one (review of this PR), and after a reboot the pid may be owned
/// by an unrelated process whose start tick and exe happen to coincide.
fn identity_alive(recorded: &ProcessIdentity) -> bool {
    if !recorded.is_verifiable() {
        return false;
    }
    if recorded.boot_id != current_boot_id().unwrap_or("") {
        return false;
    }
    let Some(stat) = std::fs::read_to_string(format!("/proc/{}/stat", recorded.pid)).ok() else {
        return false;
    };
    let Some(starttime) = stat_starttime(&stat) else {
        return false;
    };
    let Some(exe) = read_proc_exe(recorded.pid) else {
        return false;
    };
    same_process(recorded, starttime, &exe)
}

/// Whether any process currently exists at `pid`, without claiming
/// anything about which process it is. `kill(pid, 0)` is the existence
/// probe: success, or `EPERM` (the process exists but belongs to another
/// user), both mean the pid is occupied; only `ESRCH` means it is free.
fn pid_exists(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// An unverified record from `path` — a legacy bare pid, or an identity
/// sidecar that cannot be proven bound to the current pid-file write —
/// whose pid still exists, as a takeover blocker.
///
/// An unverified record cannot prove which process its pid names — the
/// pre-upgrade supervisor/worker just as well as an unrelated pid reuser —
/// and "cannot verify" must not become "safe to take over" (review of this
/// PR): while any process exists at the recorded pid, starting a second
/// supervisor/worker for the instance could contend with a live one, so
/// the operator is asked to stop the old instance first. The unverified
/// pid itself is never signaled for this; only its existence blocks
/// takeover. A record whose pid is gone blocks nothing.
fn live_legacy_blocker(path: &Path) -> Option<Box<dyn Error>> {
    let recorded = read_pid_identity(path)?;
    if !recorded.is_verifiable() && pid_exists(recorded.pid) {
        return Some(legacy_blocker_message(path, recorded.pid));
    }
    None
}

/// The takeover-refusal message for a live unverified pid record.
fn legacy_blocker_message(path: &Path, pid: i32) -> Box<dyn Error> {
    format!(
        "pid file {} records pid {pid} that is still running with an identity this \
         version cannot verify (a legacy bare-pid record, or an identity sidecar not \
         provably bound to the current pid-file write); takeover is refused — stop the \
         old instance first (kill {pid} or run the previous version's 'skillfs stop'), \
         then remove {} and remount",
        path.display(),
        path.display()
    )
    .into()
}

/// Whether a managed pid recorded in a pid file is still the process we
/// recorded: `None` (no file) and unverifiable legacy records are not.
fn managed_alive(recorded: Option<&ProcessIdentity>) -> bool {
    recorded.is_some_and(identity_alive)
}

/// Whether the pid of a legacy (unverifiable) record is still occupied by a
/// runnable process. `kill(pid, 0)` is the existence probe (success or
/// `EPERM` = occupied), with one refinement: a zombie — exited but not yet
/// reaped — can no longer contend for the instance, yet keeps its pid
/// reserved, so `/proc/<pid>/stat` state `Z` reads as gone and the stop wait
/// does not time out on a dead incumbent whose reaper (init) is slow. A pid
/// that exists per the probe but whose `/proc` entry cannot be read is
/// conservatively occupied. Verifiable records never use this probe: they
/// answer liveness exactly through [`identity_alive`].
fn unverified_pid_occupied(recorded: &ProcessIdentity) -> bool {
    if recorded.pid <= 0 {
        return false;
    }
    if !pid_exists(recorded.pid) {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{}/stat", recorded.pid)) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => true,
    }
}

/// Whether every recorded process has been confirmed gone: verifiable
/// records by identity (a dead, recycled or zombie pid reads as gone), and
/// legacy bare-pid records by the pid no longer being occupied.
fn incumbents_gone(records: &[Option<&ProcessIdentity>]) -> bool {
    records.iter().all(|recorded| match recorded {
        None => true,
        Some(identity) if identity.is_verifiable() => !identity_alive(identity),
        Some(legacy) => !unverified_pid_occupied(legacy),
    })
}

/// The incumbent that appeared (or changed) under a teardown mid-flight:
/// what the pid file names *now*, when that is no longer the record the
/// teardown acted on and it still names a process able to contend for the
/// instance.
///
/// Called immediately before the teardown's final cleanup deletes the
/// Stopped marker and the pid records. A file that is gone now (the exiting
/// incumbent's own cleanup removes its records) or an unchanged record
/// contends nothing. A changed record blocks cleanup only while it names a
/// live process: verified-and-alive, or a legacy/stale record whose pid is
/// still occupied — a changed record naming a provably gone process cannot
/// race anyone for the instance.
fn mid_stop_incumbent(
    role: &str,
    acted_on: Option<&ProcessIdentity>,
    now: Option<&ProcessIdentity>,
) -> Option<String> {
    let current = now?;
    if acted_on == Some(current) {
        return None;
    }
    let occupied = if current.is_verifiable() {
        identity_alive(current)
    } else {
        unverified_pid_occupied(current)
    };
    occupied.then(|| format!("{role} pid {}", current.pid))
}

/// Signal the exact process `recorded` was captured from, or nobody.
///
/// The signal is delivered through a pidfd so it cannot be retargeted by
/// pid reuse: the sequence is verify → `pidfd_open` → re-verify →
/// `pidfd_send_signal`. The pidfd pins whichever process owned the pid at
/// open time; if the recorded process died and the pid was reused before
/// the open, the re-verification sees the reuser's identity mismatch and
/// nothing is sent; if the recorded process dies after the open, the fd
/// still refers to it (signaling a dead process is a no-op), never to a
/// reuser. When `pidfd_open` fails — `ENOSYS` on kernels without pidfds
/// (pidfd_open arrived in Linux 5.3), or any other error — no signal is
/// sent at all: a bare `kill(pid, ...)` after the identity check leaves a
/// window in which the pid can be reused between the check and the signal
/// and take a signal meant for the dead process (review of this PR). A
/// reuse-proof handle is therefore mandatory, and refusal is the only
/// safe outcome; callers keep their records and fail explicitly when a
/// process they verified alive could not be signaled.
///
/// Returns whether a signal was delivered to the recorded process.
fn signal_identity(recorded: &ProcessIdentity, sig: i32) -> bool {
    if !identity_alive(recorded) {
        return false;
    }
    let fd = match open_pidfd(recorded.pid) {
        Ok(fd) => fd,
        Err(err) => {
            // No safe handle → no signal. Never degrade to kill(2): the
            // check-to-signal reuse window is exactly what pidfds exist to
            // close, so an unsignalable process is left running and the
            // caller decides how to surface the refusal.
            warn!(
                pid = recorded.pid,
                error = %err,
                "pidfd unavailable; refusing to signal without a reuse-proof handle"
            );
            return false;
        }
    };
    // The fd pins the process that owned the pid at open time. If that
    // was a reuser (recorded process died between the first check and the
    // open), this second check catches it before anything is sent.
    let verified = identity_alive(recorded);
    let delivered = verified
        && unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd as libc::c_int,
                sig,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        } == 0;
    unsafe {
        libc::close(fd as libc::c_int);
    }
    delivered
}

/// Open a pidfd for `pid`, or the error `pidfd_open(2)` failed with.
///
/// Test hook: with [`MOCK_PIDFD_ENOSYS`] set on the calling thread, the
/// syscall is skipped and `ENOSYS` is reported instead, exercising the
/// no-pidfd refusal path on any kernel.
fn open_pidfd(pid: i32) -> std::io::Result<libc::c_long> {
    #[cfg(test)]
    if MOCK_PIDFD_ENOSYS.with(std::cell::Cell::get) {
        return Err(std::io::Error::from_raw_os_error(libc::ENOSYS));
    }
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

// Test-only, thread-local "this kernel has no pidfd support" switch, so
// the ENOSYS refusal can be exercised without an actual pre-5.3 kernel.
// Thread-local on purpose: parallel tests in this binary call the real
// signal path and must not observe the mock.
#[cfg(test)]
thread_local! {
    static MOCK_PIDFD_ENOSYS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// Test-only, thread-local "crash after the pid file was published" switch:
// with it armed, `write_pid_identity` returns right after the pid file write
// and never writes the identity sidecar, exactly like a process that died
// between the two writes (or whose second write failed). Thread-local on
// purpose, like [`MOCK_PIDFD_ENOSYS`].
#[cfg(test)]
thread_local! {
    static SIMULATED_CRASH_AFTER_PID_PUBLISH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

// Test-only, thread-local publication barrier for the mount client: when
// armed with a path prefix, `run_client` parks inside its critical section —
// after the supervisor identity is published, before the instance lock is
// released — until the release marker appears, so a test can observe the
// published identity while the lock is still held and start a second real
// client that must serialize behind it. Thread-local on purpose, like
// [`MOCK_PIDFD_ENOSYS`]: other clients in the same process are unaffected.
#[cfg(test)]
thread_local! {
    static HOLD_AT_PUBLISH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Park inside the mount client's critical section for a test (see
/// [`HOLD_AT_PUBLISH`]): create the held marker, wait — bounded, so an
/// interrupted test cannot wedge a client for good — for the release marker,
/// then remove both markers.
#[cfg(test)]
fn hold_at_publish() {
    let Some(prefix) = HOLD_AT_PUBLISH.with(|hold| hold.borrow().clone()) else {
        return;
    };
    let mut held = prefix.as_os_str().to_os_string();
    held.push(".identity-published");
    let held = PathBuf::from(held);
    let mut release = prefix.as_os_str().to_os_string();
    release.push(".identity-release");
    let release = PathBuf::from(release);
    let _ = std::fs::write(&held, b"");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !release.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&held);
    let _ = std::fs::remove_file(&release);
}

// Test-only, thread-local barrier for the mount client: when armed with a
// state path, `run_client` parks inside its critical section immediately
// after the supervisor is spawned and BEFORE its identity is published, so a
// test can terminate the client exactly inside the spawn/publish window. The
// park ends on a release marker (the client continues normally) or on a kill
// marker, which crashes the client thread: the unwind closes every fd and
// releases the instance lock exactly like a SIGKILLed client process would.
// Thread-local on purpose, like [`HOLD_AT_PUBLISH`].
#[cfg(test)]
thread_local! {
    static HOLD_AT_SPAWN: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Park inside the mount client's critical section right after the supervisor
/// spawn for a test (see [`HOLD_AT_SPAWN`]): create the spawned marker, wait —
/// bounded, so an interrupted test cannot wedge a client for good — for a
/// release or kill marker, then honor whichever appeared.
#[cfg(test)]
fn hold_at_spawn(_state_path: &Path) {
    let Some(prefix) = HOLD_AT_SPAWN.with(|hold| hold.borrow().clone()) else {
        return;
    };
    let mut spawned = prefix.as_os_str().to_os_string();
    spawned.push(".spawned");
    let spawned = PathBuf::from(spawned);
    let mut release = prefix.as_os_str().to_os_string();
    release.push(".spawn-release");
    let release = PathBuf::from(release);
    let mut kill = prefix.as_os_str().to_os_string();
    kill.push(".spawn-kill");
    let kill = PathBuf::from(kill);
    let _ = std::fs::write(&spawned, b"");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if kill.exists() {
            let _ = std::fs::remove_file(&spawned);
            let _ = std::fs::remove_file(&kill);
            panic!(
                "simulated client crash: killed after the supervisor spawn, \
                 before the identity publication"
            );
        }
        if release.exists() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&spawned);
    let _ = std::fs::remove_file(&release);
}

/// Record a process identity for a managed pid file.
///
/// The pid file itself holds the bare pid — exactly what [`write_pid`] and
/// every previous version's writer and reader put there — while the
/// identity (boot id, start time and executable path, bound to the pid)
/// lives in a sidecar beside it. Writing the identity into the pid file as
/// `pid starttime exe` (the first cut of this change) is unreadable for a
/// rolled-back binary: the previous version's `read_pid` parses the whole
/// file as one integer, so the new-format file parses as *no pid at all*,
/// the slot looks free, and `mount --managed` clears the mount, overwrites
/// the records and starts a second supervisor over the still-running new
/// one (review of this PR). Keeping the pid file legacy-shaped — with the
/// identity in the sidecar — is what makes "revert the single commit"
/// safe: an old binary reads the same pid it always did, and its incumbent
/// logic sees the slot occupied.
///
/// The sidecar is additionally bound to the pid file's *current write*: it
/// records the write-generation representation of the pid file this writer
/// just produced (see [`pid_file_representation`]). A pid alone cannot pair
/// a sidecar with a pid file — after a rollback the legacy `write_pid`
/// rewrites only the pid file, and if the rewriting process happens to hold
/// the same numeric pid the recycled pid was recorded for, the stale sidecar
/// would read as a *verified* identity of a process that is gone;
/// `check_incumbent` would then report the slot idle while the legacy
/// incumbent still runs, and `stop` could delete the just-written Stopped
/// state under it (review of this PR). Any record whose sidecar cannot be
/// proven to describe the current pid-file write therefore degrades to
/// unverified: it occupies the slot while its pid lives, and is never
/// signaled.
///
/// The sidecar is also invalidated BEFORE the new pid-file representation is
/// published (review of this PR): resetting the generation can land on the
/// very spacing a stale sidecar recorded (a legacy bare rewrite resets the
/// count to 1), and a crash — or a failed write — after the pid file is
/// published but before the fresh sidecar lands would otherwise leave that
/// stale sidecar verifying the new write as the identity of a process that
/// no longer exists, so the reader would treat a still-alive worker as
/// exited and start a replacement over it. Removing it first closes that
/// window: every interrupted state here — old pid file without sidecar, new
/// pid file without sidecar — reads as unverified, and only the complete
/// (pid file + sidecar) pair can verify. An invalidation that fails (the run
/// directory no longer permits unlink, say) fails the whole write before the
/// pid file is touched, rather than publishing a reset generation under the
/// surviving stale sidecar (review of this PR).
fn write_pid_identity(path: &Path, identity: &ProcessIdentity) -> Result<(), Box<dyn Error>> {
    let spacing = next_pid_file_spacing(path, identity.pid);
    // Invalidate the stale sidecar BEFORE the pid file is rewritten, and let
    // the failure fail this write (review of this PR): the pid file itself
    // often stays writable when the sidecar cannot be removed (an unlink
    // needs write permission on the run directory, not on either file), and
    // publishing a reset generation over a surviving stale sidecar would let
    // the old record pair with — and verify — the new write whenever the
    // reset lands on the recorded spacing. `NotFound` is the ordinary
    // no-sidecar case.
    match std::fs::remove_file(identity_sidecar(path)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error.into());
        }
        _ => {}
    }
    std::fs::write(path, pid_file_representation(identity.pid, spacing))?;
    #[cfg(test)]
    if SIMULATED_CRASH_AFTER_PID_PUBLISH.with(std::cell::Cell::get) {
        // Test hook: stop right here, as if the writer crashed after
        // publishing the pid file and before the sidecar was written.
        return Ok(());
    }
    if identity.is_verifiable() {
        std::fs::write(
            identity_sidecar(path),
            format!(
                "{} {} {} {} {}\n",
                identity.pid, identity.starttime, identity.boot_id, spacing, identity.exe
            ),
        )?;
    }
    Ok(())
}

/// The pid-file content of write generation `spacing`: the bare pid, the
/// writer's own line terminator, and `spacing` trailing blanks.
///
/// This is the write-generation token the identity sidecar binds to
/// (review of this PR): the previous `(inode, mtime_ns)` binding was not a
/// unique write generation, because filesystem timestamps advance on the
/// kernel's coarse clock and an in-place rewrite inside the same
/// granularity tick shares the mtime with the write it replaced — exactly
/// the legacy same-pid rewrite the binding exists to catch. This
/// representation instead makes every write distinguishable by *content*:
///
/// * the previous version's `read_pid` parses the whole file with
///   `trim().parse::<i32>()`, and `trim` drops the trailing blanks, so the
///   representation stays exactly the pid for every rolled-back binary —
///   and for the shell tooling that reads the first field;
/// * the legacy `write_pid` writes `"{pid}\n"` and nothing else, so a
///   legacy rewrite — whenever it lands, same tick or not — never equals
///   this writer's representation (`spacing >= 1`) and always breaks the
///   pairing;
/// * two of this writer's writes never share a representation either
///   ([`next_pid_file_spacing`]), so a sidecar can only ever pair with the
///   one write it was written against.
fn pid_file_representation(pid: i32, spacing: usize) -> String {
    format!("{pid}\n{}", " ".repeat(spacing))
}

/// The write generation for the next write of `path`: one more than the
/// generation the file currently holds when it already is this writer's
/// representation of the very same pid, and the first generation otherwise
/// (a missing file, a legacy `"{pid}\n"` file, a different pid, or a
/// tampered file all start fresh at `1`).
///
/// The count only grows for consecutive rewrites of the *same* pid — the
/// supervisor writes its own record once and each worker gets a new pid —
/// so the padding stays minimal in normal operation while keeping every
/// write of the same file unique.
fn next_pid_file_spacing(path: &Path, pid: i32) -> usize {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return 1;
    };
    let Some(rest) = raw.strip_prefix(&format!("{pid}\n")) else {
        return 1;
    };
    if rest.is_empty() || !rest.bytes().all(|byte| byte == b' ') {
        return 1;
    }
    rest.len() + 1
}

/// The sidecar holding a pid file's process identity:
/// `<instance>.worker.pid` -> `<instance>.worker.pid.identity`.
fn identity_sidecar(pid_path: &Path) -> PathBuf {
    let mut name = pid_path.as_os_str().to_os_string();
    name.push(".identity");
    PathBuf::from(name)
}

/// Remove a managed pid file together with its identity sidecar, so a
/// later pid file can never be paired with a stale identity.
///
/// Lock-free on purpose: its callers either already hold the instance lock
/// (publication, teardown's final section, `finish`) or remove only their
/// own dying worker's record, which no concurrent generation depends on —
/// a live supervisor reaps and re-records its own workers.
fn remove_pid_record(pid_path: &Path) {
    let _ = std::fs::remove_file(pid_path);
    let _ = std::fs::remove_file(identity_sidecar(pid_path));
}

/// The instance lock path: the state file's own name with a `.lock` suffix.
fn instance_lock_path(state_path: &Path) -> PathBuf {
    let mut name = state_path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

/// The instance lock: a blocking exclusive `flock(2)` on
/// `<instance>.state.json.lock`, serializing the state transitions of one
/// managed instance (mount publication, the supervisor's self-recording
/// and final cleanup, and stop's final re-verify + record cleanup) with
/// each other (review of this PR).
///
/// What the lock closes: `stop`'s teardown re-verifies the incumbent
/// records and then deletes the Stopped marker, the pid records and the
/// state file; a concurrent `mount --managed` publishing a new generation
/// in the unlocked window between that re-verify and the cleanup had its
/// freshly written state and supervisor record deleted under a success
/// return, and a supervisor that had already read `Mounted` went on to
/// start a worker — the next mount then found no records and started a
/// second supervisor. Holding the lock across publication and across the
/// final re-verify + cleanup makes the two critical sections mutually
/// exclusive: a stop either observes the newcomer in its re-verify (and
/// refuses, keeping the records) or runs entirely before the publication.
///
/// The acquisition is blocking because the guarded sections are short and
/// bounded (publication waits at most the stop/orphan budgets, the final
/// section at most one unmount pass); the kernel releases the lock when
/// the holding process exits, so a crashed holder never wedges the
/// instance. Each site acquires the lock exactly once and never nests —
/// in particular, the lock is always released before a caller may enter
/// `teardown_instance`, whose final section takes it again.
struct InstanceLock {
    // Held for RAII: dropping closes the fd and releases the flock.
    _file: std::fs::File,
}

/// Acquire the instance lock belonging to `state_path`, blocking until any
/// concurrent holder (a publishing mount, a stopping teardown, the
/// supervisor's own cleanup) has finished its critical section.
fn acquire_instance_lock(state_path: &Path) -> Result<InstanceLock, Box<dyn Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;

    let lock_path = instance_lock_path(state_path);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| {
            format!(
                "failed to open instance lock '{}': {e}",
                lock_path.display()
            )
        })?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Err(format!(
            "failed to acquire instance lock '{}': {e}",
            lock_path.display()
        )
        .into());
    }
    Ok(InstanceLock { _file: file })
}

/// Read a managed pid file's record: the bare pid from the pid file —
/// parsed exactly the way every previous version parses it — plus, when a
/// well-formed identity sidecar naming that same pid **and** provably bound
/// to the current boot and the current pid-file write sits beside it, the
/// captured identity. Anything less — no sidecar, a malformed sidecar, one
/// whose pid does not match the pid file, one recorded on another system
/// boot (a leftover from before a reboot), one in an older format, or one
/// whose recorded write-generation representation no longer matches the
/// pid file's content (an older binary rewrote only the pid file during a
/// rollback window, possibly with a recycled pid equal to the recorded
/// one) — yields an unverified record, which callers treat as
/// never-signal and occupy-only.
///
/// The sidecar's executable path is the remainder after its first four
/// fields, so paths containing spaces round-trip — including at the edges:
/// only the writer's own line terminator is stripped, never payload
/// whitespace, so an executable path that legitimately ends in spaces or a
/// tab round-trips exactly and the instance's own identity check keeps
/// matching. A legacy file holding only a pid (with or without trailing
/// blanks) has no sidecar at all and yields the same unverified record.
fn read_pid_identity(path: &Path) -> Option<ProcessIdentity> {
    // The pid file keeps the legacy contract: the whole file is the pid.
    let raw = std::fs::read_to_string(path).ok()?;
    let pid = raw.trim().parse::<i32>().ok()?;
    let Some(sidecar_raw) = std::fs::read_to_string(identity_sidecar(path)).ok() else {
        // No sidecar: a legacy or degraded record — the pid occupies the
        // slot but can never be verified.
        return Some(ProcessIdentity::unverified(pid));
    };
    // Strip only the format's own terminator ('\n'), never payload
    // whitespace in the exe field: `trim_end` here would corrupt a recorded
    // path that ends in whitespace and make the instance's identity check
    // fail.
    let line = sidecar_raw.strip_suffix('\n').unwrap_or(&sidecar_raw);
    // pid starttime boot_id spacing exe — the boot id and the write
    // generation sit before the exe so the exe remains the free-form
    // remainder of the line.
    let mut fields = line.splitn(5, ' ');
    // A sidecar that does not name this very pid — absent fields, garbage,
    // or the pid of an earlier occupant (an older binary rewrote only the
    // pid file during a rollback window) — verifies nothing.
    let names_this_pid = fields
        .next()
        .and_then(|f| f.trim().parse::<i32>().ok())
        .is_some_and(|sidecar_pid| sidecar_pid == pid);
    if !names_this_pid {
        return Some(ProcessIdentity::unverified(pid));
    }
    let Some(starttime) = fields.next().and_then(|f| f.trim().parse::<u64>().ok()) else {
        return Some(ProcessIdentity::unverified(pid));
    };
    let bind_boot_id = fields.next().map(|f| f.trim().to_string());
    let bind_spacing = fields.next().and_then(|f| f.trim().parse::<usize>().ok());
    let Some(exe) = fields.next() else {
        return Some(ProcessIdentity::unverified(pid));
    };
    if starttime == 0 || exe.is_empty() {
        return Some(ProcessIdentity::unverified(pid));
    }
    // The boot gate: the sidecar may only verify a pid on the very boot it
    // was captured on. A boot id the sidecar does not carry (an older
    // format), or one that differs from the current boot (the machine
    // rebooted since the record was written — start ticks reset and inode
    // numbers recycle, so the record addresses processes of a previous
    // boot), cannot prove anything about the pid now: the record is
    // unverified (review of this PR).
    let bound_to_this_boot = bind_boot_id
        .as_deref()
        .is_some_and(|boot| boot == current_boot_id().unwrap_or(""));
    if !bound_to_this_boot {
        return Some(ProcessIdentity::unverified(pid));
    }
    // The generation gate: the sidecar may only verify this pid file while
    // the pid file's content is exactly the write-generation representation
    // the sidecar's writer produced. A generation the sidecar does not
    // carry, or content that no longer matches (the legacy writer rewrote
    // the pid file — same numeric pid or not, same timestamp tick or not),
    // cannot prove the sidecar describes the current write: the record is
    // unverified.
    let bound_to_this_write =
        bind_spacing.is_some_and(|spacing| raw == pid_file_representation(pid, spacing));
    if !bound_to_this_write {
        return Some(ProcessIdentity::unverified(pid));
    }
    Some(ProcessIdentity {
        pid,
        starttime,
        exe: exe.to_string(),
        boot_id: bind_boot_id.unwrap_or_default(),
    })
}

/// Signal a managed pid only after verifying, via its recorded identity,
/// that it is still the process we recorded. Pid files without a
/// verifiable identity (legacy pid-only records) and processes whose
/// identity no longer matches are never signaled.
fn signal_managed(recorded: Option<&ProcessIdentity>, sig: i32, role: &str) {
    let Some(identity) = recorded else { return };
    if !identity.is_verifiable() {
        warn!(
            pid = identity.pid,
            role, "pid file lacks a process identity; refusing to signal an unverified pid"
        );
        return;
    }
    if signal_identity(identity, sig) {
        info!(pid = identity.pid, role, sig, "signaled managed process");
    } else if identity_alive(identity) {
        warn!(
            pid = identity.pid,
            role,
            sig,
            "signal refused although the recorded process is verified alive (no reuse-proof \
             handle available); leaving it running and failing explicitly"
        );
    } else {
        warn!(
            pid = identity.pid,
            role,
            starttime = identity.starttime,
            exe = %identity.exe,
            "pid is not the recorded managed process; refusing to signal (pid reuse?)"
        );
    }
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

/// Check the incumbent supervisor recorded in `paths.supervisor_pid` and
/// decide whether a new supervisor may start for `normalized`.
///
/// Returns `Ok(true)` when the mount is already served by a live incumbent
/// (caller returns success), `Ok(false)` when the supervisor slot is free
/// (caller proceeds to start a fresh pair), and an error when takeover must
/// be refused.
///
/// Liveness is identity-aware: a verifiable record is alive only while the
/// exact recorded process still owns the pid, so a recycled pid of an
/// unrelated process does not block a restart. An unverified record — a
/// legacy bare pid, or an identity sidecar not provably bound to the
/// current pid-file write (a rollback window rewrote the pid file,
/// possibly with a recycled pid equal to the recorded one) — is the
/// deliberate exception: it cannot be verified, and unverifiable must
/// not be read as "safe to take over" (review of this PR): a
/// still-running pre-upgrade supervisor would otherwise be declared dead,
/// its mount cleared, its state and pid files overwritten, and a second
/// supervisor raced against it. While any process exists at the recorded
/// pid the slot counts as occupied — an active mount is reported as
/// already running, and anything else refuses takeover and asks the
/// operator to stop the old instance first. The unverified pid is never
/// signaled for this.
fn check_incumbent(paths: &ManagedPaths, normalized: &Path) -> Result<bool, Box<dyn Error>> {
    let Some(incumbent) = read_pid_identity(&paths.supervisor_pid) else {
        return Ok(false);
    };
    if !incumbent.is_verifiable() {
        if !pid_exists(incumbent.pid) {
            // Legacy record of a process that is gone: nothing to contend
            // with, safe to start fresh.
            return Ok(false);
        }
        if is_mount_ready(normalized) {
            info!(
                mountpoint = %normalized.display(),
                supervisor_pid = incumbent.pid,
                "managed mount already active (legacy pid file); nothing to do"
            );
            println!(
                "skillfs: managed mount already active at {}",
                normalized.display()
            );
            return Ok(true);
        }
        return Err(legacy_blocker_message(&paths.supervisor_pid, incumbent.pid));
    }
    if !identity_alive(&incumbent) {
        return Ok(false);
    }
    if is_mount_ready(normalized) {
        info!(
            mountpoint = %normalized.display(),
            supervisor_pid = incumbent.pid,
            "managed mount already active; nothing to do"
        );
        println!(
            "skillfs: managed mount already active at {}",
            normalized.display()
        );
        return Ok(true);
    }
    // Supervisor alive but mount not ready yet — wait for it to converge
    // rather than starting a competing supervisor.
    let deadline = Instant::now() + Duration::from_millis(READY_TIMEOUT_MS);
    while Instant::now() < deadline {
        if !identity_alive(&incumbent) {
            break; // incumbent died; fall through to (re)start below
        }
        if is_mount_ready(normalized) {
            println!(
                "skillfs: managed mount ready at {} (stop with: skillfs stop {})",
                normalized.display(),
                normalized.display()
            );
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if identity_alive(&incumbent) {
        return Err(format!(
            "managed supervisor (pid {}) is active for {} but the mount is not ready; \
             run 'skillfs stop {}' to recover before remounting",
            incumbent.pid,
            normalized.display(),
            normalized.display()
        )
        .into());
    }
    // Incumbent supervisor exited while we waited — safe to start fresh.
    Ok(false)
}

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

    // A live supervisor owns this instance, even if the mount is
    // momentarily down during remount/backoff recovery. Never spawn a second
    // supervisor for the same instance: it would overwrite state and race the
    // incumbent over the mountpoint. Liveness is identity-aware: the pid
    // file's recorded identity must still match, so a recycled pid of an
    // unrelated process does not block a restart — except for legacy
    // bare-pid records, which cannot be verified and therefore never
    // prove the slot free while their pid exists (see `check_incumbent`).
    //
    // The gate, the record cleanup and the publication of the new
    // generation's state and supervisor record form one critical section
    // under the instance lock, so a concurrent `stop`'s final re-verify +
    // cleanup can never interleave with this publication: either it
    // observes the freshly published generation and refuses, or it runs
    // entirely before this mount (review of this PR). The lock is released
    // before the readiness wait, and the failure-path teardown below takes
    // it again itself.
    {
        let _instance_lock = acquire_instance_lock(&paths.state)?;
        if check_incumbent(&paths, &normalized)? {
            return Ok(());
        }

        // A killed supervisor can leave behind an orphan worker that still
        // serves the mount. Do not start a competing worker over it:
        // terminate the orphan and clear the mountpoint, then create a
        // fresh supervised pair below. The orphan is only signaled through
        // its recorded identity — a recycled pid of an unrelated process
        // must not be terminated. A legacy bare-pid worker record naming a
        // live pid is unverifiable, so replacing it would be a takeover of
        // a possibly-live pre-upgrade worker: refuse instead.
        if let Some(blocker) = live_legacy_blocker(&paths.worker_pid) {
            return Err(blocker);
        }
        if let Some(orphan) = read_pid_identity(&paths.worker_pid) {
            if identity_alive(&orphan) {
                warn!(
                    worker_pid = orphan.pid,
                    mountpoint = %normalized.display(),
                    "found orphan managed worker without a live supervisor; replacing it"
                );
                replace_orphan_worker(&orphan, Duration::from_millis(STOP_TIMEOUT_MS))?;
            }
            remove_pid_record(&paths.worker_pid);
        }
        if is_mounted(&normalized) {
            clear_mount(&normalized)?;
        }
        remove_pid_record(&paths.supervisor_pid);

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

        let (mut supervisor_child, supervisor_handshake) =
            spawn_supervisor(&worker_program, &instance_id, &paths)?;

        // Test-only: park between the supervisor spawn and the identity
        // publication so a test can terminate the client exactly inside the
        // spawn/publish window (see [`HOLD_AT_SPAWN`]).
        #[cfg(test)]
        hold_at_spawn(&paths.state);

        // Publish the supervisor's identity inside this same critical
        // section, before the lock is released (review of this PR). The
        // child is parked on the startup handshake pipe meanwhile and
        // proceeds only once this publication is durably on disk (the GO
        // byte below), so a concurrent `mount --managed` can never pass the
        // gate above against a not-yet-visible record — releasing the lock
        // with no record would let it start a second supervisor, and both
        // would then read Mounted and start workers. The child is ours and
        // unreaped, so its pid cannot have been reused and the captured
        // identity is exact; the supervisor skips its own rewrite when the
        // record already is its identity (see `run_supervisor`).
        match ProcessIdentity::capture(supervisor_child.id() as i32) {
            Some(identity) => {
                if let Err(error) = write_pid_identity(&paths.supervisor_pid, &identity) {
                    // Publication failed: the child never receives the GO
                    // byte, so dropping the handshake ends it (EOF) before it
                    // records or serves anything (review of this PR). Fail
                    // the mount — releasing the lock here over a running,
                    // unrecorded supervisor is exactly the spawn/publish race
                    // the handshake closes.
                    drop(supervisor_handshake);
                    wait_child_exit(
                        &mut supervisor_child,
                        Duration::from_millis(HANDSHAKE_EXIT_BUDGET_MS),
                    );
                    if let Err(e) = teardown_instance(&paths, &normalized) {
                        warn!(error = %e, "failed to clean up after the supervisor identity publication failure");
                    }
                    return Err(format!(
                        "failed to publish supervisor identity: {error}; the startup \
                         supervisor was stopped before it could serve"
                    )
                    .into());
                }
            }
            None => {
                warn!(
                    supervisor_pid = supervisor_child.id(),
                    "failed to capture supervisor identity from /proc; recording the pid only \
                     — the slot stays occupied but unverifiable until the supervisor records itself"
                );
                let _ = write_pid(&paths.supervisor_pid, supervisor_child.id());
            }
        }

        // The identity is durably on disk: the child may serve now (review
        // of this PR).
        supervisor_handshake.release();

        // Test-only: park before the critical section ends so a test can
        // observe the published supervisor identity while the instance lock
        // is still held (and serialize a second real client behind it).
        #[cfg(test)]
        hold_at_publish();
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
        if let Some(supervisor) = read_pid_identity(&paths.supervisor_pid) {
            if !identity_alive(&supervisor) {
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

/// Terminate an orphan worker (a live worker whose supervisor is gone) and
/// wait for its confirmed exit, so the replacement pair started afterwards
/// cannot contend with it for the mountpoint.
///
/// The orphan is only ever signaled through its verified identity and only
/// through a pidfd. When the signal cannot be delivered (no reuse-proof
/// handle — see [`signal_identity`]), the orphan is left running, its pid
/// record is kept by the caller, and an explicit error is returned: starting
/// a replacement worker over one we could not stop would pit two workers
/// against the same mount. The wait budget is a parameter for tests.
fn replace_orphan_worker(
    orphan: &ProcessIdentity,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    signal_identity(orphan, libc::SIGTERM);
    let deadline = Instant::now() + timeout;
    while identity_alive(orphan) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if identity_alive(orphan) {
        signal_identity(orphan, libc::SIGKILL);
        let grace = Instant::now() + Duration::from_millis(KILL_GRACE_MS);
        while identity_alive(orphan) && Instant::now() < grace {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if identity_alive(orphan) {
        return Err(format!(
            "orphan managed worker (pid {}) could not be terminated — its signal was \
             refused for safety, its pid record was kept and no replacement was started; \
             stop the process manually, then remount",
            orphan.pid
        )
        .into());
    }
    Ok(())
}

// Test-only override of the executable the supervisor child runs. The
// handshake semantics under test live in the real `run_supervisor`, which
// only the package binary executes; cargo test builds that binary alongside
// this test harness (see [`tests::built_skillfs_binary`]). Production always
// runs the worker program itself.
#[cfg(test)]
thread_local! {
    static SUPERVISOR_CHILD_PROGRAM_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// The executable the supervisor child runs: the worker program, except in
/// tests that substitute a real `skillfs` binary (see the override above).
fn supervisor_child_program(program: &Path) -> PathBuf {
    #[cfg(test)]
    if let Some(override_exe) = SUPERVISOR_CHILD_PROGRAM_OVERRIDE.with(|o| o.borrow().clone()) {
        return override_exe;
    }
    program.to_path_buf()
}

/// The parent side of the supervisor startup handshake (review of this PR).
///
/// [`spawn_supervisor`] hands the child the pipe's read end on fd 3 and keeps
/// this write end. [`Self::release`] is written only after the child's
/// identity is durably on disk; dropping the guard closes the pipe, and the
/// child — still parked on its first read — sees EOF and exits without
/// recording or serving anything. A parent killed between the spawn and the
/// publication has its fds closed by the kernel, which is the same EOF: an
/// orphaned child can never outlive that window unrecorded, so a concurrent
/// mount client never finds a free slot behind a running supervisor.
struct SupervisorHandshake {
    write_fd: libc::c_int,
}

impl SupervisorHandshake {
    /// Let the child proceed: its identity is durably published.
    fn release(&self) {
        // A one-byte write into a pipe the child is actively reading never
        // blocks; a dead child only turns this into EPIPE (SIGPIPE is
        // ignored by the Rust runtime), and the readiness wait reports the
        // mount as failed either way.
        let byte = HANDSHAKE_GO_BYTE;
        let written =
            unsafe { libc::write(self.write_fd, &byte as *const u8 as *const libc::c_void, 1) };
        if written != 1 {
            warn!(
                error = %std::io::Error::last_os_error(),
                "failed to signal the supervisor startup handshake; \
                 falling back to the readiness wait"
            );
        }
    }
}

impl Drop for SupervisorHandshake {
    fn drop(&mut self) {
        unsafe { libc::close(self.write_fd) };
    }
}

/// Spawn the supervisor detached in its own session so a restart of the
/// caller's process group does not tear it down.
///
/// Returns the spawned, un-reaped [`std::process::Child`] — its pid cannot
/// have been reused while the caller holds the handle, so the caller can
/// capture and publish the child's exact identity from `/proc` (review of
/// this PR) — together with the parent side of the startup handshake: the
/// caller must [`SupervisorHandshake::release`] once the child's identity is
/// durably published, or drop the guard to end the child (review of this
/// PR).
fn spawn_supervisor(
    program: &Path,
    instance_id: &str,
    paths: &ManagedPaths,
) -> Result<(std::process::Child, SupervisorHandshake), Box<dyn Error>> {
    use std::os::unix::process::CommandExt;

    // The startup handshake pipe (review of this PR): the child parks on its
    // first read of fd 3 until the parent published its identity — or sees
    // EOF when the parent died or failed to publish, and exits then without
    // recording or serving (see `run_supervisor`).
    let mut pipe_fds: [libc::c_int; 2] = [0; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        return Err(format!(
            "failed to create the supervisor startup pipe: {}",
            std::io::Error::last_os_error()
        )
        .into());
    }
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);

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

    let mut cmd = std::process::Command::new(supervisor_child_program(program));
    cmd.arg("supervise")
        .arg("--instance")
        .arg(instance_id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(sup_log))
        .stderr(std::process::Stdio::from(sup_log_err));

    // Detach: new session + new process group, no controlling terminal. The
    // handshake read end is dup'ed onto the reserved fd first and both
    // original pipe fds are closed in the child, so the child exec's with
    // exactly one handle on the pipe: fd 3.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(read_fd, SUPERVISOR_HANDSHAKE_FD) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let _ = libc::close(read_fd);
            let _ = libc::close(write_fd);
            Ok(())
        });
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return Err(format!("failed to spawn supervisor: {e}").into());
        }
    };
    // The child owns its dup'ed copy of the read end now.
    unsafe { libc::close(read_fd) };
    info!(
        supervisor_pid = child.id(),
        instance = %instance_id,
        "spawned detached managed supervisor"
    );
    Ok((child, SupervisorHandshake { write_fd }))
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

/// The outcome of the supervisor's startup handshake with its publishing
/// client (review of this PR).
enum ParentHandshake {
    /// The client published our identity; serve the mount.
    Proceed,
    /// The client died or failed to publish: no record exists for us and the
    /// slot reads free — exit without recording or serving.
    ParentLost,
}

/// Block on the startup handshake pipe the client handed us on fd 3 (see
/// [`spawn_supervisor`]).
///
/// The handshake fd is recognized, not assumed: only a pipe (FIFO) on fd 3
/// can be our client's handshake. A hand-started supervisor — or one spawned
/// by a rolled-back client that never dup'ed a pipe — may inherit anything
/// there (a data file already at EOF, /dev/null, a character device); none of
/// that is a handshake, so the supervisor serves as before instead of
/// misreading a stray EOF as its client's death. On a real handshake pipe,
/// the GO byte means the identity is durably on disk; EOF means the client is
/// gone without publishing — the same EOF a SIGKILLed client produces, since
/// the kernel closes its fds. Anything else refuses to serve: a supervisor
/// without a trustworthy answer about its own record must not start.
fn await_publish_handshake() -> ParentHandshake {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(SUPERVISOR_HANDSHAKE_FD, &mut stat) } != 0 {
        // No fd there at all: no handshake was set up — serve as before.
        return ParentHandshake::Proceed;
    }
    if stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
        // Not a pipe: an inherited fd of some other kind — no handshake.
        unsafe { libc::close(SUPERVISOR_HANDSHAKE_FD) };
        return ParentHandshake::Proceed;
    }
    let mut byte = [0u8; 1];
    loop {
        let read = unsafe {
            libc::read(
                SUPERVISOR_HANDSHAKE_FD,
                byte.as_mut_ptr().cast::<libc::c_void>(),
                1,
            )
        };
        if read == 1 {
            unsafe { libc::close(SUPERVISOR_HANDSHAKE_FD) };
            return if byte[0] == HANDSHAKE_GO_BYTE {
                ParentHandshake::Proceed
            } else {
                ParentHandshake::ParentLost
            };
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EBADF) => return ParentHandshake::Proceed,
            // EOF (0) or an unexpected error on the handshake pipe itself.
            _ => return ParentHandshake::ParentLost,
        }
    }
}

/// Entry point for the detached supervisor process.
pub fn run_supervisor(instance_id: &str) -> Result<(), Box<dyn Error>> {
    // Startup handshake with the publishing client (review of this PR): the
    // client spawns us with the handshake pipe on fd 3 and writes the GO byte
    // only after our identity is durably published, inside the same critical
    // section that guards the instance records. EOF on that pipe — the client
    // died, or its publication failed — means no supervisor record exists for
    // us and none ever will: exit immediately, before recording or serving,
    // so a concurrent mount client's empty-slot gate reflects reality instead
    // of racing a running but unrecorded supervisor. No pipe on fd 3 at all
    // means we were started by hand or by a rolled-back client that knows
    // nothing about the handshake — serve as before.
    match await_publish_handshake() {
        ParentHandshake::Proceed => {}
        ParentHandshake::ParentLost => {
            info!(
                instance = %instance_id,
                "client disappeared before publishing the supervisor identity; \
                 exiting without recording or serving"
            );
            return Ok(());
        }
    }

    let paths = ManagedPaths::new(instance_id);
    secure_runtime_dir()?;
    let state = ManagedState::load(&paths.state)
        .map_err(|e| format!("failed to load managed state for '{instance_id}': {e}"))?;
    let mountpoint = PathBuf::from(&state.mountpoint);

    // Record our own identity (pid + boot + /proc start time + /proc exe)
    // so a later `stop` can prove this pid is still us before signaling
    // it. The write is a state transition of the instance — a concurrent
    // `stop`'s final re-verify must see either no record or this complete
    // one, never delete it as part of a generation it re-verified before
    // this publication — so it happens under the instance lock.
    //
    // The publishing client, however, already wrote exactly this identity
    // inside its own critical section, before releasing the lock (review
    // of this PR). When the record on disk already is our own complete
    // identity, rewriting it would only re-take a lock a concurrent mount
    // client may legitimately hold while waiting for THIS mount to become
    // ready — it saw our record and is waiting for readiness — and would
    // stall exactly that readiness. Skip the rewrite in that case;
    // otherwise (the client could not capture our identity, or a
    // concurrent stop cleaned the record in between) take the lock and
    // write, as before.
    let supervisor_identity =
        ProcessIdentity::capture(std::process::id() as i32).ok_or_else(|| {
            "failed to read own /proc identity for the supervisor pid file".to_string()
        })?;
    if read_pid_identity(&paths.supervisor_pid).as_ref() != Some(&supervisor_identity) {
        {
            let _instance_lock = acquire_instance_lock(&paths.state)?;
            write_pid_identity(&paths.supervisor_pid, &supervisor_identity)?;
        }
    }
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
        // Record the worker's identity at fork time. The child is ours and
        // unreaped, so its pid cannot have been reused yet and the captured
        // start time and exe path are exact. teardown_instance uses this
        // file to signal the worker directly when the supervisor is gone.
        // Without a captured identity the file degrades to a pid-only
        // record that stop refuses to signal, and only unmount-based
        // teardown remains.
        match ProcessIdentity::capture(worker_pid as i32) {
            Some(identity) => {
                if let Err(error) = write_pid_identity(&paths.worker_pid, &identity) {
                    warn!(
                        error = %error,
                        "failed to write worker pid file; stop cannot signal this worker directly if the supervisor dies"
                    );
                }
            }
            None => {
                warn!(
                    "failed to capture worker identity from /proc; recording the pid only — \
                     stop will refuse to signal this worker directly"
                );
                let _ = write_pid(&paths.worker_pid, worker_pid);
            }
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
                // Signaling by pid is safe here by construction: the
                // worker is our direct, unreaped child, so its pid cannot
                // have been reused. wait_child_timeout below escalates
                // through the Child handle, which is reuse-proof anyway.
                send_signal(worker_pid as i32, libc::SIGTERM);
                let _ = wait_child_timeout(&mut child, Duration::from_millis(STOP_TIMEOUT_MS));
                remove_pid_record(&paths.worker_pid);
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
        remove_pid_record(&paths.worker_pid);

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

/// Wait up to `timeout` for `child` to exit, best-effort: the failed
/// publication path uses this to reap the supervisor its handshake just
/// terminated; the child exits on the pipe EOF, and nothing depends on the
/// reap completing.
fn wait_child_exit(child: &mut std::process::Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
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
///
/// The record/state removals run under the instance lock: this exiting
/// supervisor must not delete records a concurrent generation published
/// (a mount that took over after this supervisor's Stopped state), exactly
/// like a stopping teardown's final section.
fn finish(paths: &ManagedPaths, mountpoint: &Path) {
    if is_mounted(mountpoint) {
        if let Err(e) = clear_mount(mountpoint) {
            warn!(error = %e, "failed to unmount during supervisor cleanup");
        }
    }
    {
        // Best effort: cleanup must run even when the lock cannot be
        // acquired (the warning records the degraded, unlocked removal).
        let _instance_lock = acquire_instance_lock(&paths.state)
            .inspect_err(|error| {
                warn!(
                    error = %error,
                    "failed to acquire the instance lock during supervisor cleanup"
                )
            })
            .ok();
        remove_pid_record(&paths.worker_pid);
        remove_pid_record(&paths.supervisor_pid);
        let _ = std::fs::remove_file(&paths.state);
    }
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
    teardown_instance_with_timeout(paths, mountpoint, Duration::from_millis(STOP_TIMEOUT_MS))
}

/// The teardown body, with the wait budget as a parameter so tests can
/// exercise the timeout paths without sleeping the production budget.
fn teardown_instance_with_timeout(
    paths: &ManagedPaths,
    mountpoint: &Path,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    // Clear desired state first so the supervisor will not remount even if a
    // remount races with our signal. This state write is also the cooperative
    // stop for a legacy bare-pid incumbent: its supervisor loop polls the
    // state file (already true of pre-upgrade binaries) and exits once it
    // reads Stopped, which is the only safe way to ask an unverified pid to
    // leave — it is never signaled.
    if let Ok(mut state) = ManagedState::load(&paths.state) {
        state.desired_state = DesiredState::Stopped;
        let _ = state.save(&paths.state);
    }

    // Prefer signaling the supervisor: it owns the worker and unmounts it
    // cleanly. Fall back to signaling the worker directly. Both pids are
    // only signaled through their recorded identity, so a recycled pid —
    // whether an unrelated program, a same-named binary at another path,
    // or another skillfs instance — is never signaled.
    let supervisor = read_pid_identity(&paths.supervisor_pid);
    let worker = read_pid_identity(&paths.worker_pid);

    signal_managed(supervisor.as_ref(), libc::SIGTERM, "supervisor");
    if !managed_alive(supervisor.as_ref()) {
        signal_managed(worker.as_ref(), libc::SIGTERM, "worker");
    }

    // Wait for the managed processes to exit. The supervisor unmounts cleanly
    // on SIGTERM, so once it is gone the mount is normally already down; any
    // remaining endpoint is cleared explicitly below. Pids whose identity no
    // longer matches count as gone: an unrelated reuser must not delay
    // teardown waiting for a process we refuse to signal. Legacy bare-pid
    // records cannot be verified at all, so the pid still existing is the
    // only liveness signal available — the wait covers them too (bounded by
    // `timeout`): treating them as gone immediately let `stop` delete the
    // state and pid files and return success while the pre-upgrade incumbent
    // was still shutting down, and the next mount then raced a live
    // supervisor (review of this PR).
    let mut deadline = Instant::now() + timeout;
    let mut forced = false;
    loop {
        if incumbents_gone(&[supervisor.as_ref(), worker.as_ref()]) {
            break;
        }
        if Instant::now() >= deadline {
            if forced {
                break;
            }
            warn!("teardown timed out waiting for clean shutdown; forcing");
            signal_managed(supervisor.as_ref(), libc::SIGKILL, "supervisor");
            signal_managed(worker.as_ref(), libc::SIGKILL, "worker");
            forced = true;
            // Grace for the forced exits to land before the final verdict.
            // A signal refused for safety (no pidfd) stays refused here, so
            // the holdout check below catches it.
            deadline = Instant::now() + Duration::from_millis(KILL_GRACE_MS);
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Anything still running at this point survived both the cooperative
    // stop and every signal we were willing to send: a verified process
    // whose signal was refused (pidfd unavailable — there is no bare-kill
    // fallback), or a legacy bare-pid incumbent that ignored the Stopped
    // state within the wait. Completing teardown anyway — deleting the
    // records, unmounting, reporting success — would let the next mount
    // race that live incumbent with a second supervisor. Keep every record
    // (the state already says Stopped) and fail explicitly instead.
    let holdouts: Vec<String> = [supervisor.as_ref(), worker.as_ref()]
        .into_iter()
        .filter_map(|recorded| match recorded {
            Some(identity) if identity.is_verifiable() && identity_alive(identity) => {
                Some(format!(
                    "pid {} (identity verified but signal refused)",
                    identity.pid
                ))
            }
            Some(legacy) if !legacy.is_verifiable() && unverified_pid_occupied(legacy) => {
                Some(format!(
                    "legacy pid {} (identity unverifiable; never signaled)",
                    legacy.pid
                ))
            }
            _ => None,
        })
        .collect();
    if !holdouts.is_empty() {
        return Err(format!(
            "managed stop for {} did not complete: {} still running and cannot be \
             addressed safely. The desired state is already 'stopped' and all state/pid \
             records were kept — stop the listed process(es) manually (kill, or the \
             previous version's 'skillfs stop'), then run 'skillfs stop {}' again",
            mountpoint.display(),
            holdouts.join("; "),
            mountpoint.display()
        )
        .into());
    }

    // Re-verify the incumbent records and delete the Stopped marker and
    // the pid records as ONE critical section under the instance lock.
    // Everything above acted on records read at teardown start; a writer
    // active during the stop wait — a legacy incumbent coming back, or a
    // new generation recording itself — can have replaced either record
    // since, and completing cleanup on the strength of the stale read
    // would free the slot (and drop the Stopped marker) under a live
    // incumbent. The lock additionally closes the remaining window a
    // re-verify alone cannot: a concurrent `mount --managed` publishing
    // its new state and supervisor record *between* the re-verify and the
    // cleanup had that freshly published generation deleted under a
    // success return, and its supervisor (having already read Mounted)
    // went on to start a worker — the next mount then raced it with a
    // second supervisor (review of this PR). Publication holds the same
    // lock, so the stopper here either re-verifies after the publication
    // (and refuses below) or cleans up entirely before it.
    let _instance_lock = acquire_instance_lock(&paths.state)?;
    let supervisor_now = read_pid_identity(&paths.supervisor_pid);
    let worker_now = read_pid_identity(&paths.worker_pid);
    let mid_stop: Vec<String> = [
        mid_stop_incumbent("supervisor", supervisor.as_ref(), supervisor_now.as_ref()),
        mid_stop_incumbent("worker", worker.as_ref(), worker_now.as_ref()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !mid_stop.is_empty() {
        return Err(format!(
            "managed stop for {} cannot complete: the incumbent record(s) changed \
             mid-stop ({}) and still name a live process, so this teardown must not \
             delete the stopped marker or the pid records it did not act on. The \
             desired state is already 'stopped' and all records were kept — run \
             'skillfs stop {}' again for the new incumbent",
            mountpoint.display(),
            mid_stop.join("; "),
            mountpoint.display()
        )
        .into());
    }

    // Ensure the mount is gone, clearing any stale/dead endpoint a killed
    // worker left behind. The error is surfaced after state cleanup.
    let unmount_result = if is_mounted(mountpoint) {
        clear_mount(mountpoint)
    } else {
        Ok(())
    };

    // Best-effort final cleanup of any residual state files.
    remove_pid_record(&paths.worker_pid);
    remove_pid_record(&paths.supervisor_pid);
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

    // The no-state fast path removes records (and possibly unmounts) — a
    // state transition like any other, so it runs under the instance lock:
    // a mount publishing its state + records between the existence check
    // and these removals must not have them deleted by a stop that never
    // saw them. The lock is released before `teardown_instance`, whose own
    // final section takes it again.
    {
        let _instance_lock = acquire_instance_lock(&paths.state)?;
        let had_state = paths.state.exists();

        // No managed instance recorded for this mountpoint. `stop` is still
        // a reliable teardown, so unmount immediately instead of waiting the
        // full stop timeout on processes that do not exist (handles stale or
        // non-managed dead mounts).
        if !had_state {
            remove_pid_record(&paths.worker_pid);
            remove_pid_record(&paths.supervisor_pid);
            match classify_mount(&normalized) {
                MountState::NotMounted => {
                    println!(
                        "skillfs: no managed mount at {} (already stopped)",
                        normalized.display()
                    );
                }
                // Present (ready or a stale/dead endpoint): unmount it,
                // surfacing an error if cleanup cannot complete.
                _ => {
                    clear_mount(&normalized)?;
                    println!("skillfs: unmounted {}", normalized.display());
                }
            }
            return Ok(());
        }
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

    fn sample_state(instance_id: &str) -> ManagedState {
        ManagedState {
            schema_version: STATE_SCHEMA_VERSION,
            instance_id: instance_id.to_string(),
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
        }
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

    // -----------------------------------------------------------------
    // Process identity: recording, parsing, and reuse-proof signaling
    // -----------------------------------------------------------------

    /// A crafted `/proc/<pid>/stat` line whose comm contains spaces and
    /// parentheses; field 22 (starttime) is `987654321`. Everything from
    /// the state field through field 21 is filler, so a parser that
    /// miscounts fields — or one that splits before the last `)` — fails.
    const CRAFTED_STAT: &str = "1234 (weird ) comm (v2)) S f3 f4 f5 f6 f7 f8 f9 f10 f11 f12 f13 f14 f15 f16 f17 f18 f19 f20 987654321 f23 f24";

    #[test]
    fn stat_starttime_reads_field_22_after_the_last_paren() {
        assert_eq!(stat_starttime(CRAFTED_STAT), Some(987654321));
        // A comm without special characters parses the same way (state
        // token 0 ... starttime token 19).
        assert_eq!(
            stat_starttime("42 (sleep) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 777 21 22"),
            Some(777)
        );
        // Real /proc: the parser must read our own start time.
        let real = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
        assert!(stat_starttime(&real).is_some_and(|t| t > 0));
        // No closing paren: not a stat line.
        assert_eq!(stat_starttime("no parens here"), None);
    }

    #[test]
    fn same_process_binds_starttime_and_full_exe() {
        let recorded = ProcessIdentity {
            pid: 42,
            starttime: 1111,
            exe: "/usr/bin/skillfs".into(),
            boot_id: this_boot(),
        };
        // Exact match.
        assert!(same_process(&recorded, 1111, "/usr/bin/skillfs"));
        // A replaced binary keeps " (deleted)" on the live exe side.
        assert!(same_process(&recorded, 1111, "/usr/bin/skillfs (deleted)"));
        // A recorded value that already ends in the marker text (the exe
        // was unlinked before capture, or the file name legitimately ends
        // in it) matches its exact live value...
        let recorded_deleted = ProcessIdentity {
            exe: "/usr/bin/skillfs (deleted)".into(),
            ..recorded.clone()
        };
        assert!(same_process(
            &recorded_deleted,
            1111,
            "/usr/bin/skillfs (deleted)"
        ));
        // ...and the kernel appending ITS marker to such a name —
        // "... (deleted) (deleted)" — still matches (one live marker over
        // the recorded value). What must NOT match is a live value with
        // the marker stripped: that is a different path, and stripping one
        // marker from each side (the earlier behavior) wrongly equated
        // them (review of this PR).
        assert!(same_process(
            &recorded_deleted,
            1111,
            "/usr/bin/skillfs (deleted) (deleted)"
        ));
        assert!(
            !same_process(&recorded_deleted, 1111, "/usr/bin/skillfs"),
            "a live path without the marker is a different path, not the \
             recorded file with its own name stripped"
        );
        // Two live markers over a marker-free record is equally foreign.
        assert!(!same_process(
            &recorded,
            1111,
            "/usr/bin/skillfs (deleted) (deleted)"
        ));
        // Same exe path but a later process creation (another skillfs
        // instance's worker recycling the pid): not the same process.
        assert!(!same_process(&recorded, 1112, "/usr/bin/skillfs"));
        // Same basename at a different path: not the recorded program.
        assert!(!same_process(&recorded, 1111, "/opt/other/skillfs"));
        // Unverifiable (legacy pid-only) records never match anything.
        assert!(!same_process(
            &ProcessIdentity::unverified(42),
            1111,
            "/usr/bin/skillfs"
        ));
    }

    /// The current boot id as a owned string, for records the tests expect
    /// to be bound to the running boot.
    fn this_boot() -> String {
        current_boot_id().expect("/proc boot id").to_string()
    }

    #[test]
    fn pid_identity_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.worker.pid");
        // The exe path contains a space; the format must preserve it.
        let identity = ProcessIdentity {
            pid: 4242,
            starttime: 123456,
            exe: "/opt/skill fs/skillfs".into(),
            boot_id: this_boot(),
        };
        write_pid_identity(&path, &identity).unwrap();
        assert_eq!(read_pid_identity(&path), Some(identity));
    }

    #[test]
    fn read_pid_identity_tolerates_legacy_pid_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.worker.pid");
        // Writer's terminator, no terminator, and stray trailing blanks
        // around a bare pid: all parse as the same unverified record.
        for raw in ["4242\n", "4242", "4242 \n"] {
            std::fs::write(&path, raw).unwrap();
            let identity = read_pid_identity(&path).unwrap();
            assert_eq!(identity.pid, 4242, "raw {raw:?}");
            assert!(!identity.is_verifiable(), "raw {raw:?}");
        }
    }

    #[test]
    fn pid_identity_file_round_trips_exe_trailing_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.worker.pid");
        // The recorded executable path ends in payload whitespace (a
        // directory whose name ends in a space, a trailing tab): the
        // reader must strip only the writer's own '\n' terminator, never
        // the payload, or this instance's own identity check would fail
        // and `stop` could no longer stop it.
        for exe in [
            "/opt/skill fs/skillfs ",
            "/opt/skillfs\t",
            "/opt/skillfs  ",
            " /leading/space/skillfs",
        ] {
            let identity = ProcessIdentity {
                pid: 4242,
                starttime: 123456,
                exe: exe.into(),
                boot_id: this_boot(),
            };
            write_pid_identity(&path, &identity).unwrap();
            assert_eq!(read_pid_identity(&path), Some(identity), "exe {exe:?}");
        }
    }

    /// The pid-file reader of the version before this change — verbatim
    /// from current main: the whole file parses as one integer. During a
    /// version rollback the OLD binary reads the NEW supervisor's pid file
    /// with exactly this code, and a `None` there frees the slot for a
    /// second supervisor over the still-running one. The new record must
    /// keep satisfying it.
    fn previous_version_read_pid(path: &Path) -> Option<i32> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
    }

    #[test]
    fn pid_file_stays_readable_by_the_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.supervisor.pid");
        let identity = ProcessIdentity {
            pid: 4242,
            starttime: 123456,
            exe: "/opt/skill fs/skillfs".into(),
            boot_id: this_boot(),
        };
        write_pid_identity(&path, &identity).unwrap();
        assert_eq!(
            read_pid_identity(&path),
            Some(identity),
            "the new reader must still see the full identity"
        );
        assert_eq!(
            previous_version_read_pid(&path),
            Some(4242),
            "a rolled-back binary must still read the incumbent's pid from \
             the pid file"
        );
        // The degraded (pid-only) record keeps the same contract.
        write_pid(&path, 4243).unwrap();
        assert_eq!(previous_version_read_pid(&path), Some(4243));
    }

    #[test]
    fn stale_identity_sidecar_never_verifies_a_new_pid_file() {
        // Rollback sequel: the old binary runs again and rewrites the pid
        // file with its own bare pid (its writer knows nothing of the
        // sidecar). The leftover sidecar names a different pid, so the
        // upgraded reader must not pair them: the record is unverified —
        // occupying the slot while that pid lives, never signaled. And the
        // record cleanup removes the sidecar together with the pid file,
        // so no later record can inherit it either.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.supervisor.pid");
        write_pid_identity(
            &path,
            &ProcessIdentity {
                pid: 4242,
                starttime: 123456,
                exe: "/opt/skillfs".into(),
                boot_id: this_boot(),
            },
        )
        .unwrap();
        assert!(identity_sidecar(&path).exists());
        std::fs::write(&path, "4299\n").unwrap(); // the old binary's rewrite

        let record = read_pid_identity(&path).unwrap();
        assert_eq!(record.pid, 4299);
        assert!(
            !record.is_verifiable(),
            "a sidecar naming another pid must not verify this pid file"
        );

        remove_pid_record(&path);
        assert!(!path.exists());
        assert!(!identity_sidecar(&path).exists());
        // The degraded pid-only write drops the sidecar as well.
        write_pid(&path, 4243).unwrap();
        assert!(!identity_sidecar(&path).exists());
    }

    #[test]
    fn stale_sidecar_after_a_legacy_rewrite_reads_unverified() {
        // Round-3 review scenario: the old binary's `write_pid` rewrites ONLY
        // the pid file — with the SAME numeric pid (the recycled pid of the
        // new-generation supervisor). The leftover sidecar still names that
        // pid, so pid equality alone reads the record as a verified identity
        // of a process that is gone, and the incumbent gates then treat the
        // slot as idle while the legacy process at that pid still runs. The
        // sidecar must instead be bound to the current pid-file write: a
        // record whose write generation cannot be proven reads unverified.
        let dir = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let path = dir.path().join("inst.supervisor.pid");

        // The new writer records the pair (pid file + identity sidecar).
        write_pid_identity(&path, &live_identity_of(decoy.id())).unwrap();
        assert!(
            read_pid_identity(&path).unwrap().is_verifiable(),
            "the record of the writer's own generation must verify"
        );

        // The legacy writer's bare rewrite, same numeric pid. Sleep first so
        // the pid file's mtime crosses at least one timestamp-granularity
        // tick of the filesystem — the rewrite must be observable.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, format!("{}\n", decoy.id())).unwrap();

        let record = read_pid_identity(&path).unwrap();
        assert_eq!(record.pid, decoy.id() as i32);
        assert!(
            !record.is_verifiable(),
            "a sidecar that cannot be proven to belong to the current \
             pid-file write must read as unverified, never verified"
        );

        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn takeover_refused_after_a_legacy_rewrite_with_the_same_pid() {
        // The full P1 shape at the takeover gate: the new-generation
        // supervisor recorded its identity and died; its pid was recycled,
        // and during the rollback window the legacy binary rewrote the pid
        // file with the same numeric pid now owned by a different live
        // process. `check_incumbent` must refuse takeover (the slot is
        // occupied by an unverifiable record) instead of reading the stale
        // identity as verified-but-dead and reporting the slot free.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        // The dead generation's recorded identity: same pid and exe, but the
        // start time of the process that no longer owns the pid.
        let stale = ProcessIdentity {
            starttime: live.starttime + 1,
            ..live.clone()
        };
        let paths = ManagedPaths {
            state: base.path().join("inst.state.json"),
            supervisor_pid: base.path().join("inst.supervisor.pid"),
            worker_pid: base.path().join("inst.worker.pid"),
            supervisor_log: base.path().join("inst.supervisor.log"),
            worker_log: base.path().join("inst.worker.log"),
        };
        write_pid_identity(&paths.supervisor_pid, &stale).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        // The legacy binary's `write_pid`: bare pid, same number.
        std::fs::write(&paths.supervisor_pid, format!("{}\n", decoy.id())).unwrap();

        let err = check_incumbent(&paths, base.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("refused") && msg.contains(&decoy.id().to_string()),
            "got: {msg}"
        );
        assert!(
            decoy.try_wait().unwrap().is_none(),
            "the unverified incumbent must never be signaled"
        );
        assert!(paths.supervisor_pid.exists(), "records must be kept");

        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn stop_refused_after_a_legacy_rewrite_with_the_same_pid() {
        // The same P1 shape at the stop gate: teardown of the stale record
        // must NOT mistake the still-running process for an exited incumbent,
        // delete the just-written Stopped marker and the pid records, and
        // report success — it must refuse, keep the Stopped state and every
        // record, and leave the process alone.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        let stale = ProcessIdentity {
            starttime: live.starttime + 1,
            ..live.clone()
        };
        let paths = instance_with_identity(base.path(), &stale);
        std::thread::sleep(Duration::from_millis(20));
        // The legacy binary rewrote only the supervisor pid file, same pid.
        std::fs::write(&paths.supervisor_pid, format!("{}\n", decoy.id())).unwrap();

        let result = teardown_instance_with_timeout(
            &paths,
            Path::new("/nonexistent/skillfs-mount"),
            Duration::from_millis(300),
        );
        let survived = decoy.try_wait().unwrap().is_none();
        if survived {
            let _ = decoy.kill();
            let _ = decoy.wait();
        }
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains(&decoy.id().to_string()) && err.contains("still running"),
            "got: {err}"
        );
        assert!(survived, "the unverified incumbent must never be signaled");
        assert!(paths.supervisor_pid.exists(), "records must be kept");
        assert_eq!(
            ManagedState::load(&paths.state).unwrap().desired_state,
            DesiredState::Stopped,
            "the stopped marker must be kept for the incumbent"
        );
    }

    #[test]
    fn stop_refuses_when_the_incumbent_record_changes_mid_stop() {
        // `stop` read the incumbent records once, at its start. If a writer
        // active during the stop wait replaces a record — here a new
        // supervisor records itself over the dying legacy incumbent's file —
        // deleting the Stopped marker and the pid records on the strength of
        // the stale read would free the slot under a live incumbent. Stop
        // must re-verify the records immediately before cleanup and refuse
        // when the incumbent identity changed mid-stop.
        let base = tempfile::tempdir().unwrap();
        let mut incumbent = spawn_sleep("1");
        let incumbent_pid = incumbent.id();
        // Production incumbents are reaped by init; reap the decoy from a
        // helper thread so the pid genuinely disappears when it exits.
        let reaper = std::thread::spawn(move || {
            let _ = incumbent.wait();
        });
        let mut newcomer = spawn_sleep("300");
        let paths = ManagedPaths {
            state: base.path().join("inst.state.json"),
            supervisor_pid: base.path().join("inst.supervisor.pid"),
            worker_pid: base.path().join("inst.worker.pid"),
            supervisor_log: base.path().join("inst.supervisor.log"),
            worker_log: base.path().join("inst.worker.log"),
        };
        sample_state("inst").save(&paths.state).unwrap();
        // The incumbent's legacy bare-pid record (no identity sidecar).
        std::fs::write(&paths.supervisor_pid, format!("{incumbent_pid}\n")).unwrap();

        // Mid-stop, a new generation records itself over the pid file.
        let sup_pid_path = paths.supervisor_pid.clone();
        let newcomer_id = newcomer.id();
        let rewriter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            write_pid_identity(&sup_pid_path, &live_identity_of(newcomer_id)).unwrap();
        });

        let result = teardown_instance_with_timeout(
            &paths,
            Path::new("/nonexistent/skillfs-mount"),
            Duration::from_secs(5),
        );
        rewriter.join().unwrap();
        let survived = newcomer.try_wait().unwrap().is_none();
        if survived {
            let _ = newcomer.kill();
            let _ = newcomer.wait();
        }
        let _ = reaper.join();

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains(&newcomer_id.to_string()),
            "the refusal must name the incumbent that appeared mid-stop: {err}"
        );
        assert!(survived, "the mid-stop incumbent must never be signaled");
        assert!(paths.supervisor_pid.exists(), "records must be kept");
        assert!(identity_sidecar(&paths.supervisor_pid).exists());
        assert_eq!(
            ManagedState::load(&paths.state).unwrap().desired_state,
            DesiredState::Stopped,
            "the stopped marker must be kept"
        );
    }

    #[test]
    fn a_record_from_a_previous_boot_never_verifies_or_signals() {
        // Round-4 review: the identity triple (pid, start tick, exe) is only
        // unique within ONE boot — the start tick restarts from zero at
        // reboot, and the runtime directory can be persistent, so a sidecar
        // left by a previous boot can meet a post-reboot process with the
        // same pid, the same relative start tick and the same exe path. A
        // reader without a boot binding verified such a record and pidfd'd
        // the wrong new process. A record not provably captured on the
        // current boot must read unverified and must never be signaled.
        // The previous boot's writer is emulated exactly: the previous
        // generation's pid file and sidecar formats (no boot binding), all
        // fields naming the process that happens to hold the pid now.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        let paths = ManagedPaths {
            state: base.path().join("inst.state.json"),
            supervisor_pid: base.path().join("inst.supervisor.pid"),
            worker_pid: base.path().join("inst.worker.pid"),
            supervisor_log: base.path().join("inst.supervisor.log"),
            worker_log: base.path().join("inst.worker.log"),
        };
        sample_state("inst").save(&paths.state).unwrap();
        std::fs::write(&paths.supervisor_pid, format!("{}\n", decoy.id())).unwrap();
        let (ino, mtime_ns) = pid_file_metadata(&paths.supervisor_pid);
        std::fs::write(
            identity_sidecar(&paths.supervisor_pid),
            format!(
                "{} {} {} {} {}\n",
                live.pid, live.starttime, ino, mtime_ns, live.exe
            ),
        )
        .unwrap();

        let record = read_pid_identity(&paths.supervisor_pid).unwrap();
        assert_eq!(record.pid, decoy.id() as i32);
        assert!(
            !record.is_verifiable(),
            "a record not provably captured on the current boot must not verify"
        );

        // And through the stop gate: the decoy must be spared, the records
        // kept, the refusal explicit.
        let result = teardown_instance_with_timeout(
            &paths,
            Path::new("/nonexistent/skillfs-mount"),
            Duration::from_millis(300),
        );
        let survived = decoy.try_wait().unwrap().is_none();
        if survived {
            let _ = decoy.kill();
            let _ = decoy.wait();
        }
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains(&decoy.id().to_string()) && err.contains("still running"),
            "got: {err}"
        );
        assert!(
            survived,
            "a record from another boot must never be signaled"
        );
        assert!(paths.supervisor_pid.exists(), "records must be kept");
        assert_eq!(
            ManagedState::load(&paths.state).unwrap().desired_state,
            DesiredState::Stopped,
            "the stopped marker must be kept"
        );
    }

    #[test]
    fn sidecar_bound_to_a_foreign_boot_id_reads_unverified() {
        // The reviewer's minimal shape: pid, starttime and exe unchanged,
        // only the recorded boot id differs (the machine rebooted between
        // capture and read). The reader must refuse the pairing.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let path = base.path().join("inst.supervisor.pid");
        write_pid_identity(&path, &live_identity_of(decoy.id())).unwrap();
        assert!(
            read_pid_identity(&path).unwrap().is_verifiable(),
            "the writer's own boot must verify"
        );

        // Rewrite only the sidecar's boot-id field, keeping pid, starttime,
        // spacing and exe byte-for-byte.
        let sidecar = identity_sidecar(&path);
        let raw = std::fs::read_to_string(&sidecar).unwrap();
        let mut fields = raw.splitn(5, ' ');
        let pid = fields.next().unwrap();
        let starttime = fields.next().unwrap();
        let _recorded_boot = fields.next().unwrap();
        let spacing = fields.next().unwrap();
        let exe = raw.splitn(5, ' ').nth(4).unwrap();
        std::fs::write(
            &sidecar,
            format!("{pid} {starttime} 00000000-0000-0000-0000-000000000000 {spacing}{exe}"),
        )
        .unwrap();

        let record = read_pid_identity(&path).unwrap();
        assert_eq!(record.pid, decoy.id() as i32);
        assert!(
            !record.is_verifiable(),
            "a sidecar naming another boot must not verify this pid"
        );
        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn same_tick_rewrite_with_the_same_pid_still_reads_unverified() {
        // Round-4 review: (inode, mtime_ns) is not a unique write
        // generation — the kernel's coarse clock can leave an in-place
        // rewrite inside the same granularity tick with the very mtime the
        // sidecar recorded, and the legacy same-pid rewrite escaped the
        // binding check (the round-3 tests slept 20 ms first and dodged
        // exactly this boundary). The collision is forced here
        // deterministically: the rewrite's mtime is restored to the exact
        // nanosecond the sidecar's writer observed, so the (inode,
        // mtime_ns) tuple is unchanged — and the record must STILL degrade
        // to unverified.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let path = base.path().join("inst.supervisor.pid");
        write_pid_identity(&path, &live_identity_of(decoy.id())).unwrap();
        assert!(
            read_pid_identity(&path).unwrap().is_verifiable(),
            "the writer's own generation must verify"
        );
        let (_, mtime_ns) = pid_file_metadata(&path);

        // The legacy writer's bare in-place rewrite, same numeric pid, no
        // sleep — then force the recorded mtime back, so the tuple the
        // previous binding compared is exactly what it recorded.
        std::fs::write(&path, format!("{}\n", decoy.id())).unwrap();
        force_mtime_ns(&path, mtime_ns);

        let record = read_pid_identity(&path).unwrap();
        assert_eq!(record.pid, decoy.id() as i32);
        assert!(
            !record.is_verifiable(),
            "a same-tick legacy rewrite must not pair with the sidecar"
        );
        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn pid_file_generations_are_unique_and_stay_legacy_readable() {
        // The strengthened generation primitive itself: every write of the
        // same pid file is a fresh, unique representation; the previous
        // version's whole-file integer reader keeps reading the pid from
        // every one of them; and a legacy bare rewrite is never equal to
        // any generation this writer produced.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.supervisor.pid");
        let identity = live_identity_of(std::process::id());

        let mut generations: Vec<String> = Vec::new();
        for _ in 0..4 {
            write_pid_identity(&path, &identity).unwrap();
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(
                !generations.contains(&raw),
                "every write of the same pid file must be a fresh generation"
            );
            generations.push(raw);
            assert_eq!(
                previous_version_read_pid(&path),
                Some(std::process::id() as i32),
                "a rolled-back binary must keep reading the pid"
            );
            assert_eq!(
                read_pid_identity(&path),
                Some(identity.clone()),
                "the current writer's generation must keep verifying"
            );
        }
        std::fs::write(&path, format!("{}\n", std::process::id())).unwrap();
        assert!(
            generations
                .iter()
                .all(|generation| generation.as_str() != format!("{}\n", std::process::id())),
            "the legacy bare representation is never one of this writer's generations"
        );
        assert!(!read_pid_identity(&path).unwrap().is_verifiable());
    }

    #[test]
    fn takeover_refused_after_a_same_tick_rewrite_with_the_same_pid() {
        // The full P1 shape at the takeover gate, on the same-tick boundary:
        // the legacy rewrite keeps the recorded (inode, mtime_ns) — forced
        // equal here — and uses the same numeric pid now owned by a live
        // process. `check_incumbent` must refuse takeover instead of
        // reading the stale identity as verified-but-dead.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        let stale = ProcessIdentity {
            starttime: live.starttime + 1,
            ..live.clone()
        };
        let paths = ManagedPaths {
            state: base.path().join("inst.state.json"),
            supervisor_pid: base.path().join("inst.supervisor.pid"),
            worker_pid: base.path().join("inst.worker.pid"),
            supervisor_log: base.path().join("inst.supervisor.log"),
            worker_log: base.path().join("inst.worker.log"),
        };
        write_pid_identity(&paths.supervisor_pid, &stale).unwrap();
        let (_, mtime_ns) = pid_file_metadata(&paths.supervisor_pid);
        // The legacy binary's `write_pid`: bare pid, same number, same tick.
        std::fs::write(&paths.supervisor_pid, format!("{}\n", decoy.id())).unwrap();
        force_mtime_ns(&paths.supervisor_pid, mtime_ns);

        let err = check_incumbent(&paths, base.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("refused") && msg.contains(&decoy.id().to_string()),
            "got: {msg}"
        );
        assert!(
            decoy.try_wait().unwrap().is_none(),
            "the unverified incumbent must never be signaled"
        );
        assert!(paths.supervisor_pid.exists(), "records must be kept");

        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn stop_refused_after_a_same_tick_rewrite_with_the_same_pid() {
        // The same P1 shape at the stop gate: the same-tick legacy rewrite
        // must not let teardown mistake the still-running process for an
        // exited incumbent, delete the just-written Stopped marker and the
        // pid records, and report success.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        let stale = ProcessIdentity {
            starttime: live.starttime + 1,
            ..live.clone()
        };
        let paths = instance_with_identity(base.path(), &stale);
        let (_, mtime_ns) = pid_file_metadata(&paths.supervisor_pid);
        // The legacy binary rewrote only the supervisor pid file, same pid,
        // inside the same timestamp tick.
        std::fs::write(&paths.supervisor_pid, format!("{}\n", decoy.id())).unwrap();
        force_mtime_ns(&paths.supervisor_pid, mtime_ns);

        let result = teardown_instance_with_timeout(
            &paths,
            Path::new("/nonexistent/skillfs-mount"),
            Duration::from_millis(300),
        );
        let survived = decoy.try_wait().unwrap().is_none();
        if survived {
            let _ = decoy.kill();
            let _ = decoy.wait();
        }
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains(&decoy.id().to_string()) && err.contains("still running"),
            "got: {err}"
        );
        assert!(survived, "the unverified incumbent must never be signaled");
        assert!(paths.supervisor_pid.exists(), "records must be kept");
        assert_eq!(
            ManagedState::load(&paths.state).unwrap().desired_state,
            DesiredState::Stopped,
            "the stopped marker must be kept for the incumbent"
        );
    }

    #[test]
    fn stop_cleanup_is_serialized_with_a_concurrent_publication() {
        // Round-4 review, deterministic barrier form. The vulnerable window
        // was between stop's final re-verify of the incumbent records and
        // the record/state cleanup: a concurrent `mount --managed`
        // publishing its new state and supervisor record there had that
        // generation deleted under a success return. Publication and the
        // final re-verify + cleanup are now mutually exclusive via the
        // instance lock, so the interleave is only constructible by a
        // writer HOLDING the lock — exactly what this test does: it holds
        // the instance lock while the stopper reaches its final section,
        // publishes a live newcomer (fresh Mounted state + supervisor
        // record), and only then releases. The stopper must re-verify
        // AFTER the publication, refuse, and keep the newcomer's records —
        // never delete them and return success. On the previous head (no
        // lock) the stopper ran to completion during the hold and
        // returned Ok.
        let base = tempfile::tempdir().unwrap();
        let mut newcomer = spawn_sleep("300");
        let state_path = base.path().join("inst.state.json");
        let sup_pid_path = base.path().join("inst.supervisor.pid");
        sample_state("inst").save(&state_path).unwrap();

        // Hold the instance lock the publication and the teardown final
        // section serialize on (the state path plus `.lock`).
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(instance_lock_path_for_tests(&state_path))
            .unwrap();
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
            assert_eq!(rc, 0, "the test must hold the instance lock");
        }

        let mount_dir = base.path().to_path_buf();
        let stopper = std::thread::spawn(move || {
            let paths = ManagedPaths {
                state: mount_dir.join("inst.state.json"),
                supervisor_pid: mount_dir.join("inst.supervisor.pid"),
                worker_pid: mount_dir.join("inst.worker.pid"),
                supervisor_log: mount_dir.join("inst.supervisor.log"),
                worker_log: mount_dir.join("inst.worker.log"),
            };
            teardown_instance_with_timeout(
                &paths,
                Path::new("/nonexistent/skillfs-mount"),
                Duration::from_millis(300),
            )
            .map_err(|e| e.to_string())
        });

        // Park the stopper at its final section (on the broken head, let it
        // run to completion instead) before publishing.
        std::thread::sleep(Duration::from_millis(300));

        // The concurrent mount's publication, as it runs under the lock:
        // fresh Mounted state + a live supervisor record.
        sample_state("inst").save(&state_path).unwrap();
        write_pid_identity(&sup_pid_path, &live_identity_of(newcomer.id())).unwrap();

        drop(lock); // release the instance lock

        let result = stopper.join().unwrap();
        let survived = newcomer.try_wait().unwrap().is_none();
        if survived {
            let _ = newcomer.kill();
            let _ = newcomer.wait();
        }
        let err = result.expect_err("stop must refuse when a newcomer published under the lock");
        assert!(
            err.contains(&newcomer.id().to_string()),
            "the refusal must name the newcomer: {err}"
        );
        assert!(survived, "the newcomer must never be signaled");
        assert!(sup_pid_path.exists(), "the newcomer's records must be kept");
        assert!(
            identity_sidecar(&sup_pid_path).exists(),
            "the newcomer's identity sidecar must be kept"
        );
        assert_eq!(
            ManagedState::load(&state_path).unwrap().desired_state,
            DesiredState::Mounted,
            "the newcomer's published state must be kept"
        );
    }

    #[test]
    fn supervisor_identity_is_published_before_the_instance_lock_is_released() {
        // Round-5 review: the mount client's critical section only spawned
        // the supervisor; the pid identity was written later by the child,
        // after it re-took the same instance lock. Two concurrent
        // `mount --managed` clients could therefore both pass the incumbent
        // gate against a not-yet-visible record and start two supervisors,
        // either of whose `finish` would delete the other generation's
        // records. The identity must already be on disk — and verifiable —
        // while the publishing client still holds the instance lock, and a
        // second real client must serialize behind that lock instead of
        // deciding on a missing record.
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        let mountpoint = base.path().join("mnt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&mountpoint).unwrap();
        let instance_id = instance_id_for(&normalize_mountpoint(&mountpoint));
        let paths = ManagedPaths::new(&instance_id);

        let client = {
            let source = source.clone();
            let mountpoint = mountpoint.clone();
            let hold_prefix = paths.state.clone();
            std::thread::spawn(move || {
                with_hold_at_publish(&hold_prefix, || {
                    run_client(&[], &source, &mountpoint)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
            })
        };

        // The client parks inside its critical section once the identity is
        // published. While it is parked — the instance lock still held — the
        // supervisor record must already exist and verify.
        let held = wait_for_file(
            &instance_mark_path(&paths.state, ".identity-published"),
            Duration::from_secs(10),
        );
        let published_while_held = read_pid_identity(&paths.supervisor_pid);

        // A second real client, started while the first still holds the
        // instance lock, must block behind it — never decide on the slot
        // concurrently.
        let second = {
            let source = source.clone();
            let mountpoint = mountpoint.clone();
            std::thread::spawn(move || {
                run_client(&[], &source, &mountpoint)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        };
        std::thread::sleep(Duration::from_millis(500));
        let second_still_blocked = !second.is_finished();

        // Release the parked client, then join both in every path before
        // asserting, so a failing assertion cannot wedge a lock holder.
        std::fs::write(instance_mark_path(&paths.state, ".identity-release"), b"").unwrap();
        let _ = client.join().unwrap();
        let _ = second.join().unwrap();

        assert!(
            held,
            "the client never reached the publication point inside its critical section"
        );
        assert!(
            second_still_blocked,
            "the second client must serialize behind the first client's instance lock"
        );
        let record = published_while_held
            .expect("supervisor identity must be published before the instance lock is released");
        assert!(
            record.is_verifiable(),
            "the published supervisor identity must be verifiable, got {record:?}"
        );
        assert_ne!(
            record.pid as u32,
            std::process::id(),
            "the record must name the spawned supervisor, not the client"
        );
    }

    #[test]
    fn client_death_between_spawn_and_publish_leaves_exactly_one_supervisor() {
        // Round-6 review: the client spawned the detached supervisor and only
        // then published its identity. A client killed between the two steps
        // released the instance lock with no supervisor record on disk while
        // the detached child kept running (and would record itself, serving
        // over a slot every later client reads as free): a concurrent second
        // `mount --managed` passed the empty-slot gate and started a second
        // supervisor. The client and child now share a startup handshake —
        // the child stays parked on a pipe and proceeds (or exits) only once
        // the identity is durably published, or never will be. This barrier
        // regression kills the client exactly after the spawn returned and
        // before the publication, then starts a second real client and
        // requires that exactly one supervisor ends up serving.
        let base = tempfile::tempdir().unwrap();
        let source = base.path().join("source");
        let mountpoint = base.path().join("mnt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&mountpoint).unwrap();
        let instance_id = instance_id_for(&normalize_mountpoint(&mountpoint));
        let paths = ManagedPaths::new(&instance_id);
        let supervisor_exe = built_skillfs_binary();

        // The first client parks after its supervisor is spawned and dies
        // there, before publishing anything.
        let client = {
            let source = source.clone();
            let mountpoint = mountpoint.clone();
            let hold_prefix = paths.state.clone();
            let supervisor_exe = supervisor_exe.clone();
            std::thread::spawn(move || {
                SUPERVISOR_CHILD_PROGRAM_OVERRIDE.with(|o| *o.borrow_mut() = Some(supervisor_exe));
                with_hold_at_spawn(&hold_prefix, || {
                    run_client(&[], &source, &mountpoint)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
            })
        };

        let parked = wait_for_file(
            &instance_mark_path(&paths.state, ".spawned"),
            Duration::from_secs(10),
        );
        assert!(
            parked,
            "the client never reached the post-spawn, pre-publication park"
        );
        // The supervisor is running and recordless: exactly the window under
        // test. Its command line becomes visible once the child exec'd, so
        // wait for it deterministically.
        let orphan = wait_for_one_supervise(&instance_id, Duration::from_secs(5))
            .expect("the just-spawned supervisor must be visible before the publication");
        assert!(
            read_pid_identity(&paths.supervisor_pid).is_none(),
            "the park sits before the publication: no supervisor record may exist yet"
        );

        // Kill the client inside the window. The thread unwind closes the
        // handshake pipe and drops the instance lock exactly like a
        // SIGKILLed client process would.
        std::fs::write(instance_mark_path(&paths.state, ".spawn-kill"), b"").unwrap();
        let payload = client
            .join()
            .expect_err("the killed client must have crashed inside the park, not returned");
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&'static str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains("simulated client crash"),
            "the client must die in the spawn/publish window, got: {message}"
        );

        // The orphaned supervisor must exit on the handshake EOF instead of
        // surviving recordless over a slot that reads as free.
        assert!(
            wait_process_gone(orphan, Duration::from_secs(3)),
            "supervisor {orphan} kept running after the publishing client died \
             before the publication; a concurrent mount client would see a \
             free slot behind it"
        );
        assert!(
            read_pid_identity(&paths.supervisor_pid).is_none(),
            "the orphaned supervisor must not record itself after the client's death"
        );

        // The second real client becomes THE one supervisor for the instance.
        let second = {
            let source = source.clone();
            let mountpoint = mountpoint.clone();
            let supervisor_exe = supervisor_exe.clone();
            std::thread::spawn(move || {
                SUPERVISOR_CHILD_PROGRAM_OVERRIDE.with(|o| *o.borrow_mut() = Some(supervisor_exe));
                let result = run_client(&[], &source, &mountpoint)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                SUPERVISOR_CHILD_PROGRAM_OVERRIDE.with(|o| *o.borrow_mut() = None);
                result
            })
        };
        let mut published = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match read_pid_identity(&paths.supervisor_pid) {
                Some(record) if record.is_verifiable() => {
                    published = Some(record);
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        // The one supervisor that survives the crash/restart cycle.
        let survivor =
            wait_for_one_supervise(&instance_id, Duration::from_secs(5)).unwrap_or_else(|| {
                panic!(
                    "exactly one supervisor must survive the crash/restart cycle, got: {:?}",
                    live_supervise_pids(&instance_id)
                )
            });
        // Join the second client (its mount may legitimately fail to become
        // ready in a test environment) and clean up in every path, so even a
        // failing assertion cannot leak a live instance.
        let _ = second.join();
        let _ = teardown_instance_with_timeout(
            &paths,
            &normalize_mountpoint(&mountpoint),
            Duration::from_secs(2),
        );

        let published =
            published.expect("the second client must publish a verifiable supervisor identity");
        assert_ne!(
            survivor, orphan,
            "the survivor must be the second client's supervisor, not the orphan"
        );
        assert_ne!(
            published.pid, orphan,
            "the published record must name the second client's supervisor"
        );
    }

    #[test]
    fn stale_generation1_sidecar_never_verifies_an_interrupted_first_write() {
        // Round-5 review: `write_pid_identity` published the new pid-file
        // representation BEFORE touching the sidecar. When the current pid
        // file is a legacy bare record of the same pid, the generation count
        // resets to 1 — the very generation a stale sidecar may still
        // record — so a crash (or a failed write) after the pid file was
        // published but before the fresh sidecar landed left that stale
        // sidecar verifying the new write as the identity of the process
        // that owned the pid before the recycle. The reader then treated a
        // still-alive worker as exited, and the orphan gate started a
        // replacement over it. The sidecar must therefore be invalidated
        // BEFORE the generation is reset, so every interrupted state reads
        // unverified and takeover stays refused while the pid lives.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        // The previous generation's record: same pid and exe, but the start
        // time of the process that owned the pid before the recycle.
        let stale = ProcessIdentity {
            starttime: live.starttime + 1,
            ..live.clone()
        };
        let path = base.path().join("inst.worker.pid");

        // This writer's generation 1, then a legacy bare rewrite of the same
        // pid — exactly the state a rollback window leaves behind.
        write_pid_identity(&path, &stale).unwrap();
        std::fs::write(&path, format!("{}\n", decoy.id())).unwrap();
        assert!(
            !read_pid_identity(&path).unwrap().is_verifiable(),
            "the legacy bare rewrite must read unverified while the sidecar is stale"
        );

        // The next write of the same pid is interrupted after the pid file
        // is published and before the fresh sidecar is written.
        let interrupted = with_crash_after_pid_publish(|| write_pid_identity(&path, &live).is_ok());
        assert!(
            interrupted,
            "the interrupted write must still report success"
        );

        let record = read_pid_identity(&path).unwrap();
        assert_eq!(record.pid, decoy.id() as i32);
        assert!(
            !record.is_verifiable(),
            "the stale generation-1 sidecar must not verify the first write of the \
             reset generation"
        );
        let blocker = live_legacy_blocker(&path)
            .expect("a live unverified record must block takeover after an interrupted write");
        let message = blocker.to_string();
        assert!(
            message.contains("refused") && message.contains(&decoy.id().to_string()),
            "the blocker must refuse takeover and name the live pid: {message}"
        );
        assert!(
            decoy.try_wait().unwrap().is_none(),
            "the live worker must never be signaled"
        );

        // The retried write (no interruption this time) re-establishes
        // verification on a fresh generation.
        write_pid_identity(&path, &live).unwrap();
        assert!(
            read_pid_identity(&path).unwrap().is_verifiable(),
            "the retried write must verify on its own generation"
        );

        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn sidecar_invalidation_failure_blocks_the_pid_rewrite() {
        // Round-6 review: the stale sidecar's invalidation error was ignored
        // and the pid file was rewritten anyway. With an undeletable sidecar
        // (run directory 0500 — unlink(2) needs directory write) and a still-
        // writable pid file, a rewrite that resets the generation to 1 could
        // land on the very spacing a stale generation-1 sidecar recorded, and
        // a crash (or failed write) after the rewrite left that stale sidecar
        // verifying the new write as the identity of the process that owned
        // the pid before the recycle — exactly the pairing the generation
        // binding exists to prevent. The invalidation error must therefore
        // fail the whole write BEFORE the pid file is touched.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        // The previous generation's occupant: same pid and exe, earlier start
        // time — the identity a recycled pid's stale sidecar would carry.
        let stale = ProcessIdentity {
            starttime: live.starttime + 1,
            ..live.clone()
        };
        let path = base.path().join("inst.worker.pid");
        let sidecar = identity_sidecar(&path);

        // This writer's generation 1, then a legacy bare rewrite of the same
        // pid: the next write resets the generation to 1 — the generation the
        // stale sidecar still records.
        write_pid_identity(&path, &stale).unwrap();
        std::fs::write(&path, format!("{}\n", decoy.id())).unwrap();

        let pid_before = std::fs::read_to_string(&path).unwrap();
        let sidecar_before = std::fs::read_to_string(&sidecar).unwrap();

        // Run directory 0500: the sidecar cannot be unlinked while the
        // existing pid file stays writable — the review's repro.
        struct RestoreDirPerms<'a>(&'a Path);
        impl Drop for RestoreDirPerms<'_> {
            fn drop(&mut self) {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let _restore = RestoreDirPerms(base.path());

        // The interrupted write: the crash hook stops it right after the pid
        // file would have been published, had the rewrite been allowed to
        // proceed over the failed invalidation.
        let result = with_crash_after_pid_publish(|| write_pid_identity(&path, &live));

        // Restore before asserting so even a failed assert cannot leave the
        // tempdir unusable.
        drop(_restore);

        let error = result.expect_err("a failed sidecar invalidation must fail the whole write");
        let kind = error
            .downcast_ref::<std::io::Error>()
            .map(|io_error| io_error.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "the invalidation failure itself must surface: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            pid_before,
            "the pid file must be untouched when the invalidation fails"
        );
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            sidecar_before,
            "the stale sidecar must be untouched when the invalidation fails"
        );
        // And the stale generation-1 sidecar must never verify the would-be
        // new generation: the record on disk is still the interrupted legacy
        // pair, which reads unverified.
        assert!(
            !read_pid_identity(&path).unwrap().is_verifiable(),
            "the stale sidecar must not verify across the refused rewrite"
        );

        // Once the directory is writable again, the retried write succeeds
        // and verifies on its own fresh generation.
        write_pid_identity(&path, &live).unwrap();
        let record = read_pid_identity(&path).unwrap();
        assert!(record.is_verifiable(), "the retried write must verify");
        assert_eq!(record.starttime, live.starttime);

        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn identity_survives_unlink_of_an_exe_literally_named_deleted_suffix() {
        // A legitimate executable name can end in the kernel's own marker
        // text. Recorded while the file is still linked, /proc shows the
        // literal path; after an unlink the kernel appends ITS marker, and
        // the live value is the recorded path plus one " (deleted)".
        // Stripping one marker from each side (the earlier behavior)
        // leaves the literal name on one side and the name-plus-marker on
        // the other: the running instance read as dead, `stop` cleared its
        // records, and the next remount raced the still-live supervisor
        // (review of this PR).
        let base = tempfile::tempdir().unwrap();
        let mut probe = spawn_sleep("300");
        let original_exe = live_identity_of(probe.id()).exe;
        probe.kill().unwrap();
        probe.wait().unwrap();
        let exe = base.path().join("skillfs (deleted)");
        std::fs::copy(&original_exe, &exe).expect("copy sleep binary");
        let mut decoy = std::process::Command::new(&exe)
            .arg("300")
            .spawn()
            .expect("spawn literal-suffix decoy");
        // The capture must not race the child's exec: until it lands,
        // /proc/<pid>/exe still shows this test binary, so wait for the
        // copied binary to appear before recording the identity.
        let expected_exe = exe.to_str().unwrap();
        let mut recorded = live_identity_of(decoy.id());
        for _ in 0..200 {
            if recorded.exe == expected_exe {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            recorded = live_identity_of(decoy.id());
        }
        assert_eq!(
            recorded.exe, expected_exe,
            "while linked, the literal name carries no kernel marker"
        );
        std::fs::remove_file(&exe).expect("unlink the running binary");

        assert!(
            identity_alive(&recorded),
            "an unlinked binary whose name ends in ' (deleted)' is still the \
             recorded process (live exe: {})",
            read_proc_exe(recorded.pid).unwrap_or_default()
        );
        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[test]
    fn live_legacy_worker_pid_blocks_orphan_replacement() {
        // A legacy bare-pid worker record naming a live process cannot be
        // verified, so it must block takeover of the worker slot; a dead
        // legacy pid or a verifiable record blocks nothing.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let path = base.path().join("inst.worker.pid");
        std::fs::write(&path, format!("{}\n", decoy.id())).unwrap();

        let blocker = live_legacy_blocker(&path).expect("live legacy pid must block takeover");
        let msg = blocker.to_string();
        assert!(
            msg.contains("refused") && msg.contains(&decoy.id().to_string()),
            "got: {msg}"
        );

        // The decoy is never signaled by the check itself, and the pid
        // file is left for the operator.
        assert!(decoy.try_wait().unwrap().is_none());
        assert!(path.exists());

        // Once the process is gone and reaped, the legacy record is inert.
        decoy.kill().unwrap();
        decoy.wait().unwrap();
        assert!(live_legacy_blocker(&path).is_none());

        // A verifiable record is never a legacy blocker, live or not.
        write_pid_identity(&path, &live_identity_of(std::process::id())).unwrap();
        assert!(live_legacy_blocker(&path).is_none());
    }

    #[test]
    fn takeover_is_refused_for_a_live_legacy_supervisor_pid() {
        // The upgrade scenario from the review: a pre-upgrade supervisor
        // is still running with a legacy bare-pid file. On the previous
        // revision `identity_alive` was the only incumbent gate, returned
        // false for the unverified record, and the client fell through to
        // clear the mount, overwrite state/pid files and spawn a second
        // supervisor. The gate must refuse takeover while the pid exists.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let paths = ManagedPaths {
            state: base.path().join("inst.state.json"),
            supervisor_pid: base.path().join("inst.supervisor.pid"),
            worker_pid: base.path().join("inst.worker.pid"),
            supervisor_log: base.path().join("inst.supervisor.log"),
            worker_log: base.path().join("inst.worker.log"),
        };
        std::fs::write(&paths.supervisor_pid, format!("{}\n", decoy.id())).unwrap();

        // A plain directory is not a ready mount: the gate must refuse
        // takeover immediately, not wait the readiness timeout and not
        // clear anything.
        let err = check_incumbent(&paths, base.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("refused") && msg.contains(&decoy.id().to_string()),
            "got: {msg}"
        );
        assert!(decoy.try_wait().unwrap().is_none());
        assert!(paths.supervisor_pid.exists());

        // Once the legacy-recorded process is gone, the slot frees up.
        decoy.kill().unwrap();
        decoy.wait().unwrap();
        assert!(!check_incumbent(&paths, base.path()).unwrap());
    }

    /// The live identity of a spawned process, read through the same
    /// /proc path the production code uses.
    fn live_identity_of(pid: u32) -> ProcessIdentity {
        ProcessIdentity::capture(pid as i32).expect("capture decoy identity from /proc")
    }

    /// The pid file's `(inode, mtime_ns)` — the tuple the previous
    /// generation's sidecar recorded as its write-generation token.
    fn pid_file_metadata(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;

        let meta = std::fs::metadata(path).unwrap();
        let mtime_ns = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        (meta.ino(), mtime_ns)
    }

    /// Restore a file's mtime to an exact nanosecond value via
    /// `utimensat(2)`, deterministically emulating a rewrite that landed
    /// inside the same kernel timestamp-granularity tick as the previous
    /// write (whose mtime_ns the coarse clock left unchanged).
    fn force_mtime_ns(path: &Path, mtime_ns: u64) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let seconds = (mtime_ns / 1_000_000_000) as libc::time_t;
        let nanos = (mtime_ns % 1_000_000_000) as libc::c_long;
        let times = [
            libc::timespec {
                tv_sec: seconds,
                tv_nsec: nanos,
            },
            libc::timespec {
                tv_sec: seconds,
                tv_nsec: nanos,
            },
        ];
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(rc, 0, "forcing the pid file mtime must succeed");
    }

    /// The instance lock path, mirroring the production derivation
    /// (`<state>.lock`) so tests can hold the very lock the publication
    /// and teardown critical sections serialize on.
    fn instance_lock_path_for_tests(state: &Path) -> PathBuf {
        let mut name = state.as_os_str().to_os_string();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// Write a managed instance whose supervisor and worker pid files both
    /// record `identity`.
    fn instance_with_identity(base: &Path, identity: &ProcessIdentity) -> ManagedPaths {
        let paths = ManagedPaths {
            state: base.join("inst.state.json"),
            supervisor_pid: base.join("inst.supervisor.pid"),
            worker_pid: base.join("inst.worker.pid"),
            supervisor_log: base.join("inst.supervisor.log"),
            worker_log: base.join("inst.worker.log"),
        };
        let state = ManagedState {
            schema_version: STATE_SCHEMA_VERSION,
            instance_id: "inst".into(),
            mountpoint: "/nonexistent/skillfs-mount".into(),
            source: "/nonexistent/source".into(),
            worker_program: identity.exe.clone(),
            worker_args: vec![],
            desired_state: DesiredState::Mounted,
        };
        state.save(&paths.state).unwrap();
        write_pid_identity(&paths.supervisor_pid, identity).unwrap();
        write_pid_identity(&paths.worker_pid, identity).unwrap();
        paths
    }

    /// A decoy standing in for a managed process, sleeping `secs` seconds.
    fn spawn_sleep(secs: &str) -> std::process::Child {
        std::process::Command::new("sleep")
            .arg(secs)
            .spawn()
            .expect("spawn sleep decoy")
    }

    /// Run `f` with the thread-local pidfd mock armed: `pidfd_open` reports
    /// ENOSYS for every call on this thread, as on a kernel without pidfd
    /// support.
    fn with_pidfd_enosys<R>(f: impl FnOnce() -> R) -> R {
        MOCK_PIDFD_ENOSYS.with(|m| m.set(true));
        let out = f();
        MOCK_PIDFD_ENOSYS.with(|m| m.set(false));
        out
    }

    /// Run `f` with the crash simulation armed: `write_pid_identity` on this
    /// thread publishes the pid file and then "dies" before the identity
    /// sidecar is written.
    fn with_crash_after_pid_publish<R>(f: impl FnOnce() -> R) -> R {
        SIMULATED_CRASH_AFTER_PID_PUBLISH.with(|m| m.set(true));
        let out = f();
        SIMULATED_CRASH_AFTER_PID_PUBLISH.with(|m| m.set(false));
        out
    }

    /// Arm the mount client's publication barrier for this thread (see
    /// [`hold_at_publish`]): the next `run_client` on this thread parks
    /// inside its critical section after publishing the supervisor identity,
    /// until the release marker appears.
    fn with_hold_at_publish<R>(prefix: &Path, f: impl FnOnce() -> R) -> R {
        HOLD_AT_PUBLISH.with(|hold| *hold.borrow_mut() = Some(prefix.to_path_buf()));
        let out = f();
        HOLD_AT_PUBLISH.with(|hold| *hold.borrow_mut() = None);
        out
    }

    /// Arm the mount client's spawn barrier for this thread (see
    /// [`hold_at_spawn`]): the next `run_client` on this thread parks inside
    /// its critical section after the supervisor spawn, before the identity
    /// publication, until a release or kill marker appears.
    fn with_hold_at_spawn<R>(prefix: &Path, f: impl FnOnce() -> R) -> R {
        HOLD_AT_SPAWN.with(|hold| *hold.borrow_mut() = Some(prefix.to_path_buf()));
        let out = f();
        HOLD_AT_SPAWN.with(|hold| *hold.borrow_mut() = None);
        out
    }

    /// The package binary `cargo test` builds alongside this harness: the
    /// supervisor child under test must run the real `supervise` entry point,
    /// whose handshake semantics live in the shipped binary, not in this test
    /// harness.
    fn built_skillfs_binary() -> PathBuf {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("the crate manifest has two ancestors")
            .to_path_buf();
        let target_base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target"));
        for profile in ["debug", "release"] {
            let candidate = target_base.join(profile).join("skillfs");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!(
            "the built skillfs binary was not found under {} — cargo test builds \
             it alongside the test harness",
            target_base.display()
        );
    }

    /// Live pids whose command line contains `supervise --instance <id>` as
    /// one argument triple — the detached supervisors of one managed
    /// instance. Zombies (unreaped children of this very test process) have
    /// an empty command line and never match.
    fn live_supervise_pids(instance_id: &str) -> Vec<i32> {
        let needle = format!("supervise\0--instance\0{instance_id}\0");
        let needle = needle.as_bytes();
        let mut pids = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return pids;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            if cmdline.windows(needle.len()).any(|window| window == needle) {
                pids.push(pid);
            }
        }
        pids.sort_unstable();
        pids
    }

    /// Wait until exactly one live supervisor exists for `instance_id` and
    /// return its pid, or `None` on timeout or an ambiguous count.
    fn wait_for_one_supervise(instance_id: &str, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            let pids = live_supervise_pids(instance_id);
            if pids.len() == 1 {
                return Some(pids[0]);
            }
            if !pids.is_empty() || Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Wait until `pid` is gone — either reaped (`/proc` entry gone) or
    /// lingering only as a zombie nobody has reaped (an unreaped child of
    /// this test process). Returns whether it is gone.
    fn wait_process_gone(pid: i32, timeout: Duration) -> bool {
        let stat = format!("/proc/{pid}/stat");
        let deadline = Instant::now() + timeout;
        loop {
            match std::fs::read_to_string(&stat) {
                Err(_) => return true,
                Ok(raw) => {
                    let zombie = raw
                        .rsplit_once(')')
                        .and_then(|(_, rest)| rest.trim_start().chars().next())
                        == Some('Z');
                    if zombie {
                        return true;
                    }
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The marker file names the publication barrier derives from a state
    /// path (mirroring [`instance_lock_path`]'s suffixing).
    fn instance_mark_path(state: &Path, suffix: &str) -> PathBuf {
        let mut name = state.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }

    /// Wait for `path` to appear, up to `timeout`. Returns whether it was
    /// seen.
    fn wait_for_file(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        path.exists()
    }

    #[test]
    fn teardown_spares_a_recycled_pid_of_another_program() {
        // Managed pid files can outlive their process. When an unrelated
        // same-uid process (here: sleep) has recycled the recorded pid,
        // teardown must not terminate it — the recorded executable does
        // not match, even though the start time was pinned exactly.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let recorded = ProcessIdentity {
            pid: decoy.id() as i32,
            starttime: live_identity_of(decoy.id()).starttime,
            exe: "/usr/bin/skillfs".into(),
            boot_id: this_boot(),
        };
        let paths = instance_with_identity(base.path(), &recorded);

        teardown_instance(&paths, Path::new("/nonexistent/skillfs-mount")).unwrap();

        let survived = decoy.try_wait().unwrap().is_none();
        let _ = decoy.kill();
        let _ = decoy.wait();
        assert!(
            survived,
            "an unrelated process reusing a managed pid must not be signaled"
        );
    }

    #[test]
    fn teardown_spares_a_same_named_binary_at_a_different_path() {
        // A different executable with the same basename as the recorded
        // program must be rejected: identity binds the full /proc exe
        // path, not the file name.
        let base = tempfile::tempdir().unwrap();
        // Find where the system sleep actually lives, then run a copy of
        // it from the temp dir: the decoy's /proc exe is the copy's path
        // while the record claims the original's.
        let mut probe = spawn_sleep("300");
        let original_exe = live_identity_of(probe.id()).exe;
        probe.kill().unwrap();
        probe.wait().unwrap();
        let copy = base.path().join("sleep");
        std::fs::copy(&original_exe, &copy).expect("copy sleep binary");
        let mut decoy = std::process::Command::new(&copy)
            .arg("300")
            .spawn()
            .expect("spawn copied-binary decoy");
        let recorded = ProcessIdentity {
            pid: decoy.id() as i32,
            starttime: live_identity_of(decoy.id()).starttime,
            exe: original_exe.clone(),
            boot_id: this_boot(),
        };
        let paths = instance_with_identity(base.path(), &recorded);

        teardown_instance(&paths, Path::new("/nonexistent/skillfs-mount")).unwrap();

        let survived = decoy.try_wait().unwrap().is_none();
        let _ = decoy.kill();
        let _ = decoy.wait();
        assert!(
            survived,
            "a same-named executable at a different path must not be signaled"
        );
    }

    #[test]
    fn teardown_spares_another_instance_with_the_same_exe() {
        // Another skillfs instance's worker runs the same binary (same
        // full exe path) but is a different process creation: the start
        // time recorded at our fork time does not match its own, so it is
        // never treated as ours. Emulated with a decoy whose real exe path
        // is recorded but whose recorded start time is one tick later.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let live = live_identity_of(decoy.id());
        let recorded = ProcessIdentity {
            pid: live.pid,
            starttime: live.starttime + 1,
            exe: live.exe.clone(),
            boot_id: this_boot(),
        };
        let paths = instance_with_identity(base.path(), &recorded);

        teardown_instance(&paths, Path::new("/nonexistent/skillfs-mount")).unwrap();

        let survived = decoy.try_wait().unwrap().is_none();
        let _ = decoy.kill();
        let _ = decoy.wait();
        assert!(
            survived,
            "a same-exe process with a different start time (another instance) must not be signaled"
        );
    }

    #[test]
    fn teardown_spares_unverifiable_legacy_pid_files() {
        // A pid-only (legacy) record cannot prove the pid still belongs
        // to the managed instance, so teardown must never signal it — and,
        // while that pid is still occupied, must not complete either: the
        // records stay, the state stays (already marked stopped), and the
        // failure is explicit.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let paths =
            instance_with_identity(base.path(), &ProcessIdentity::unverified(decoy.id() as i32));

        let err = teardown_instance_with_timeout(
            &paths,
            Path::new("/nonexistent/skillfs-mount"),
            Duration::from_millis(300),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&decoy.id().to_string()) && msg.contains("still running"),
            "got: {msg}"
        );

        let survived = decoy.try_wait().unwrap().is_none();
        let _ = decoy.kill();
        let _ = decoy.wait();
        assert!(
            survived,
            "a pid without a verifiable identity must not be signaled"
        );
        assert!(paths.supervisor_pid.exists(), "records must be kept");
        assert!(paths.worker_pid.exists(), "records must be kept");
        assert_eq!(
            ManagedState::load(&paths.state).unwrap().desired_state,
            DesiredState::Stopped,
            "the stopped marker must be kept for the incumbent"
        );
    }

    #[test]
    fn stop_waits_for_a_live_legacy_incumbent_before_freeing_the_slot() {
        // The review scenario: stop against a legacy bare-pid record whose
        // process is a real, slow-to-exit incumbent. The Stopped state is
        // its cooperative stop; stop must block until the pid is confirmed
        // gone (here: the incumbent lives ~2s) and only then remove the
        // records and report success — never free the slot while it runs.
        let base = tempfile::tempdir().unwrap();
        let mut incumbent = spawn_sleep("2");
        let pid = incumbent.id();
        // Production incumbents are reaped by init; reap the decoy from a
        // helper thread so the pid genuinely disappears when it exits
        // instead of lingering as a zombie.
        let reaper = std::thread::spawn(move || {
            let _ = incumbent.wait();
        });
        let paths = instance_with_identity(base.path(), &ProcessIdentity::unverified(pid as i32));

        let started = Instant::now();
        teardown_instance(&paths, Path::new("/nonexistent/skillfs-mount")).unwrap();
        let waited = started.elapsed();
        let _ = reaper.join();

        assert!(
            waited >= Duration::from_millis(1_500),
            "stop must wait for the incumbent to exit; returned after {waited:?}"
        );
        assert!(
            waited < Duration::from_millis(STOP_TIMEOUT_MS),
            "a cooperating incumbent must not exhaust the stop timeout"
        );
        assert!(!paths.supervisor_pid.exists());
        assert!(!paths.worker_pid.exists());
        assert!(!identity_sidecar(&paths.supervisor_pid).exists());
        assert!(!identity_sidecar(&paths.worker_pid).exists());
        assert!(!paths.state.exists());
    }

    #[test]
    fn enosys_pidfd_refuses_to_bare_kill_a_verified_live_process() {
        // Regression for the ENOSYS window: with pidfd_open unavailable,
        // signaling must not degrade to kill(2) after the identity check.
        // The process is verified alive yet must survive the signal call.
        let mut decoy = spawn_sleep("300");
        let recorded = live_identity_of(decoy.id());

        let delivered = with_pidfd_enosys(|| signal_identity(&recorded, libc::SIGTERM));

        let survived = decoy.try_wait().unwrap().is_none();
        let _ = decoy.kill();
        let _ = decoy.wait();
        assert!(
            !delivered,
            "signal_identity must report refusal when no pidfd can be opened"
        );
        assert!(
            survived,
            "ENOSYS must not fall back to a bare kill of a verified-live pid"
        );
    }

    #[test]
    fn enosys_teardown_fails_explicitly_and_keeps_records() {
        // With no safe handle available, teardown of a verified-live pair
        // cannot terminate it: it must fail explicitly and keep every
        // record (the state already says stopped) instead of deleting the
        // files and reporting success — completing would let the next
        // mount race the survivor.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let recorded = live_identity_of(decoy.id());
        let paths = instance_with_identity(base.path(), &recorded);

        let result = with_pidfd_enosys(|| {
            teardown_instance_with_timeout(
                &paths,
                Path::new("/nonexistent/skillfs-mount"),
                Duration::from_millis(300),
            )
        });

        let survived = decoy.try_wait().unwrap().is_none();
        let _ = decoy.kill();
        let _ = decoy.wait();
        assert!(survived, "no signal path may reach the process");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("signal refused") && err.contains(&decoy.id().to_string()),
            "got: {err}"
        );
        assert!(paths.supervisor_pid.exists(), "records must be kept");
        assert!(paths.worker_pid.exists(), "records must be kept");
        assert_eq!(
            ManagedState::load(&paths.state).unwrap().desired_state,
            DesiredState::Stopped
        );
    }

    #[test]
    fn orphan_replacement_refuses_when_the_signal_cannot_be_delivered() {
        // An orphan worker we cannot safely signal must never be replaced:
        // the caller keeps its pid record and surfaces an error instead of
        // starting a second worker over the still-live one.
        let mut decoy = spawn_sleep("300");
        let orphan = live_identity_of(decoy.id());

        let result =
            with_pidfd_enosys(|| replace_orphan_worker(&orphan, Duration::from_millis(300)));

        let survived = decoy.try_wait().unwrap().is_none();
        let _ = decoy.kill();
        let _ = decoy.wait();
        assert!(survived, "the orphan must not be bare-killed");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("could not be terminated") && err.contains(&decoy.id().to_string()),
            "got: {err}"
        );
    }

    #[test]
    fn teardown_signals_a_matching_identity_and_cleans_pid_files() {
        // A pid whose recorded identity still matches — same start time
        // and full exe path — is the managed process: it must be
        // terminated and the pid files removed.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let recorded = live_identity_of(decoy.id());
        let paths = instance_with_identity(base.path(), &recorded);

        teardown_instance(&paths, Path::new("/nonexistent/skillfs-mount")).unwrap();

        let exited = decoy.try_wait().unwrap().is_some();
        if !exited {
            let _ = decoy.kill();
            let _ = decoy.wait();
        }
        assert!(
            exited,
            "the process the identity was recorded from must be signaled"
        );
        assert!(!paths.supervisor_pid.exists());
        assert!(!paths.worker_pid.exists());
        // The identity sidecars go with their pid files, never left behind
        // to pair with a later record.
        assert!(!identity_sidecar(&paths.supervisor_pid).exists());
        assert!(!identity_sidecar(&paths.worker_pid).exists());
        assert!(!paths.state.exists());
    }

    #[test]
    fn signal_identity_refuses_when_the_recorded_process_is_gone() {
        // The identity was recorded, then the process exited. Whatever
        // owns the pid now — nothing, or a recycled unrelated process —
        // must not receive the signal: verification (or the pidfd open,
        // for a reaper-race exit) refuses first.
        let base = tempfile::tempdir().unwrap();
        let mut decoy = spawn_sleep("300");
        let recorded = live_identity_of(decoy.id());
        let _paths = instance_with_identity(base.path(), &recorded);

        decoy.kill().expect("kill decoy");
        decoy.wait().expect("reap decoy so the pid is released");

        assert!(
            !signal_identity(&recorded, libc::SIGTERM),
            "a signal must not be delivered after the recorded process is gone"
        );
    }
}
