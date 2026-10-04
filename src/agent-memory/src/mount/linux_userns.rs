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

/// Tracks that unshare(NEWUSER) itself ran. unshare(NEWUSER) is one-shot
/// per task — a second call EINVALs — so once this is set the unshare must
/// never be attempted again, even if the map writes that follow failed.
static UNSHARED: AtomicBool = AtomicBool::new(false);
/// Tracks that the one-shot map writes (setgroups/uid_map/gid_map) all
/// succeeded, i.e. stage 1 is genuinely complete. `UNSHARED` alone cannot
/// distinguish "stage 1 done" from "stage 1 half-done": if a map write
/// failed after the unshare, the process is stuck inside an unmapped user
/// namespace and a retry must keep propagating `UserNsUnrecoverable`
/// instead of proceeding to the mount steps (which would succeed — the
/// process holds full capabilities in the namespace it created — and
/// return Ok in a namespace where every home-dir syscall runs as
/// `nobody`).
static MAPS_OK: AtomicBool = AtomicBool::new(false);
/// Tracks completion of the mount steps (private /, tmpfs /mnt, mkdir
/// /mnt/memory). Each mount call is individually idempotent (EBUSY when
/// already done), so a partial failure is safe to retry by calling
/// enter() again — which `auto` strategy needs to fall back cleanly.
static MOUNTS_READY: AtomicBool = AtomicBool::new(false);
/// Serialises concurrent enter() calls so the unshare/maps stage runs
/// exactly once even under contention.
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// What a fresh `enter()` call must do, given the recorded stage state.
///
/// The decision is kept as a pure function so the retry semantics are
/// unit-testable without actually unsharing the test process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnterStage {
    /// Never unshared: run the one-shot unshare + map writes, then mounts.
    UnshareAndMap,
    /// Unshare and map writes both done: only the idempotent mount steps
    /// remain (a mid-mount failure is safe to retry).
    MountsOnly,
    /// Unshare ran but a map write failed: the process is stuck in a
    /// half-initialised user namespace. Every retry must propagate
    /// `UserNsUnrecoverable`; proceeding mount-only would return Ok
    /// inside a namespace whose uid/gid are unmapped.
    PoisonedRetry,
}

fn enter_stage(unshared: bool, maps_ok: bool) -> EnterStage {
    match (unshared, maps_ok) {
        (false, _) => EnterStage::UnshareAndMap,
        (true, true) => EnterStage::MountsOnly,
        (true, false) => EnterStage::PoisonedRetry,
    }
}

pub struct LinuxUserNsMount;

impl LinuxUserNsMount {
    /// Enter the new namespace + set up `/mnt` as a private tmpfs.
    /// Idempotent at the process level: if mounts are already set up,
    /// returns Ok. If a prior call failed mid-mount (after the map writes
    /// succeeded), retry mount steps without re-running the one-shot
    /// unshare/maps stage. If a prior call failed mid-map-write, every
    /// retry returns `UserNsUnrecoverable` — the process is stuck in an
    /// unmapped namespace and must not keep running.
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

        // Stage 1: unshare + write maps. Runs at most once. Mark UNSHARED
        // immediately after unshare succeeds so a maps-stage failure does
        // not leave the process able to call unshare(NEWUSER) again
        // (which would EINVAL). Maps failures are reported as
        // UserNsUnrecoverable so the auto-fallback path knows the
        // process is in a broken user namespace and refuses to keep
        // running under userland (which would silently produce wrong
        // ownership / permissions on every home-dir syscall).
        match enter_stage(
            UNSHARED.load(Ordering::Acquire),
            MAPS_OK.load(Ordering::Acquire),
        ) {
            EnterStage::PoisonedRetry => {
                // A previous call unshared but its map writes failed. The
                // original failure was UserNsUnrecoverable; a retry that
                // skipped stage 1 and succeeded at the mounts would return
                // Ok inside the same broken namespace, defeating the
                // auto-fallback's fail-hard arm. Keep propagating.
                return Err(MemoryError::UserNsUnrecoverable(
                    "retry after failed uid/gid map writes: process is stuck \
                     inside an unmapped user namespace"
                        .into(),
                ));
            }
            EnterStage::UnshareAndMap => {
                // Record the launching uid *before* the unshare below makes
                // `geteuid()` report the mapped one, so the per-user session
                // fallback name can still tell host users apart. `main`
                // already did this; it is repeated here because `enter` is
                // also reached from `MemoryService::new` in library
                // consumers.
                crate::host::capture_host_uid();

                let real_uid = geteuid().as_raw();
                let real_gid = getegid().as_raw();

                unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)
                    .map_err(|e| MemoryError::Other(format!("unshare(NEWUSER|NEWNS): {e}")))?;
                UNSHARED.store(true, Ordering::Release);

                // Order: setgroups=deny -> uid_map -> gid_map (kernel
                // requirement). From here on we are inside a fresh user
                // namespace; any error is unrecoverable for this process —
                // see UserNsUnrecoverable.
                write_proc_unrecoverable("/proc/self/setgroups", "deny")?;
                write_proc_unrecoverable("/proc/self/uid_map", &format!("0 {real_uid} 1"))?;
                write_proc_unrecoverable("/proc/self/gid_map", &format!("0 {real_gid} 1"))?;
                // Stage 1 is only complete once the maps are in place.
                MAPS_OK.store(true, Ordering::Release);
            }
            EnterStage::MountsOnly => {}
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
mod stage_tests {
    use super::{EnterStage, LinuxUserNsMount, MAPS_OK, MOUNTS_READY, UNSHARED, enter_stage};
    use crate::error::MemoryError;

    #[test]
    fn fresh_process_runs_the_full_stage() {
        assert_eq!(enter_stage(false, false), EnterStage::UnshareAndMap);
    }

    #[test]
    fn completed_stage_one_only_redo_mounts() {
        assert_eq!(enter_stage(true, true), EnterStage::MountsOnly);
    }

    #[test]
    fn half_done_stage_one_poisons_the_retry() {
        // unshare succeeded but a map write failed: the retry must NOT
        // proceed to the mount steps — that would return Ok inside a
        // namespace whose uid/gid are unmapped, defeating the
        // UserNsUnrecoverable propagation the auto-fallback relies on.
        assert_eq!(enter_stage(true, false), EnterStage::PoisonedRetry);
    }

    #[test]
    fn enter_propagates_unrecoverable_on_poisoned_retry() {
        // Simulate the poisoned state (unshared, maps never landed) and
        // check the retry through enter() itself. The poisoned branch is
        // taken before any syscall, so this is safe to run in-process.
        // Pre-fix, this state sailed into the stage-2 mount steps and
        // could return Ok (or a recoverable mount error) inside the
        // broken namespace.
        UNSHARED.store(true, std::sync::atomic::Ordering::SeqCst);
        MAPS_OK.store(false, std::sync::atomic::Ordering::SeqCst);
        MOUNTS_READY.store(false, std::sync::atomic::Ordering::SeqCst);
        let err = LinuxUserNsMount::enter().err().expect("enter must fail");
        assert!(
            matches!(err, MemoryError::UserNsUnrecoverable(_)),
            "expected UserNsUnrecoverable, got: {err:?}"
        );
        // Restore the fresh state for any test that runs after this one.
        UNSHARED.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
