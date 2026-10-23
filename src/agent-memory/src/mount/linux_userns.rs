//! Linux user-namespace mount strategy (Phase 2).
//!
//! Pipeline (run once at process startup):
//!
//! 1. `unshare(CLONE_NEWUSER | CLONE_NEWNS)` — enter fresh `(user, mount)`
//!    namespaces. Inside, our uid is 0 (mapped to the real uid on host).
//! 2. Write `/proc/self/setgroups = "deny"` (required before gid_map on
//!    kernels 4.6+) and `/proc/self/{uid_map,gid_map}` with a single
//!    line `0 <real> 1`.
//! 3. `mount("none", "/mnt", "tmpfs", ...)` — overlay a private tmpfs on
//!    the host `/mnt` so we can create directories without touching real
//!    `/mnt`.
//! 4. `mkdir -p /mnt/memory/<ns>` and bind-mount `<base>/<ns>/` onto it.
//!
//! After this, every subsequent file IO that targets `/mnt/memory/<ns>/`
//! is transparently redirected to `<base>/<ns>/` on the home filesystem,
//! and host-side processes see nothing under `/mnt`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};
use nix::unistd::{getegid, geteuid};

use crate::error::{MemoryError, Result};
use crate::ns::Namespace;

use super::MountStrategy;

const MNT_ROOT: &str = "/mnt";
const MNT_MEMORY: &str = "/mnt/memory";

/// Tracks completion of the unshare itself. unshare(NEWUSER) is one-shot per
/// task — a second call EINVALs — so this is set as soon as the unshare
/// returns, whether or not the map writes that follow succeed. Read together
/// with `MAPS_OK` by `stage1_action`.
static UNSHARED: AtomicBool = AtomicBool::new(false);
/// Tracks completion of the uid/gid map writes. Kept separate from `UNSHARED`
/// because a failed write strands the process inside a namespace it can never
/// use: `UNSHARED` alone would let a retry fall through to the stage-2 mounts,
/// which succeed there and make `enter()` report `Ok` for an unmapped uid.
static MAPS_OK: AtomicBool = AtomicBool::new(false);
/// Why the map writes failed, so every retried `enter()` reports the original
/// cause. Written and read only while `INIT_LOCK` is held.
static MAP_FAILURE: Mutex<Option<String>> = Mutex::new(None);
/// Tracks completion of the mount steps (private /, tmpfs /mnt, mkdir
/// /mnt/memory). Each mount call is individually idempotent (EBUSY when
/// already done), so a partial failure is safe to retry by calling
/// enter() again — which `auto` strategy needs to fall back cleanly.
static MOUNTS_READY: AtomicBool = AtomicBool::new(false);
/// Serialises concurrent enter() calls so the unshare/maps stage runs
/// exactly once even under contention.
static INIT_LOCK: Mutex<()> = Mutex::new(());

pub struct LinuxUserNsMount;

impl LinuxUserNsMount {
    /// Enter the new namespace + set up `/mnt` as a private tmpfs.
    /// Idempotent at the process level: if mounts are already set up,
    /// returns Ok. If a prior call failed mid-mount, retry mount steps
    /// without re-running the one-shot unshare/maps stage.
    ///
    /// # Errors
    /// Returns [`MemoryError::UserNsUnrecoverable`] when an earlier call
    /// unshared the user namespace but failed to write the uid/gid maps.
    /// That verdict is stable: the process cannot leave the half-initialised
    /// namespace, so every later call fails the same way rather than mounting
    /// on top of it.
    pub fn enter() -> Result<Self> {
        if MOUNTS_READY.load(Ordering::Acquire) {
            return Ok(Self);
        }

        // INIT_LOCK establishes happens-before; Acquire is sufficient for
        // the second-check load (no need for SeqCst).
        let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if MOUNTS_READY.load(Ordering::Acquire) {
            return Ok(Self);
        }

        // Stage 1: unshare + write maps. Runs at most once. Maps failures are
        // reported as UserNsUnrecoverable so the auto-fallback path knows the
        // process is in a broken user namespace and refuses to keep running
        // under userland (which would silently produce wrong ownership /
        // permissions on every home-dir syscall).
        match stage1_action(
            UNSHARED.load(Ordering::Acquire),
            MAPS_OK.load(Ordering::Acquire),
        ) {
            Stage1::Skip => {}
            Stage1::Poisoned => return Err(poisoned_namespace_error()),
            Stage1::Run => {
                // Record the launching uid *before* the unshare below makes
                // `geteuid()` report the mapped one, so the per-user session
                // fallback name can still tell host users apart. `main` already
                // did this; it is repeated here because `enter` is also reached
                // from `MemoryService::new` in library consumers.
                crate::host::capture_host_uid();

                let real_uid = geteuid().as_raw();
                let real_gid = getegid().as_raw();

                // A failure here happens before the namespace exists, so the
                // process is still usable and `auto` may fall back.
                unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)
                    .map_err(|e| MemoryError::Other(format!("unshare(NEWUSER|NEWNS): {e}")))?;
                UNSHARED.store(true, Ordering::Release);

                // From here on we are inside a fresh user namespace; any error
                // is unrecoverable for this process — see UserNsUnrecoverable.
                // Record the cause so a retry repeats the verdict instead of
                // proceeding to the stage-2 mounts.
                if let Err(e) = write_id_maps(real_uid, real_gid) {
                    record_map_failure(&e);
                    return Err(e);
                }
                MAPS_OK.store(true, Ordering::Release);
            }
        }

