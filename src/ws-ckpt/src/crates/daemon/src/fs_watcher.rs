use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{error, warn};

/// Monotonic watcher generation counter: every `start` increments it, so
/// tests can observe re-arming (a fresh watcher has a strictly larger seq)
/// without waiting on inotify event delivery.
static WATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long after any registration walk the "no writes observed" shortcut is
/// disabled. notify's event loop does not read the inotify descriptor while
/// it walks a tree, so events generated during (and right after) a walk can
/// still be queued when a quiescence check starts. The shortcut is only safe
/// once those events have had time to drain.
const REGISTRATION_SETTLE: Duration = Duration::from_millis(250);

/// A path that is never registered. `unwatch` on it is a pure control
/// message to notify's event loop: the reply proves the loop finished any
/// registration walk it was already executing (including its internal
/// auto-add walk) and is back to handling messages.
const REGISTRATION_BARRIER_PATH: &str = "/proc/self/__ws_ckpt_registration_barrier__";

/// Instance-wide registration activity for the shared watcher.
///
/// notify 6.1.1 runs every recursive `watch()` walk and every auto-add of a
/// newly created or moved-in directory on its single event-loop thread, and
/// does not read the inotify descriptor while walking. With one shared
/// instance that thread serves every workspace, so a walk pauses event
/// delivery for all of them (#8448). A quiescence check must therefore not
/// treat a quiet window that overlaps a registration as evidence: the flag
/// can stay silent only because the events are still queued in the kernel.
///
/// `epoch`/`active` cover the registrations we drive ourselves (the caller
/// blocks in `watch()` until the walk is done); `auto_add_*` cover notify's
/// internal auto-add, whose end is observed through a control-message
/// barrier on the same event loop.
#[derive(Default)]
struct RegistrationState {
    /// Bumped at the start and at the end of every registration we drive.
    /// A check that sees a different value at decision time cannot trust the
    /// observation window.
    epoch: AtomicU64,
    /// Registrations currently in flight. Non-zero at the start or at the
    /// decision means events may still be queued behind a walk.
    active: AtomicU32,
    /// Directory create/rename-in events notify may turn into an auto-add
    /// walk (one per event), and the highest sequence the barrier proved
    /// finished.
    auto_add_seq: AtomicU64,
    auto_add_covered: AtomicU64,
    /// One barrier round-trip at a time; a burst of directory events is
    /// covered by the same message.
    barrier_in_flight: AtomicBool,
    /// After a registration ends, queued events may still be undelivered:
    /// the "not writing => Safe" shortcut stays disabled until this instant.
    settle_until: Mutex<Option<Instant>>,
}

impl RegistrationState {
    fn begin(&self) {
        self.active.fetch_add(1, Ordering::AcqRel);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    fn end(&self) {
        // Publish the settle window before the in-flight count drops: a
        // check that observes "not busy" must also observe "settling", or
        // it could take the idle shortcut against still-queued events.
        self.settle();
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.active.fetch_sub(1, Ordering::AcqRel);
    }

    /// A registration walk just finished; queued events need time to drain
    /// before a silent flag may be trusted again.
    fn settle(&self) {
        *self
            .settle_until
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(Instant::now() + REGISTRATION_SETTLE);
    }

    fn settling(&self) -> bool {
        self.settle_until
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_some_and(|until| Instant::now() < until)
    }

    fn busy(&self) -> bool {
        self.active.load(Ordering::Acquire) > 0
    }

    /// Every auto-add event counted so far has had a post-walk barrier
    /// reply, so no walk is known to be in flight.
    fn auto_add_settled(&self) -> bool {
        self.auto_add_covered.load(Ordering::Acquire) >= self.auto_add_seq.load(Ordering::Acquire)
    }
}

/// Route- and instance-level evidence that a quiet window stayed valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuiescenceEvidence {
    coverage_epoch: u64,
    registration_epoch: u64,
    auto_add_seq: u64,
}

/// Whether a registration walk is pausing event delivery on `shared`, so a
/// silent write flag proves nothing (#8448).
pub(crate) fn delivery_paused(shared: &Arc<SharedWatcher>) -> bool {
    !shared.registration_quiet()
}

/// One quiet-period attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationAttempt {
    /// The window stayed quiet and the evidence was valid throughout.
    Quiet,
    /// A write was observed inside the window.
    Active,
    /// The window overlapped a registration walk / coverage change: its
    /// silence proves nothing and the check must observe again.
    Unstable,
}

/// One shared quiet-period attempt.
///
/// The watcher helper and the daemon gate both call this, so the
/// registration/coverage evidence cannot drift between the two entry points
/// (#8448). `still_current` must re-read the same evidence and reject a
/// replaced watcher; `settling` only disables the not-writing shortcut, since
/// events queued behind a just-finished walk can still be undelivered.
pub(crate) async fn observe_once<F>(
    flag: &AtomicBool,
    force_observation: bool,
    settling: bool,
    still_current: F,
) -> ObservationAttempt
where
    F: Fn() -> bool,
{
    if !still_current() {
        return ObservationAttempt::Unstable;
    }
    if !force_observation && !settling && !flag.load(Ordering::Acquire) {
        return if still_current() {
            ObservationAttempt::Quiet
        } else {
            ObservationAttempt::Unstable
        };
    }
    // Wait 100ms quiet period
    tokio::time::sleep(Duration::from_millis(100)).await;
    flag.store(false, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(100)).await;
    if flag.load(Ordering::Acquire) {
        return ObservationAttempt::Active;
    }
    if still_current() {
        ObservationAttempt::Quiet
    } else {
        ObservationAttempt::Unstable
    }
}

/// Pairs `begin` with `end` on every return path (including the failure
/// branches that release partial registrations).
struct RegistrationGuard<'a>(&'a RegistrationState);

impl<'a> RegistrationGuard<'a> {
    fn new(registration: &'a RegistrationState) -> Self {
        registration.begin();
        Self(registration)
    }
}

impl Drop for RegistrationGuard<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Recursive workspace write watcher. CLOSE_WRITE clears the flag so
/// checkpoint can skip the quiescence wait when all writers have closed.
pub struct WorkspaceWatcher {
    is_writing: Arc<AtomicBool>,
    /// Route identity in [`SharedWatcher`]; dropping the handle releases
    /// the route (and, with the last handle for a path, the watch itself).
    workspace_path: PathBuf,
    shared: Arc<SharedWatcher>,
    /// (dev, ino) of the directory the inotify watch was armed on, captured
    /// at `start` by following the registration symlink. inotify watches
    /// inodes, not paths: after a rollback swaps the live subvolume behind
    /// the registration path, this is how we detect the watch went stale.
    target: (u64, u64),
    watch_seq: u64,
}

