# Watcher Topology: One Shared Inotify Instance

[中文版](watcher-topology_zh.md)

This document records the watcher topology decision for ws-ckpt: all
workspaces share one process-wide inotify instance instead of one
`notify::recommended_watcher` per workspace. It covers why that topology was
chosen, the inherent costs it carries (a shared kernel event queue that can
overflow, and event delivery pausing for every workspace while any
registration walk runs), how those costs are contained and made observable,
the shared architecture itself, and the invariants future changes must
preserve. It is
aimed at maintainers touching `src/crates/daemon/src/fs_watcher.rs` so the
reasoning does not have to be re-derived from review threads.

Scope note: this is purely the watcher *topology* — how many inotify
instances exist and how events reach workspaces. The quiescence model (clearable
latch vs. last-activity window) is a separate concern handled by #4049.

## Background and problem

Before this change, every workspace watcher armed its own
`notify::recommended_watcher`, and each of those performs one `inotify_init`.
Linux caps inotify instances per user (`fs.inotify.max_user_instances`,
default 128), so a daemon restoring more than ~128 workspaces made the rest
fail with `Too many open files (os error 24)` — and restore only logged one
WARN per workspace and kept going. At the reported 1000-workspace deployment
(#7233), ~873 workspaces silently lost change detection: no counter, no status
field, no aggregate alert; the only discovery path was grepping the journal.

## Single vs. multiple watcher

Three candidate topologies were considered:

| | One shared instance (this design) | One instance per workspace (#7669) | Instance pooling |
|---|---|---|---|
| inotify instances | 1 per process | 1 per workspace | N per process |
| fd cost per workspace | ~0 extra | 3 | 3/N extra + bookkeeping |
| First kernel limit hit at scale | `max_user_watches` (descriptors) | `max_user_instances` (128 default) | `max_user_instances` at N > 128 |
| Event queues | One shared `max_queued_events` queue | One per workspace (independent) | One per pooled instance |
| Failure blast radius | Overflow affects all workspaces | One workspace per instance | One pool slice |

Two measurement anchor points, from the same-machine, same-script comparison
of this PR's head (`39db95fa7`) against #7669 (`771b2977e`) posted in the
#7592 discussion:

- **Locking is not the bottleneck.** The event path takes one read-lock route
  lookup per event plus atomic stores; registration syscalls are serialized by
  a mutex that the event loop never touches. Under a pure write storm the two
  topologies finish within a few milliseconds of each other.
- **The shared queue is the real trade-off.** All workspaces' events share one
  `max_queued_events` kernel queue. When a large directory workspace is rolled
  back and its tree re-watched while other workspaces are writing, the queue
  can overflow and the kernel drops events; in the comparison run roughly half
  of the checkpoint attempts of workspaces unrelated to both the rollback and
  the writes were rejected during the overflow window. #7669 is unaffected by
  that scenario; its cost is 3 fds per workspace and a required
  `LimitNOFILE` raise.

## Why one shared instance

- **Deterministic budget vs. probabilistic loss.** The per-workspace topology
  hits a deterministic per-user cap (128 instances) long before a 1000-workspace
  restore finishes, and the pre-change failure mode was silent. The shared
  instance turns the scarce resource into watch *descriptors*
  (`max_user_watches`), which recursive watches already shared under either
  topology, and its residual risk — queue overflow — is a probabilistic event
  with a fail-closed containment (below) and an observable counter.
- **It is the direction the incident asked for.** #7233's expected behavior
  lists cap-aware lazy start *or a shared instance*; the suggested fixes
  include sharing inotify instances across workspaces.
- **fd arithmetic.** A 1000-workspace restore needs 1 instance here versus
  1000+ instances (cap 128) under per-workspace arming, before any pooling
  complexity is added.

## The inherent costs, and the alerting design

The shared-queue overflow is an inherent property of the topology and is
treated as a first-class failure mode, not logged-and-forgotten:

- **Fail-closed containment.** notify 6.1.1 surfaces `IN_Q_OVERFLOW` as
  `Ok(EventKind::Other)` carrying the rescan flag — it never reaches the
  `Err` arm. `forward_event` detects it, increments the cumulative overflow
  counter, logs an ERROR, and latches **every** route's write-activity flag to
  `writing`. Every watched workspace is therefore treated as actively written
  until quiescence has been re-observed over a fresh quiet period: one noisy
  workspace must not be able to make the others look quiescent while the
  kernel was dropping their events.
- **Observable surface (implemented):**
  - `Request::WatcherHealth` replies with `WatcherHealthOk {
    workspaces_without_watcher, watcher_queue_overflows }`; the fields are
    wire-frozen (bincode encodes positionally).
  - `ws-ckpt status --format json` reports `watcher_queue_overflows`
    (cumulative) and `workspaces_without_watcher` (empty when healthy;
    `serde(default)` keeps the wire tolerant of older daemons).
  - The human status table prints a warning line naming workspaces without a
    watcher and a warning line when the overflow counter is nonzero.
  - Restore aggregates watcher start failures into one ERROR (failed count of
    total restored, the workspace ids, and the `fs.inotify` sysctl hint).
- **Operator guidance.** Any nonzero overflow is alert-worthy: it means events
    were dropped and workspaces were held "active" across the window. Sustained
    overflow growth calls for raising `fs.inotify.max_queued_events`, lowering
    the parallelism of restore/rollback re-watch sweeps, or scheduling the
    affected checkpoints for a re-run once the quiet period re-establishes.
- **Future hardening options (not implemented; recorded so the trade-off space
  stays visible):** an overflow-*rate* threshold that escalates to an ERROR
  status line; a post-overflow re-checkpoint sweep for workspaces whose
  checkpoint was rejected during the window; and, if a deployment shows
  sustained overflow, splitting into a small pool of instances along workspace
  classes. The single-instance topology does not preclude any of these; they
  are additive to the route table.

### The walk-pause cost (silent; tracked as a follow-up issue)

The overflow above is one half of the shared-queue cost. The other half has
no counter: notify 6.1.1 runs `watch()` (a `WalkDir` over the whole tree) and
the auto-add of created or moved-in directories on its single event-loop
thread, and it does not read the inotify fd while a walk is in progress.
With one shared instance that thread serves the whole daemon, so events from
*every* workspace wait in the kernel queue until the walk finishes.

`check_workspace_quiescent` decides from what the forwarder delivered during
its final 100 ms quiet period. When that window falls inside a walk longer
than ~100 ms, the flag keeps the `false` the check itself stored, and the
decision returns a false `Safe` while the workspace is still being written.
The per-route coverage epoch cannot detect this: the checked route's epoch,
degraded mark, and watcher identity are all unchanged, because the walk
belongs to another workspace.

Measured on a release build during the #7592 review: with a writer appending
every 10 ms and an unrelated 1,000-directory / 100,000-file workspace
registering 80 ms into the check (walk of about 170 ms), the check returned
`Safe` 5/5 under this design and 0/5 under the per-workspace topology; with
50 files per directory (walk of about 87 ms) it returned 0/5. Triggers
include the rollback re-arm, large-workspace init, the heal path, watcher
restore at startup, and notify's auto-add when an agent creates or moves in
a large directory (`npm install`, `git clone`, `mv`).

Unlike the overflow, this pause produces no `IN_Q_OVERFLOW`, no counter, and
no latch — it is silent. It follows from the one-instance/one-thread
topology rather than from any specific bug in the registration code, and is
tracked as a follow-up issue rather than a blocker for the shared-instance
change. Candidate directions for that follow-up: a delivery barrier through
the shared queue before returning `Safe`; an instance-wide registration
epoch (partial — it misses notify's internal auto-add); or sharding /
per-workspace instances.

## Shared architecture

- **Handle.** `WorkspaceWatcher` owns only its route identity, its
  write-activity flag, the `(dev, ino)` of the directory the watch was armed
  on (resolved through the registration symlink before arming — inotify
  watches inodes, so a rollback that swaps the live subvolume behind the path
  is detectable), and a monotonic `watch_seq` for observing re-arms.
- **Route table.** `routes: RwLock<HashMap<PathBuf, Vec<Arc<AtomicBool>>>>` —
  a path maps to the flags of the live handles on it (normally one; a re-arm
  briefly keeps two, which is why the value is a `Vec`). Events are dispatched
  by walking the event path's ancestors, so a write inside an overlapping root
  flags every containing workspace, matching what per-workspace instances
  observed.
- **Lock discipline.** The `watcher` mutex serializes registration mutations
  and is always acquired before `routes`; `routes` is never write-locked across
  a `watch()`/`unwatch()` call (the notify event loop takes `routes.read()`
  inside delivery — holding the write side across the call would deadlock the
  loop against ourselves). The coverage map is a leaf lock. The event path
  itself takes no lock beyond the route read.
- **Publication order.** `add_route` publishes the route *before* arming the
  kernel watch: descriptors covering the path do not exist before `watch()`,
  so every event that can possibly be delivered already finds its route. A
  failed registration rolls the publication back and releases the partial
  descriptors the recursive walk installed (notify 6.1.1 does not roll back on
  its own).
- **Overlap-preserving release.** notify 6.1.1 keys one descriptor per path
  and a recursive `unwatch(root)` removes *every* descriptor whose path starts
  with `root` — including descriptors overlapping survivors rely on. Before
  the first descriptor goes away, `unwatch_preserving_overlaps` latches every
  overlapping survivor to "writing" and marks its route uncovered; then
  unwatches; then re-arms each survivor. A failed re-arm keeps the degraded
  mark (fail closed) and releases its own partial registration with a
  bounded failure cascade.
- **Quiescence.** `check_quiescent` fails closed while the route is degraded,
  and snapshots the coverage epoch before its quiet period: an interruption
  that both started and ended inside the observation window leaves the route
  healthy again with a silent flag that proves nothing, so the epoch moving
  voids the `Safe` decision.
- **Initialization cache.** `WatcherCache` caches only successful
  initialization; transient `inotify_init` failures (`EMFILE`/`ENFILE`/
  `ENOMEM`) are retried by the next registration instead of leaving a
  permanently dead watcher until daemon restart.

## Consistency guidance

Invariants future changes must preserve:

1. All workspace watchers register through the `SharedWatcher`; nothing may
   construct a `notify` watcher for workspace paths directly.
2. Latch semantics: writing = `true` on Create/Modify/Remove, `false` only on
   `Close(Write)`.
3. Fail-closed direction: any doubt about coverage or dropped events resolves
   toward "actively written"; silence must never read as quiescence.
4. Route publication precedes kernel arming; the last-handle unpublish is
   atomic with the flag detach.
5. Any unwatch must preserve overlapping survivors (latch, mark, re-arm).
6. `watch_seq` is monotonic per `start`.
7. `WatcherHealthOk` fields are wire-frozen; changes are additive only.
8. The `watcher_shared_instance` suite runs in its own test binary on purpose
   (instance counting via `/proc/self/fd` blurs when the lib tests share the
   process); keep it isolated.

## References

- #7233 — original incident (silent watcher loss at scale).
- #7669 — per-workspace instances + alert + hard-reject alternative.
- #4049 — quiescence model (orthogonal to topology).
- #7592 discussion — same-machine comparison data and the review rounds that
  shaped the overlap and epoch handling.