        // Stage 2: mount setup. Each step is idempotent — EBUSY signals
        // a prior call already established the state — so a failure at
        // any step can be retried by calling enter() again without
        // re-entering the namespace.
        if let Err(e) = mount::<str, str, str, str>(
            None,
            "/",
            None,
            MsFlags::MS_PRIVATE | MsFlags::MS_REC,
            None,
        ) {
            if e != nix::errno::Errno::EBUSY {
                return Err(MemoryError::Other(format!("mount-private /: {e}")));
            }
        }

        match mount::<str, str, str, str>(
            Some("none"),
            MNT_ROOT,
            Some("tmpfs"),
            MsFlags::empty(),
            None,
        ) {
            Ok(()) => {}
            Err(nix::errno::Errno::EBUSY) => {
                tracing::debug!("/mnt tmpfs already established in this namespace");
            }
            Err(e) => return Err(MemoryError::Other(format!("tmpfs /mnt: {e}"))),
        }

        std::fs::create_dir_all(MNT_MEMORY)?;

        MOUNTS_READY.store(true, Ordering::Release);
        Ok(Self)
    }
}

/// What stage 1 (unshare + uid/gid maps) must do for this `enter()` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage1 {
    /// Neither the unshare nor the map writes have run yet.
    Run,
    /// Both succeeded. The stage is one-shot, so it must not run again.
    Skip,
    /// The unshare landed but a map write failed. The process is inside a user
    /// namespace with no uid mapping and has no way out.
    Poisoned,
}

/// Decide stage 1 from the two facts recorded about this process. Deliberately
/// free of syscalls so the retry behaviour is unit-testable: making a real
/// `/proc/self/uid_map` write fail needs root or userns control over `/proc`.
fn stage1_action(unshared: bool, maps_ok: bool) -> Stage1 {
    if !unshared {
        // `maps_ok` cannot be set before the unshare that enables the map
        // writes. Ignore it rather than let a stray flag skip namespace setup.
        return Stage1::Run;
    }
    if maps_ok {
        Stage1::Skip
    } else {
        Stage1::Poisoned
    }
}

/// Write `/proc/self/{setgroups,uid_map,gid_map}` for the namespace the caller
/// just unshared. The kernel requires exactly this order.
fn write_id_maps(real_uid: u32, real_gid: u32) -> Result<()> {
    write_proc_unrecoverable("/proc/self/setgroups", "deny")?;
    write_proc_unrecoverable("/proc/self/uid_map", &format!("0 {real_uid} 1"))?;
    write_proc_unrecoverable("/proc/self/gid_map", &format!("0 {real_gid} 1"))
}

/// Remember the map-write failure that poisoned this process. The first cause
/// wins, so a later retry cannot overwrite it. Called with `INIT_LOCK` held.
fn record_map_failure(err: &MemoryError) {
    let mut slot = MAP_FAILURE.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = Some(err.to_string());
    }
}

/// Verdict for an `enter()` retry after a failed map write. The stage-2 mounts
/// would succeed inside the unmapped namespace and leave every later home-dir
/// syscall running as `nobody`, so the failure has to stay sticky.
fn poisoned_namespace_error() -> MemoryError {
    let cause = MAP_FAILURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| "uid/gid map write failed".to_string());
    MemoryError::UserNsUnrecoverable(format!(
        "an earlier enter() left this process in an unmapped user namespace; refusing to retry: {cause}"
    ))
}