/// The process-wide inotify instance every workspace watcher shares.
///
/// `notify::recommended_watcher` performs one `inotify_init` per watcher,
/// and Linux caps instances per user (`fs.inotify.max_user_instances`,
/// default 128): one watcher per workspace made the ~129th registration
/// fail while the daemon only warned and kept going, silently dropping
/// change detection for most of a large deployment (#7233). One shared
/// instance removes that cap; the recursive watches already share the
/// per-user watch-descriptor budget (`max_user_watches`) exactly as they
/// did with one instance per workspace.
///
/// Events are routed to the owning workspaces by walking the event path's
/// ancestors, so a write inside an overlapping root still flags every
/// containing workspace, matching what per-workspace instances observed.
/// Registered workspace roots and the write-activity flags of the live
/// handles on each root (normally one; a re-arm briefly keeps two).
type Routes = RwLock<HashMap<PathBuf, Vec<Arc<AtomicBool>>>>;

pub(crate) struct SharedWatcher {
    watcher: Arc<Mutex<RecommendedWatcher>>,
    routes: Arc<Routes>,
    /// Instance-wide registration activity (ours and notify's auto-add);
    /// see [`RegistrationState`].
    registration: Arc<RegistrationState>,
    /// Cumulative inotify queue overflows (`IN_Q_OVERFLOW`) observed on
    /// this instance, exposed through the watcher-health IPC surface.
    queue_overflows: Arc<AtomicU64>,
    /// Per-route kernel-coverage bookkeeping. A leaf lock: never held
    /// while acquiring `watcher` or `routes`.
    ///
    /// The route cannot simply be dropped when coverage is lost: the
    /// handles keyed to it stay alive in [`crate::state::DaemonState`]
    /// and their write-activity flags would keep looking like evidence
    /// (a permanently silent `false` reads as quiescent). The
    /// `uncovered` mark makes the loss explicit instead:
    /// [`WorkspaceWatcher::is_degraded`] feeds the health surface and the
    /// quiescence heal path, and only a successful re-registration
    /// ([`SharedWatcher::add_route`]) clears it. The interruption counter
    /// additionally lets a quiescence check detect a coverage change
    /// *during* its quiet-period observation: an interruption that
    /// started and ended between the observation's snapshot and its
    /// decision leaves the route healthy again, but the flag's silence
    /// proves nothing about the writes that happened while no descriptor
    /// covered them (#7592 review).
    coverage: RwLock<HashMap<PathBuf, RouteCoverage>>,
}

/// Per-route kernel-coverage state: whether coverage is currently known
/// gone, and how many interruptions it has survived.
#[derive(Default)]
struct RouteCoverage {
    /// Kernel coverage is gone while the route stays published (a
    /// survivor re-arm failed after an overlapping release).
    uncovered: bool,
    /// Monotonic count of coverage interruptions — every transition into
    /// `uncovered`. Observers snapshot it before a quiet-period
    /// observation and must void a `Safe` decision when it moved, even
    /// if coverage was restored in between.
    interruptions: u64,
}