/// Write `body` into the procfs path that controls a one-shot user-ns
/// mapping (setgroups / uid_map / gid_map). Any error here means the
/// process is stuck inside a half-initialised user namespace, so the
/// error is wrapped in `UserNsUnrecoverable` to prevent the caller's
/// fallback path from silently downgrading to userland.
fn write_proc_unrecoverable(path: &str, body: &str) -> Result<()> {
    write_proc(path, body).map_err(|e| {
        MemoryError::UserNsUnrecoverable(format!(
            "wrote unshare(NEWUSER) but {path} update failed: {e}"
        ))
    })
}

impl MountStrategy for LinuxUserNsMount {
    fn ensure(&self, ns: &Namespace, base: &Path) -> Result<PathBuf> {
        // 1. Ensure backing data dir exists in the user's home.
        let backing = base.join(ns.dir_name());
        std::fs::create_dir_all(&backing)?;
        // 2. Populate README + manifest in the backing dir BEFORE bind —
        //    bind transparently exposes them at the public path.
        super::populate_mount_dir(&backing, ns)?;

        // 3. Public path inside our namespace: /mnt/memory/<ns>/.
        let public = PathBuf::from(MNT_MEMORY).join(ns.dir_name());
        std::fs::create_dir_all(&public)?;

        // 4. bind-mount backing → public. If already bound (e.g. retried),
        //    `mount` returns EBUSY; treat as success.
        match mount::<Path, Path, str, str>(Some(&backing), &public, None, MsFlags::MS_BIND, None) {
            Ok(()) => {}
            Err(nix::errno::Errno::EBUSY) => {
                tracing::debug!(
                    "bind {} -> {} already mounted",
                    backing.display(),
                    public.display()
                );
            }
            Err(e) => {
                return Err(MemoryError::Other(format!(
                    "bind {} -> {}: {e}",
                    backing.display(),
                    public.display()
                )));
            }
        }

        Ok(public)
    }

    fn name(&self) -> &'static str {
        "linux-userns"
    }
}

fn write_proc(path: &str, body: &str) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| MemoryError::Other(format!("open {path}: {e}")))?;
    f.write_all(body.as_bytes())
        .map_err(|e| MemoryError::Other(format!("write {path}: {e}")))?;
    Ok(())
}

/// Read /proc/self/uid_map; a one-id mapping indicates we're in a user
/// namespace we created (vs the init ns which has the full 0..2^32-1
/// range). Used by `info` for diagnostics.
pub fn in_user_namespace() -> bool {
    // Format per line: "inside_uid  outside_uid  range".
    // - Init ns:        "0  0  4294967295"
    // - Rootless unshare with `0 <real> 1`: "0  <real>  1"
    //
    // Pre-fix this checked the second column for `"0"`, which falsely
    // reported "not in userns" when a root user (uid=0) had unshared,
    // since the outside_uid was still 0. Inspecting the range is the
    // reliable signal: anything other than the full 2^32 means we're
    // inside a confined user namespace.
    match std::fs::read_to_string("/proc/self/uid_map") {
        Ok(s) => s
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(2))
            .map(|range| range != "4294967295")
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Stage1, stage1_action};

    #[test]
    fn fresh_process_runs_stage1() {
        assert_eq!(stage1_action(false, false), Stage1::Run);
    }

    #[test]
    fn completed_stage1_is_skipped_on_retry() {
        // unshare(NEWUSER) EINVALs on a second call and the maps are
        // write-once, so a successful stage 1 must never run again.
        assert_eq!(stage1_action(true, true), Stage1::Skip);
    }

    #[test]
    fn failed_map_write_poisons_every_retry() {
        // Regression: `UNSHARED` alone used to be the gate, so a retry after a
        // failed uid_map write skipped stage 1 and ran the stage-2 mounts
        // inside the unmapped namespace. Those mounts succeed — the process
        // holds full capabilities in the userns it created — and enter()
        // returned Ok, leaving all home-dir IO running as `nobody`.
        assert_eq!(stage1_action(true, false), Stage1::Poisoned);
    }

    #[test]
    fn maps_ok_without_unshare_still_runs_stage1() {
        // Unreachable in practice; guards the invariant that stage 1 is only
        // ever skipped on evidence of a completed unshare.
        assert_eq!(stage1_action(false, true), Stage1::Run);
    }
}