/// Lock discipline inside [`SharedWatcher`], all locks private to this
/// module:
///
/// * `watcher` serializes every registration mutation (route add/remove).
///   It is always acquired *before* `routes`.
/// * `routes` is only ever write-locked for plain map edits, never across
///   a `watch()`/`unwatch()` call: those block until the notify event-loop
///   thread replies, and that same thread takes `routes.read()` inside the
///   delivery callback — holding the write side across the call could
///   deadlock the event loop against ourselves. The event loop never
///   touches the `watcher` mutex, so kernel calls under it are safe.
/// * `degraded`/`coverage` is a leaf lock: taken under `watcher`
///   (registration paths) or standalone (health queries), and never held
///   while acquiring another lock.
impl SharedWatcher {
    fn new() -> anyhow::Result<Self> {
        let routes = Arc::new(RwLock::new(HashMap::<PathBuf, Vec<Arc<AtomicBool>>>::new()));
        let queue_overflows = Arc::new(AtomicU64::new(0));
        let registration = Arc::new(RegistrationState::default());
        // The event callback needs to send a control message back through the
        // event loop that is currently running it. The slot is filled right
        // after the watcher is built; no event can fire before the first
        // route is published, which happens after `new` returns.
        let control: Arc<OnceLock<Weak<Mutex<RecommendedWatcher>>>> = Arc::new(OnceLock::new());
        let callback_routes = Arc::clone(&routes);
        let callback_overflows = Arc::clone(&queue_overflows);
        let callback_registration = Arc::clone(&registration);
        let callback_control = Arc::clone(&control);
        let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            forward_event(
                &callback_routes,
                &callback_overflows,
                &callback_registration,
                &callback_control,
                res,
            )
        })
        .map_err(|e| anyhow::anyhow!("Failed to init notify watcher: {}", e))?;
        let watcher = Arc::new(Mutex::new(watcher));
        let _ = control.set(Arc::downgrade(&watcher));
        Ok(Self {
            watcher,
            routes,
            queue_overflows,
            registration,
            coverage: RwLock::new(HashMap::new()),
        })
    }

    fn begin_registration(&self) -> RegistrationGuard<'_> {
        RegistrationGuard::new(&self.registration)
    }

    fn registration_epoch(&self) -> u64 {
        self.registration.epoch.load(Ordering::Acquire)
    }

    fn auto_add_seq(&self) -> u64 {
        self.registration.auto_add_seq.load(Ordering::Acquire)
    }

    fn registration_settling(&self) -> bool {
        self.registration.settling()
    }

    /// No registration walk is known to be in flight and no auto-add event is
    /// waiting for its barrier reply.
    fn registration_quiet(&self) -> bool {
        !self.registration.busy() && self.registration.auto_add_settled()
    }

    /// Cumulative `IN_Q_OVERFLOW` count on this instance.
    fn queue_overflows(&self) -> u64 {
        self.queue_overflows.load(Ordering::Relaxed)
    }

    /// Mark `root` as published-but-uncovered (kernel coverage is being
    /// or has been lost). Counts as a new interruption only on the
    /// healthy→uncovered transition, so the mark before an unwatch and
    /// the confirmation after a failed re-arm belong to the same
    /// interruption window.
    fn mark_degraded(&self, root: &Path) {
        let mut coverage = self.coverage.write().unwrap_or_else(|err| err.into_inner());
        let entry = coverage.entry(root.to_path_buf()).or_default();
        if !entry.uncovered {
            entry.uncovered = true;
            entry.interruptions += 1;
        }
    }

    /// Clear the degraded mark: a successful (re)registration restored
    /// the kernel coverage for `root`.
    fn clear_degraded(&self, root: &Path) {
        let mut coverage = self.coverage.write().unwrap_or_else(|err| err.into_inner());
        coverage.entry(root.to_path_buf()).or_default().uncovered = false;
    }

    /// Drop `root`'s coverage bookkeeping once its last route is retired,
    /// so registration churn over distinct paths does not retain an entry
    /// per historical path for the daemon's lifetime (#7592 review).
    fn retire_coverage(&self, root: &Path) {
        self.coverage
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .remove(root);
    }

    /// Whether `root`'s route is published but its kernel coverage is gone.
    fn route_degraded(&self, root: &Path) -> bool {
        self.coverage
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .get(root)
            .is_some_and(|c| c.uncovered)
    }

    /// Monotonic count of `root`'s coverage interruptions (0 when never
    /// interrupted). A quiet-period observation that spans a change of
    /// this counter cannot trust the flag's silence: writes to
    /// directories a recursive re-arm had not reached yet leave no event.
    fn coverage_epoch(&self, root: &Path) -> u64 {
        self.coverage
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .get(root)
            .map_or(0, |c| c.interruptions)
    }

    /// Release the kernel watch on `root` without silencing other
    /// registered roots that overlap it.
    ///
    /// notify 6.1.1 keys one descriptor per path, and a recursive
    /// `unwatch(root)` removes EVERY descriptor whose path starts with
    /// `root` — including descriptors other registered roots rely on: a
    /// workspace nested inside this one, or an ancestor whose recursive
    /// walk covers this tree. Their routes would stay published and status
    /// would keep reporting them healthy while no event ever arrives
    /// again. So, before the first descriptor goes away, latch every
    /// overlapping survivor to "writing" AND mark its route uncovered
    /// (#7592 review: coverage must be unavailable throughout the
    /// interruption — a concurrent quiescence check that is already past
    /// its initial degraded probe must still find the interruption when
    /// it decides, so the mark goes up before the unwatch, not after a
    /// failed re-arm), then unwatch, then re-arm each survivor. A re-arm
    /// that succeeds clears the mark; one that fails keeps it — fail
    /// closed — instead of leaving a route whose silent flag reads as
    /// quiescence.
    fn unwatch_preserving_overlaps(&self, watcher: &mut RecommendedWatcher, root: &Path) {
        self.release_overlapping(watcher, root, &mut Vec::new());
    }

    /// One overlap-preserving release of `root`: latch and mark every
    /// published route overlapping it, unwatch, then re-arm each of them.
    ///
    /// A failed re-arm keeps the route's degraded mark and releases the
    /// partial descriptors its walk installed: notify 6.1.1's recursive
    /// `watch()` adds one descriptor per directory and bails out at the
    /// first failure without rolling back, and a leaked partial set pins
    /// the per-user watch budget (`max_user_watches`) for later re-arms
    /// and heals. That release is the same recursive
    /// `unwatch`, so it disturbs every published route overlapping the
    /// failed root — not only the survivors of the original release: with
    /// `/p` ⊃ {`/p/a`, `/p/x`}, releasing `/p/a` disturbs only `/p`, but
    /// releasing a failed `/p` strips `/p/x` too (#7592 review). Roots
    /// whose re-arm failed are collected in `failed` and never disturbed
    /// again in this call tree, so each failure adds one root and the
    /// cascade is bounded by the number of routes.
    fn release_overlapping(
        &self,
        watcher: &mut RecommendedWatcher,
        root: &Path,
        failed: &mut Vec<PathBuf>,
    ) {
        let _registration = self.begin_registration();
        let survivors: Vec<PathBuf> = {
            let routes = self.routes.write().unwrap_or_else(|err| err.into_inner());
            let survivors: Vec<PathBuf> = routes
                .keys()
                .filter(|registered| registered.starts_with(root) || root.starts_with(*registered))
                .filter(|registered| !failed.contains(*registered))
                .cloned()
                .collect();
            for survivor in &survivors {
                if let Some(flags) = routes.get(survivor) {
                    for flag in flags {
                        flag.store(true, Ordering::Release);
                    }
                }
                self.mark_degraded(survivor);
            }
            survivors
        };
        let _ = watcher.unwatch(root);
        // Restore the kernel descriptors the prefix removal took away.
        // notify re-adds the root first, then walks the tree, so the
        // interruption window is bounded by one registration round-trip.
        for survivor in survivors {
            // A nested release below may already have failed this root.
            if failed.contains(&survivor) {
                continue;
            }
            match watcher.watch(&survivor, RecursiveMode::Recursive) {
                // Coverage is whole again; the interruption is over.
                Ok(()) => self.clear_degraded(&survivor),
                Err(e) => {
                    // The pre-unwatch mark stays: coverage remains
                    // unavailable for this root until a heal re-registers.
                    failed.push(survivor.clone());
                    self.release_overlapping(watcher, &survivor, failed);
                    error!(
                        "failed to re-arm the overlapping watch on {:?} while releasing {:?}: \
                         {} — change detection is degraded for that workspace until it is \
                         re-registered",
                        survivor, root, e
                    );
                }
            }
        }
    }

    fn add_route(&self, workspace_path: &Path, flag: Arc<AtomicBool>) -> anyhow::Result<()> {
        let _registration = self.begin_registration();
        let mut watcher = self.watcher.lock().unwrap_or_else(|err| err.into_inner());
        // Publish the route BEFORE arming the kernel watch. notify installs
        // kernel descriptors synchronously inside `watch()`, so an event
        // delivered right after the kernel add but before the route landed
        // in the map would find no route and be dropped forever — a writer
        // holding its file open would then look quiescent once the eventual
        // CLOSE_WRITE stored `false`. With the route published first, every
        // event that can possibly be delivered already finds it: the
        // descriptors covering this path do not exist before `watch()`.
        {
            let mut routes = self.routes.write().unwrap_or_else(|err| err.into_inner());
            routes
                .entry(workspace_path.to_path_buf())
                .or_default()
                .push(Arc::clone(&flag));
        }
        if let Err(e) = watcher.watch(workspace_path, RecursiveMode::Recursive) {
            // Roll the publication back.
            let last_handle = {
                let mut routes = self.routes.write().unwrap_or_else(|err| err.into_inner());
                let last_handle = match routes.get_mut(workspace_path) {
                    Some(flags) => {
                        flags.retain(|f| !Arc::ptr_eq(f, &flag));
                        flags.is_empty()
                    }
                    None => false,
                };
                if last_handle {
                    routes.remove(workspace_path);
                }
                last_handle
            };
            if last_handle {
                self.retire_coverage(workspace_path);
                // notify 6.1.1 does not roll back a partially installed
                // recursive registration: its walk adds one descriptor per
                // directory and bails out on the first failure (an
                // `ENOSPC`/MaxFilesWatch under `max_user_watches`
                // pressure). Unwatch the root so everything the walk did
                // manage to install is released (a recursive unwatch
                // removes the whole subtree) instead of pinning the
                // per-user watch budget until daemon restart. The release
                // preserves overlapping survivors exactly like an ordinary
                // removal: the same recursive unwatch would otherwise
                // prefix-strip an already-registered nested workspace's
                // descriptors and leave it silently dead (#7592 review).
                self.unwatch_preserving_overlaps(&mut watcher, workspace_path);
            } else {
                // A surviving handle on the same path (a re-arm racing this
                // failure) keeps the already-installed descriptors alive;
                // anything the failed walk added on top of them is released
                // together with that handle when the last one for this
                // path drops.
                warn!(
                    "partial watch descriptors from the failed registration of {:?} stay \
                     armed for the surviving watcher handle until it drops",
                    workspace_path
                );
            }
            return Err(anyhow::anyhow!(
                "Failed to add recursive watch for {:?}: {}",
                workspace_path,
                e
            ));
        }
        // A successful (re)registration restores this root's kernel
        // coverage, so any degraded mark from an earlier failed re-arm is
        // obsolete.
        self.clear_degraded(workspace_path);
        Ok(())
    }

    fn remove_route(&self, workspace_path: &Path, flag: &Arc<AtomicBool>) {
        let mut watcher = self.watcher.lock().unwrap_or_else(|err| err.into_inner());
        let last_handle = {
            // One atomic step: detach this handle's flag and, for the last
            // one, unpublish the route — a concurrent `add_route` on the
            // same path must never observe a half-removed entry.
            let mut routes = self.routes.write().unwrap_or_else(|err| err.into_inner());
            let last_handle = match routes.get_mut(workspace_path) {
                Some(flags) => {
                    flags.retain(|f| !Arc::ptr_eq(f, flag));
                    flags.is_empty()
                }
                None => false,
            };
            if last_handle {
                routes.remove(workspace_path);
            }
            last_handle
        };
        if !last_handle {
            return;
        }
        self.retire_coverage(workspace_path);
        // The recursive unwatch prefix-strips every descriptor under the
        // released root; re-arm the overlapping survivors it disturbs (or
        // mark them degraded when that fails) — see the helper.
        self.unwatch_preserving_overlaps(&mut watcher, workspace_path);
    }
}

/// Apply one notify event to every registered workspace containing it.
fn forward_event(
    routes: &Arc<Routes>,
    queue_overflows: &AtomicU64,
    registration: &Arc<RegistrationState>,
    control: &Arc<OnceLock<Weak<Mutex<RecommendedWatcher>>>>,
    res: notify::Result<Event>,
) {
    let event = match res {
        Ok(event) => event,
        Err(e) => {
            warn!("notify error: {}", e);
            return;
        }
    };
    if event.need_rescan() {
        // notify 6.1.1 surfaces inotify IN_Q_OVERFLOW as
        // Ok(EventKind::Other) carrying Flag::Rescan — it never reaches the
        // Err arm, and without this check the wildcard match below would
        // silently discard it. The kernel dropped events: every `false`
        // write-activity flag on this shared instance is now unproven, and
        // one noisy workspace must not be able to make the others look
        // quiescent. Fail closed: count the overflow for the health
        // surface and treat every watched workspace as actively written
        // until quiescence has been re-observed over a fresh quiet period.
        queue_overflows.fetch_add(1, Ordering::Relaxed);
        error!(
            "inotify queue overflow on the shared watcher instance: events were dropped; \
             every watched workspace is treated as actively written until a quiet period \
             is re-observed"
        );
        let routes = routes.read().unwrap_or_else(|err| err.into_inner());
        for flags in routes.values() {
            for flag in flags {
                flag.store(true, Ordering::Release);
            }
        }
        return;
    }
    if is_dir_registration_event(&event) {
        // notify follows a directory create or move-in with an internal
        // recursive auto-add walk on this same event-loop thread, after the
        // callback returns and before it reads the inotify descriptor again.
        // Count it and arm a barrier that can only be answered once that
        // walk is over (#8448).
        let target = registration.auto_add_seq.fetch_add(1, Ordering::AcqRel) + 1;
        arm_registration_barrier(registration, control, target);
    }
    let writing = match &event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => true,
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => false,
        _ => return,
    };
    let routes = routes.read().unwrap_or_else(|err| err.into_inner());
    for path in &event.paths {
        for ancestor in path.ancestors() {
            if let Some(flags) = routes.get(ancestor) {
                // Idempotent store: overlapping roots may match twice.
                for flag in flags {
                    flag.store(writing, Ordering::Release);
                }
            }
        }
    }
}

/// Whether notify may follow this event with an internal recursive auto-add
/// walk. notify 6.1.1 calls `add_watch_by_event` only from its CREATE and
/// MOVED_TO branches, and only when the event carries `ISDIR`; a file create
/// or rename cannot start a walk.
fn is_dir_registration_event(event: &Event) -> bool {
    match &event.kind {
        EventKind::Create(CreateKind::Folder) => true,
        EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both)) => {
            event.paths.iter().any(|path| {
                std::fs::metadata(path)
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false)
            })
        }
        _ => false,
    }
}

/// Prove through notify's own event loop that every auto-add walk counted up
/// to `target` has finished.
///
/// The barrier is an `unwatch` of a path that is never registered: notify
/// turns it into a control message and answers it on the event-loop thread.
/// A message sent while the loop is executing `handle_inotify` (which is
/// where the event callback runs, and where the auto-add walks happen) is
/// only handled after that pass returns, so the reply is ordered after the
/// walk. While the message is in flight, further directory events keep
/// bumping `auto_add_seq`; the loop covers them with another round-trip
/// instead of claiming them covered.
fn arm_registration_barrier(
    registration: &Arc<RegistrationState>,
    control: &Arc<OnceLock<Weak<Mutex<RecommendedWatcher>>>>,
    target: u64,
) {
    if registration.barrier_in_flight.swap(true, Ordering::AcqRel) {
        return;
    }
    let owner = Arc::clone(registration);
    let registration = Arc::clone(registration);
    let control = Arc::clone(control);
    let spawned = std::thread::Builder::new()
        .name("ws-ckpt-watcher-barrier".to_string())
        .spawn(move || {
            let mut target = target;
            loop {
                if let Some(watcher) = control.get().and_then(Weak::upgrade) {
                    let _ = watcher
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .unwatch(Path::new(REGISTRATION_BARRIER_PATH));
                }
                registration.settle();
                registration
                    .auto_add_covered
                    .fetch_max(target, Ordering::AcqRel);
                let next = registration.auto_add_seq.load(Ordering::Acquire);
                if registration.auto_add_covered.load(Ordering::Acquire) >= next {
                    registration
                        .barrier_in_flight
                        .store(false, Ordering::Release);
                    if registration.auto_add_covered.load(Ordering::Acquire)
                        >= registration.auto_add_seq.load(Ordering::Acquire)
                    {
                        return;
                    }
                    if registration.barrier_in_flight.swap(true, Ordering::AcqRel) {
                        return;
                    }
                    target = registration.auto_add_seq.load(Ordering::Acquire);
                    continue;
                }
                target = next;
            }
        });
    if let Err(e) = spawned {
        error!("failed to spawn the watcher registration barrier: {}", e);
        owner.barrier_in_flight.store(false, Ordering::Release);
    }
}

/// The process-wide shared watcher instance. Only successful
/// initialization is cached: `Inotify::init` can fail for transient
/// process/system pressure (`EMFILE`, `ENFILE`, `ENOMEM` — for example the
/// per-user instance limit being exhausted by another process at that
/// moment), and a later registration must retry instead of reporting a
/// permanently dead watcher until the daemon restarts. The cache mutex
/// also serializes initialization attempts, so concurrent registrations
/// never race to open more than one instance.
/// Production shares one instance for the whole daemon. Tests run many
/// daemons (and watcher scenarios) in one process, so under `cfg(test)` the
/// cache is per-thread: quiescence is instance-wide, and a registration
/// started by another test would otherwise be (correctly) reported as "not
/// quiescent" and make unrelated tests nondeterministic. Multi-route sharing
/// within a test is unchanged; it is covered by
/// `tests/watcher_shared_instance.rs`.
#[cfg(not(test))]
static SHARED_WATCHER: WatcherCache = WatcherCache::new();

#[cfg(test)]
std::thread_local! {
    static SHARED_WATCHER: WatcherCache = const { WatcherCache::new() };
}

fn shared_watcher() -> anyhow::Result<Arc<SharedWatcher>> {
    #[cfg(not(test))]
    {
        SHARED_WATCHER.get_or_init_with(SharedWatcher::new)
    }
    #[cfg(test)]
    {
        SHARED_WATCHER.with(|cache| cache.get_or_init_with(SharedWatcher::new))
    }
}

/// Cumulative inotify queue overflows on the shared watcher instance, for
/// the watcher-health IPC surface. `None` when no watcher was ever
/// initialized — nothing is being watched, so nothing could have
/// overflowed.
pub fn shared_queue_overflows() -> Option<u64> {
    #[cfg(not(test))]
    {
        SHARED_WATCHER
            .cached
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .as_ref()
            .map(|shared| shared.queue_overflows())
    }
    #[cfg(test)]
    {
        SHARED_WATCHER.with(|cache| {
            cache
                .cached
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .as_ref()
                .map(|shared| shared.queue_overflows())
        })
    }
}

/// Holds an instance-wide registration in flight, exactly as `watch()` does
/// while walking a tree. Tests use it to prove a gate fails closed while
/// another workspace's registration delays event delivery (#8448).
#[cfg(test)]
pub(crate) struct TestRegistrationHold(Arc<SharedWatcher>);

#[cfg(test)]
impl TestRegistrationHold {
    pub(crate) fn begin() -> Self {
        let shared = shared_watcher().expect("shared watcher");
        shared.registration.begin();
        Self(shared)
    }
}

#[cfg(test)]
impl Drop for TestRegistrationHold {
    fn drop(&mut self) {
        self.0.registration.end();
    }
}

/// Cache holding the one successfully initialized [`SharedWatcher`];
/// transient init failures are retried by the next caller.
struct WatcherCache {
    cached: Mutex<Option<Arc<SharedWatcher>>>,
}

impl WatcherCache {
    const fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }

    /// Return the cached instance, or initialize it with `init`.
    ///
    /// A failed `init` is never cached: the caller reports the error, and
    /// the next caller invokes `init` again — transient pressure
    /// (`EMFILE`/`ENFILE`/`ENOMEM`) clears, and monitoring recovers
    /// without a daemon restart. Only one thread runs `init` at a time.
    fn get_or_init_with(
        &self,
        init: impl FnOnce() -> anyhow::Result<SharedWatcher>,
    ) -> anyhow::Result<Arc<SharedWatcher>> {
        let mut cached = self.cached.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(shared) = cached.as_ref() {
            return Ok(Arc::clone(shared));
        }
        let shared = Arc::new(init()?);
        *cached = Some(Arc::clone(&shared));
        Ok(shared)
    }
}

impl WorkspaceWatcher {
    /// Start watching a workspace directory (recursively) for write activity.
    /// Called after workspace init or during rebuild_from_disk.
    ///
    /// All watchers share one inotify instance ([`SharedWatcher`]); a handle
    /// owns only its route and its write-activity flag.
    pub fn start(workspace_path: &Path) -> anyhow::Result<Self> {
        let shared = shared_watcher()?;
        let is_writing = Arc::new(AtomicBool::new(false));
        let path = workspace_path.to_path_buf();

        // std::fs::metadata FOLLOWS symlinks, so this is the directory
        // inode inotify actually observes — the identity to compare against
        // after the backend swaps the live subvolume behind the path.
        // Resolve it before arming the watch so a failure leaves no route.
        let meta = std::fs::metadata(&path)
            .map_err(|e| anyhow::anyhow!("cannot resolve watch target for {:?}: {}", path, e))?;

        shared.add_route(&path, Arc::clone(&is_writing))?;

        Ok(Self {
            is_writing,
            workspace_path: path,
            shared,
            target: (meta.dev(), meta.ino()),
            watch_seq: WATCH_SEQ.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// The (dev, ino) this watch is armed on.
    pub fn target_identity(&self) -> (u64, u64) {
        self.target
    }

    /// Monotonic generation of this watcher (for observing re-arms).
    pub fn watch_seq(&self) -> u64 {
        self.watch_seq
    }

    /// Whether this watcher's kernel coverage is known to be gone: a
    /// survivor re-arm failed after an overlapping release (or is still
    /// in progress), so no event can arrive while the handle and its
    /// route stay published. The quiescence heal path retries the
    /// registration and the watcher-health surface reports the workspace
    /// as uncovered until a successful re-registration clears the mark.
    pub fn is_degraded(&self) -> bool {
        self.shared.route_degraded(&self.workspace_path)
    }

    /// Monotonic count of this route's coverage interruptions. A
    /// quiescence observation snapshots it before its quiet period and
    /// must treat a change as "the observation is void": coverage was
    /// interrupted in between (possibly restored already), so the flag's
    /// silence proves nothing about the writes that happened while no
    /// descriptor covered them.
    /// Evidence sampled before a quiescence observation.
    ///
    /// The write flag only proves silence when coverage and event delivery
    /// stayed continuous for the whole observation: `coverage_epoch` covers
    /// this route, the registration counters cover the shared instance's
    /// recursive walks (ours and notify's auto-add), which pause delivery for
    /// every workspace (#8448).
    pub(crate) fn quiescence_evidence(&self) -> QuiescenceEvidence {
        QuiescenceEvidence {
            coverage_epoch: self.shared.coverage_epoch(&self.workspace_path),
            registration_epoch: self.shared.registration_epoch(),
            auto_add_seq: self.shared.auto_add_seq(),
        }
    }

    /// The instance this watcher is registered on.
    pub(crate) fn shared(&self) -> &Arc<SharedWatcher> {
        &self.shared
    }

    /// Whether a just-finished registration walk may still have events queued
    /// behind it.
    pub(crate) fn delivery_settling(&self) -> bool {
        self.shared.registration_settling()
    }

    /// Whether `evidence` still describes this watcher: coverage stayed
    /// continuous, no registration walk ran, and the route is not degraded.
    pub(crate) fn quiescence_evidence_is_current(&self, evidence: QuiescenceEvidence) -> bool {
        !self.is_degraded()
            && !self.shared.route_degraded(&self.workspace_path)
            && self.shared.coverage_epoch(&self.workspace_path) == evidence.coverage_epoch
            && self.shared.registration_quiet()
            && self.shared.registration_epoch() == evidence.registration_epoch
            && self.shared.auto_add_seq() == evidence.auto_add_seq
    }

    /// Check if workspace is quiescent (no recent writes).
    /// Returns true if safe to snapshot, false if writes are active.
    pub async fn check_quiescent(&self) -> bool {
        // Fail closed while coverage is known gone: a permanently silent
        // flag reads as quiescent but proves nothing (#7592 review).
        if self.is_degraded() {
            return false;
        }
        // A registration walk pauses delivery for the shared instance, so the
        // flag may stay silent only because its events are still queued. A
        // check must not start during one (#8448).
        if delivery_paused(&self.shared) {
            return false;
        }
        let evidence = self.quiescence_evidence();
        match observe_once(&self.is_writing, false, self.delivery_settling(), || {
            self.quiescence_evidence_is_current(evidence)
        })
        .await
        {
            ObservationAttempt::Quiet => true,
            ObservationAttempt::Active | ObservationAttempt::Unstable => false,
        }
    }

    /// Stop watching. Called when workspace is deleted.
    ///
    /// The watch is released when this handle drops: dropping removes the
    /// handle's route from the shared watcher and, for the last handle on
    /// a path, unwatches the workspace tree (notify removes a recursive
    /// watch's whole subtree together with the root, so any other
    /// registered root overlapping it is re-armed), so re-arms and
    /// unregisters neither leak descriptors nor silence nested workspaces.
    pub fn stop(&self) {}

    /// Get the watched workspace path.
    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    /// Get a clone of the is_writing flag for external quiescence checks.
    pub fn is_writing_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.is_writing)
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        self.shared
            .remove_route(&self.workspace_path, &self.is_writing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn start_valid_dir() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        assert_eq!(watcher.workspace_path(), dir.path());
    }

    #[tokio::test]
    async fn watcher_targets_the_resolved_registration_path() {
        // Registration layout: a symlink pointing at the live directory.
        // std::fs::metadata follows it, so the recorded target identity must
        // be the LIVE directory's (dev, ino) — the inode inotify observes.
        use std::os::unix::fs::MetadataExt;
        let base = tempfile::tempdir().unwrap();
        let live = base.path().join("live");
        std::fs::create_dir(&live).unwrap();
        let link = base.path().join("ws");
        std::os::unix::fs::symlink(&live, &link).unwrap();

        let watcher = WorkspaceWatcher::start(&link).unwrap();
        let live_meta = std::fs::metadata(&live).unwrap();
        assert_eq!(
            watcher.target_identity(),
            (live_meta.dev(), live_meta.ino()),
            "the watch must be pinned to the symlink target's inode"
        );
    }

    #[tokio::test]
    async fn quiescent_when_idle() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        assert!(watcher.check_quiescent().await);
    }

    #[tokio::test]
    async fn not_quiescent_when_writing() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        let flag = watcher.is_writing_flag();
        // Simulate ongoing writes: a background task keeps setting the flag
        let flag_c = flag.clone();
        let handle = tokio::spawn(async move {
            loop {
                flag_c.store(true, Ordering::Release);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        flag.store(true, Ordering::Release);
        assert!(!watcher.check_quiescent().await);
        handle.abort();
    }

    #[tokio::test]
    async fn stop_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        watcher.stop();
        assert_eq!(watcher.workspace_path(), dir.path());
    }

    #[tokio::test]
    async fn each_start_gets_a_strictly_larger_seq() {
        let dir = tempfile::tempdir().unwrap();
        let a = WorkspaceWatcher::start(dir.path()).unwrap();
        let b = WorkspaceWatcher::start(dir.path()).unwrap();
        assert!(b.watch_seq() > a.watch_seq(), "seq must be monotonic");
    }

    #[test]
    fn queue_overflow_latches_every_route_active() {
        // #7592 review: notify 6.1.1 delivers inotify IN_Q_OVERFLOW as
        // Ok(EventKind::Other) with Flag::Rescan — not as Err — so the
        // event-kind wildcard used to discard it as an irrelevant event
        // while the kernel had just dropped events. A shared queue
        // overflow must latch every watched workspace as actively written
        // (fail closed) and count for the health surface.
        use notify::event::Flag;
        let routes = Arc::new(RwLock::new(HashMap::new()));
        let overflows = AtomicU64::new(0);
        let registration = Arc::new(RegistrationState::default());
        let control: Arc<OnceLock<Weak<Mutex<RecommendedWatcher>>>> = Arc::new(OnceLock::new());
        let ws_a = Arc::new(AtomicBool::new(false));
        let ws_b = Arc::new(AtomicBool::new(false));
        routes
            .write()
            .unwrap()
            .insert(PathBuf::from("/ws/a"), vec![Arc::clone(&ws_a)]);
        routes
            .write()
            .unwrap()
            .insert(PathBuf::from("/ws/b"), vec![Arc::clone(&ws_b)]);

        let overflow = Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan));
        forward_event(&routes, &overflows, &registration, &control, overflow);

        assert!(ws_a.load(Ordering::Acquire), "workspace A must be latched");
        assert!(ws_b.load(Ordering::Acquire), "workspace B must be latched");
        assert_eq!(overflows.load(Ordering::Relaxed), 1);

        // Ordinary unrelated events must not count as overflows.
        let other = Ok(Event::new(EventKind::Other));
        forward_event(&routes, &overflows, &registration, &control, other);
        assert_eq!(overflows.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn watcher_cache_retries_transient_init_failures_and_caches_success() {
        // #7592 review: `Inotify::init` can fail with transient pressure
        // (EMFILE/ENFILE/ENOMEM); caching the failure forever — as the
        // previous OnceLock<Result<..>> did — left monitoring dead until a
        // daemon restart even after the pressure cleared.
        let cache = WatcherCache::new();
        let calls = std::cell::Cell::new(0u32);
        let count = || {
            calls.set(calls.get() + 1);
            calls.get()
        };

        let err = match cache.get_or_init_with(|| -> anyhow::Result<SharedWatcher> {
            count();
            Err(anyhow::anyhow!("inotify_init: EMFILE"))
        }) {
            Err(e) => e,
            Ok(_) => panic!("first init must surface its error"),
        };
        assert!(err.to_string().contains("EMFILE"));

        // The failure was NOT cached: the next caller retries and succeeds.
        let ok = cache
            .get_or_init_with(|| -> anyhow::Result<SharedWatcher> {
                count();
                Ok(make_shared_watcher_for_tests())
            })
            .expect("second init must succeed once pressure cleared");

        // And the success IS cached: no further init calls.
        let again = cache
            .get_or_init_with(|| -> anyhow::Result<SharedWatcher> {
                count();
                Ok(make_shared_watcher_for_tests())
            })
            .expect("cached instance must be returned");
        assert!(
            Arc::ptr_eq(&ok, &again),
            "the successful instance must be cached and reused"
        );
        assert_eq!(
            calls.get(),
            2,
            "init must run exactly twice (fail, then succeed)"
        );
    }

    /// A real SharedWatcher for cache tests: a valid notify watcher with no
    /// routes, initialized exactly like production.
    fn make_shared_watcher_for_tests() -> SharedWatcher {
        SharedWatcher::new().expect("inotify must be available in tests")
    }

    #[tokio::test]
    async fn failed_survivor_rearm_degrades_the_route_until_reregistration() {
        // #7592 second review: rename the nested root away, drop the outer
        // handle — the survivor re-arm fails with NotFound while the nested
        // route and handle stay published. The failure must be observable
        // (is_degraded) and must clear only through a successful
        // re-registration, not through the inode moving back: the recorded
        // (dev, ino) identity still matches then, which is exactly why the
        // old identity-only heal never retried.
        let outer = tempfile::tempdir().unwrap();
        let nested = outer.path().join("nested-ws");
        std::fs::create_dir(&nested).unwrap();
        let outer_watcher = WorkspaceWatcher::start(outer.path()).unwrap();
        let nested_watcher = WorkspaceWatcher::start(&nested).unwrap();

        let moved = outer.path().join("moved-ws");
        std::fs::rename(&nested, &moved).unwrap();
        drop(outer_watcher); // survivor re-arm of `nested` fails: path missing
        assert!(
            nested_watcher.is_degraded(),
            "the failed survivor re-arm must mark the route degraded"
        );

        // Moving the SAME inode back does not heal: the identity check
        // would match, but nothing re-armed the kernel watch.
        std::fs::rename(&moved, &nested).unwrap();
        assert!(
            nested_watcher.is_degraded(),
            "an identity-matching inode is not coverage; only re-registration heals"
        );

        // A successful registration on the same root clears the mark —
        // for every handle on that route.
        let healed = WorkspaceWatcher::start(&nested).unwrap();
        assert!(!healed.is_degraded());
        assert!(
            !nested_watcher.is_degraded(),
            "the degraded mark is per route, not per handle"
        );
        drop(healed);
        drop(nested_watcher);
    }

    #[tokio::test]
    async fn quiescence_observed_across_a_coverage_interruption_is_void() {
        // #7592 fourth review (P1): releasing an overlapping handle
        // interrupts the survivor's kernel coverage for the whole
        // recursive re-arm. A quiet-period observation that spans that
        // interruption must not certify quiescence: writes to directories
        // the re-arm has not reached yet (here: the entire tree — the
        // re-arm of the renamed-away root fails) leave no event, so the
        // flag's silence proves nothing.
        let outer = tempfile::tempdir().unwrap();
        let nested = outer.path().join("nested-ws");
        std::fs::create_dir(&nested).unwrap();
        let outer_watcher = WorkspaceWatcher::start(outer.path()).unwrap();
        let nested_watcher = WorkspaceWatcher::start(&nested).unwrap();
        let observed_flag = nested_watcher.is_writing_flag();

        // An active writer keeps the workspace genuinely busy, so the
        // check must run the full quiet-period observation instead of
        // taking the not-writing shortcut.
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let stop = Arc::clone(&stop);
            let path = nested.join("writer.log");
            std::thread::spawn(move || {
                use std::io::Write as _;
                let mut held = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .unwrap();
                while !stop.load(Ordering::Acquire) {
                    let _ = held.write_all(b"data");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })
        };
        // Latch gate: wait until the writer is OBSERVED before starting
        // the check, so the observation is guaranteed to be in progress
        // when the interruption begins.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !observed_flag.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "the nested watcher never observed the writer"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // The check runs concurrently; the interruption happens well
        // inside its first 100 ms quiet-period sleep: rename the nested
        // root away, drop the outer handle (survivor latch + unwatch +
        // failed re-arm of the now-missing path), then move it back. The
        // writer keeps writing the whole time — into a tree nothing
        // watches anymore.
        let check = tokio::spawn(async move { nested_watcher.check_quiescent().await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let moved = outer.path().join("moved-ws");
        std::fs::rename(&nested, &moved).unwrap();
        drop(outer_watcher); // survivor re-arm of `nested` fails: path missing
        std::fs::rename(&moved, &nested).unwrap();

        let quiescent = check.await.unwrap();
        stop.store(true, Ordering::Release);
        writer.join().unwrap();
        assert!(
            !quiescent,
            "a quiet period observed across a coverage interruption proves nothing; the \
             check must report activity so the caller re-observes once coverage is stable"
        );
    }

    #[tokio::test]
    async fn failed_ancestor_rearm_disturbs_routes_outside_the_original_release() {
        // #7592 fourth review (P1): `/p` ⊃ {`/p/a`, `/p/x`}. Dropping
        // `/p/a` disturbs only `/p`; when `/p`'s re-arm then fails, the
        // recursive unwatch that releases its partial registration also
        // strips `/p/x`, which never overlapped `/p/a`. `/p/x` must be
        // handled like any other disturbed route — here its re-arm fails
        // too (the whole tree was renamed away), so it must fail closed
        // instead of keeping a silent flag that reads as quiescence.
        let base = tempfile::tempdir().unwrap();
        let p = base.path().join("p");
        let a = p.join("a");
        let x = p.join("x");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&x).unwrap();
        let p_watcher = WorkspaceWatcher::start(&p).unwrap();
        let a_watcher = WorkspaceWatcher::start(&a).unwrap();
        let x_watcher = WorkspaceWatcher::start(&x).unwrap();

        std::fs::rename(&p, base.path().join("moved")).unwrap();
        drop(a_watcher); // survivor re-arm of `/p` fails: path missing

        assert!(p_watcher.is_degraded());
        assert!(
            x_watcher.is_degraded(),
            "a route stripped by the failed re-arm's release must be marked degraded"
        );
        assert!(
            !x_watcher.check_quiescent().await,
            "a route stripped by the failed re-arm's release must not read as quiescent"
        );
    }

    #[tokio::test]
    async fn retiring_the_last_route_drops_its_coverage_entry() {
        // #7592 fourth review (P2): coverage entries are created by every
        // successful registration and must be retired with the path's last
        // route, or registration churn over distinct paths retains one
        // entry per historical path for the daemon's lifetime.
        let base = tempfile::tempdir().unwrap();
        let shared = shared_watcher().unwrap();
        let paths: Vec<PathBuf> = (0..8)
            .map(|i| base.path().join(format!("ws-{i}")))
            .collect();
        for path in &paths {
            std::fs::create_dir(path).unwrap();
            drop(WorkspaceWatcher::start(path).unwrap());
        }
        {
            let coverage = shared.coverage.read().unwrap();
            for path in &paths {
                assert!(
                    !coverage.contains_key(path),
                    "retired path {path:?} must not keep a coverage entry"
                );
            }
        }

        // A remaining handle on the same path keeps the entry.
        let kept = WorkspaceWatcher::start(&paths[0]).unwrap();
        drop(WorkspaceWatcher::start(&paths[0]).unwrap());
        assert!(shared.coverage.read().unwrap().contains_key(&paths[0]));
        drop(kept);
        assert!(!shared.coverage.read().unwrap().contains_key(&paths[0]));
    }

    #[tokio::test]
    async fn registration_in_flight_is_not_quiescent() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        let _registration = RegistrationGuard::new(&watcher.shared.registration);

        assert!(
            !watcher.check_quiescent().await,
            "an in-flight registration walk pauses event delivery for every workspace; \
             a silent flag proves nothing until it finishes (#8448)"
        );
    }

    #[tokio::test]
    async fn observation_spanning_a_registration_walk_is_void() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        // Drive the full quiet-period path instead of the idle shortcut.
        watcher.is_writing_flag().store(true, Ordering::Release);
        let shared = Arc::clone(&watcher.shared);
        let check = tokio::spawn(async move { watcher.check_quiescent().await });

        // A registration walk that starts and ends inside the observation
        // window: the flag's silence cannot be evidence because events
        // generated during the walk were still queued.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(RegistrationGuard::new(&shared.registration));

        assert!(
            !check.await.unwrap(),
            "a quiet period that spans a registration walk proves nothing and must void \
             the Safe decision (#8448)"
        );
    }

    #[tokio::test]
    async fn idle_check_after_a_registration_does_not_take_the_shortcut() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        let started = Instant::now();

        assert!(watcher.check_quiescent().await);
        assert!(
            started.elapsed() >= Duration::from_millis(190),
            "a check that starts right after a registration must observe the full quiet \
             period instead of trusting a flag that may predate the queued events \
             (took {:?})",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn auto_add_barrier_covers_a_new_directory() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = WorkspaceWatcher::start(dir.path()).unwrap();
        let shared = Arc::clone(&watcher.shared);
        let before = shared.auto_add_seq();

        std::fs::create_dir(dir.path().join("created")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while shared.auto_add_seq() == before || !shared.registration.auto_add_settled() {
            assert!(
                Instant::now() < deadline,
                "notify's auto-add walk was never proven complete by the barrier \
                 (seq={}, covered={})",
                shared.auto_add_seq(),
                shared.registration.auto_add_covered.load(Ordering::Acquire)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
