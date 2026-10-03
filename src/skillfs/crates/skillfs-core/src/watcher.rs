use std::collections::HashSet;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Cap on the retained directory-type memory. The set only ever holds
/// immediate children of the source root, so this bound exists purely to
/// defend against a pathological source; the seed takes the first
/// `MAX_TRACKED_DIRS` entries and drops the rest (their move-outs then
/// stay silent — the pre-watcher behavior, never a false positive).
const MAX_TRACKED_DIRS: usize = 10_000;

// ---------------------------------------------------------------------------
// SkillEvent
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum SkillEvent {
    /// New SKILL.md detected
    Created(PathBuf),
    /// Existing SKILL.md changed
    Modified(PathBuf),
    /// SKILL.md removed
    Deleted(PathBuf),
    /// New skill directory created
    DirCreated(PathBuf),
    /// Skill directory removed
    DirDeleted(PathBuf),
}

// ---------------------------------------------------------------------------
// WatchError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("notify error: {0}")]
    NotifyError(#[from] notify::Error),
    #[error("path not found: {0}")]
    PathNotFound(PathBuf),
    /// The background watcher task closed its readiness channel before
    /// signaling success or failure (e.g. it panicked, or the runtime
    /// dropped it). Treat as a watcher startup failure rather than a
    /// silent half-running watcher.
    #[error("watcher readiness signal closed before watcher attached")]
    ReadyChannelClosed,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Explicit shutdown handle for a running source watcher.
///
/// Long-lived embedders that repeatedly mount and unmount SkillFS need a
/// way to stop the underlying `notify` watcher and await its task
/// completion deterministically, instead of dropping the receiver without
/// awaiting task completion. [`WatcherHandle`] is
/// that surface.
///
/// Acquired through [`watch_source_with_handle`]. The companion
/// [`watch_source`] entry point is unchanged for callers that do not need
/// explicit shutdown — the watcher event loop still exits when its
/// outbound receiver is dropped, without waiting for a filesystem event.
///
/// Calling [`WatcherHandle::shutdown`] signals the watcher event loop to
/// exit and waits until the spawned task has finished. After
/// `shutdown().await` returns, the underlying `notify` watcher has been
/// dropped and no further [`SkillEvent`]s will be emitted. The handle is
/// consumed by `shutdown` so misuse (double-shutdown) is impossible.
///
/// Dropping the handle without calling `shutdown` is best-effort: the
/// shutdown signal is sent and the task is aborted, but the caller does
/// not get to await completion. Prefer the explicit path when timing
/// matters (CLI signal handlers, embedder teardown, tests).
#[derive(Debug)]
pub struct WatcherHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl WatcherHandle {
    /// Signal the watcher event loop to exit and await task completion.
    ///
    /// Returns once the spawned task has finished. Errors from the
    /// shutdown signal channel and the join handle are absorbed: the
    /// task may already have exited (receiver dropped, send failure)
    /// before the explicit signal landed, in which case the channel send
    /// returns `Err` and the join still yields the final task result.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            // Receiver may have been consumed by the select arm or the
            // task may have already exited via receiver-drop. Either
            // way, ignore the error; the join below is the source of
            // truth for "watcher fully torn down".
            let _ = tx.send(());
        }
        if let Some(h) = self.join.take() {
            let _ = h.await;
        }
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        // Best-effort cleanup when the caller forgets to call
        // `shutdown().await`. Signal the loop and abort the task; we
        // cannot await here without blocking the executor thread.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.join.take() {
            h.abort();
        }
    }
}

/// Start watching a source directory for SKILL.md changes.
///
/// Returns a channel receiver for skill events.
///
/// **Readiness contract.** The future returned by `watch_source` only
/// resolves to `Ok(rx)` after the underlying `notify` watcher has been
/// constructed *and* has successfully attached to `source` recursively. If
/// either step fails the future resolves to `Err` synchronously and no
/// background watcher is left running. This lets callers (e.g. the W1
/// drift runtime in `skillfs-fuse`) treat watcher startup as a regular
/// fallible operation: when `watch_source().await` returns `Ok`, the
/// receiver is connected to a live watcher and any subsequent filesystem
/// activity has a chance to be observed; when it returns `Err`, no
/// observer is running and the caller can decide whether to surface the
/// failure or fall back. The watcher itself keeps running on a separate
/// tokio task until the receiver is dropped.
///
/// **Implicit cleanup.** This entry point exposes only the receiver, so
/// callers cannot signal shutdown explicitly. Receiver closure wakes the
/// watcher even when the source is quiet or debounce is long. Long-lived
/// embedders that need to await shutdown deterministically should use
/// [`watch_source_with_handle`] instead.
pub async fn watch_source(
    source: PathBuf,
    debounce_ms: u64,
) -> Result<mpsc::UnboundedReceiver<SkillEvent>, WatchError> {
    let (rx, _join) = start_watcher(source, debounce_ms, None).await?;
    Ok(rx)
}

/// Variant of [`watch_source`] that returns an explicit [`WatcherHandle`]
/// alongside the receiver.
///
/// The receiver behaves identically to [`watch_source`]'s output; the
/// readiness contract is unchanged. The additional [`WatcherHandle`]
/// lets callers signal shutdown and await task completion deterministically
/// instead of relying on receiver-drop to tear the watcher
/// down. This is the entry point the W1 drift runtime adapter consumes
/// so [`crate::watcher::WatcherHandle::shutdown`] can be threaded through
/// to long-lived embedders. The implicit, receiver-drop-driven cleanup
/// continues to work — the new shutdown signal is just an additional way
/// to exit early.
pub async fn watch_source_with_handle(
    source: PathBuf,
    debounce_ms: u64,
) -> Result<(mpsc::UnboundedReceiver<SkillEvent>, WatcherHandle), WatchError> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (rx, join) = start_watcher(source, debounce_ms, Some(shutdown_rx)).await?;
    Ok((
        rx,
        WatcherHandle {
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        },
    ))
}

async fn start_watcher(
    source: PathBuf,
    debounce_ms: u64,
    shutdown_rx: Option<oneshot::Receiver<()>>,
) -> Result<(mpsc::UnboundedReceiver<SkillEvent>, JoinHandle<()>), WatchError> {
    if !source.exists() {
        return Err(WatchError::PathNotFound(source));
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), WatchError>>();

    // Spawn the watcher task. The task signals on `ready_tx` once
    // `RecommendedWatcher::new` and `watcher.watch(...)` both succeed
    // (or surfaces the underlying error if either fails). We do NOT
    // return `Ok(rx)` until that signal has arrived so the caller
    // cannot race the watcher's attach phase.
    let join = tokio::task::spawn(async move {
        run_watcher(source, debounce_ms, tx, ready_tx, shutdown_rx).await;
    });

    match ready_rx.await {
        Ok(Ok(())) => Ok((rx, join)),
        Ok(Err(e)) => {
            // Watcher task surfaced an init failure and is exiting; wait
            // for it so no orphan task is left running.
            let _ = join.await;
            Err(e)
        }
        Err(_canceled) => {
            let _ = join.await;
            Err(WatchError::ReadyChannelClosed)
        }
    }
}

async fn run_watcher(
    source: PathBuf,
    debounce_ms: u64,
    tx: mpsc::UnboundedSender<SkillEvent>,
    ready_tx: oneshot::Sender<Result<(), WatchError>>,
    shutdown_rx: Option<oneshot::Receiver<()>>,
) {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::collections::HashMap;
    use std::time::Instant;

    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();

    // Construct the watcher. Surface any notify-side error through the
    // readiness channel and exit before starting the event loop.
    let mut watcher = match RecommendedWatcher::new(
        move |result: Result<notify::Event, notify::Error>| {
            if let Ok(event) = result {
                let _ = notify_tx.send(event);
            }
        },
        Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            let _ = ready_tx.send(Err(WatchError::NotifyError(e)));
            return;
        }
    };

    // Attach the watcher recursively to the source tree. This is the
    // operation that actually decides whether subsequent filesystem
    // events can be observed; failing it must not appear as a "silent
    // half-running" watcher to the caller.
    if let Err(e) = watcher.watch(&source, RecursiveMode::Recursive) {
        let _ = ready_tx.send(Err(WatchError::NotifyError(e)));
        return;
    }

    // Watcher is attached: signal readiness so the caller's
    // `watch_source().await` can resolve to `Ok(rx)`. Any later loss of
    // the receiver is treated as a normal "consumer dropped" exit, not a
    // startup failure.
    let _ = ready_tx.send(Ok(()));

    // Debounce state: path -> (last_event_time, last_event_kind)
    let debounce = std::time::Duration::from_millis(debounce_ms);
    let mut pending: HashMap<PathBuf, (std::time::Instant, notify::EventKind)> = HashMap::new();

    // Type memory for the move-out arm: immediate children of the source
    // that we know to be directories. Seeded once from the source root so a
    // move-out of a directory that predates this watcher is still
    // attributable, and kept current by restats below. A path absent from
    // this set is either a regular file or unknown — in both cases its
    // `RenameMode::From` must NOT be reported as a directory deletion.
    let mut known_dirs = seed_known_dirs(&source);

    // Shutdown signal future. When `shutdown_rx` is `Some`, the loop
    // exits as soon as the corresponding `WatcherHandle::shutdown` (or
    // its Drop fallback) sends on the channel. When it is `None`
    // (callers that went through the original `watch_source` API) the
    // future is `pending` forever; receiver closure still independently
    // exits the loop without waiting for a filesystem event.
    let shutdown_fut = async move {
        match shutdown_rx {
            Some(rx) => {
                let _ = rx.await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(shutdown_fut);

    loop {
        tokio::select! {
            _ = tx.closed() => return,
            _ = &mut shutdown_fut => {
                // Explicit shutdown requested. Drop the notify watcher
                // by returning so any in-flight events stop being
                // observed; the `tx` channel closes when this task
                // ends, signalling the consumer side.
                return;
            }
            Some(event) = notify_rx.recv() => {
                debounce_insert(&mut pending, &event, &mut known_dirs);
            }
            _ = tokio::time::sleep(debounce) => {
                let now = Instant::now();
                let ready: Vec<(PathBuf, notify::EventKind)> = pending
                    .iter()
                    .filter(|(_, (time, _))| now.duration_since(*time) >= debounce)
                    .map(|(path, (_, kind))| (path.clone(), *kind))
                    .collect();

                for (path, kind) in ready {
                    pending.remove(&path);
                    // Retain the observed type while the path still exists:
                    // a stat failure (already moved away or removed) simply
                    // records nothing, so stale entries are never created.
                    if path.parent().map(|p| p == source).unwrap_or(false)
                        && path.is_dir()
                    {
                        known_dirs.insert(path.clone());
                    }
                    if let Some(event) = classify_event(&source, &path, kind, &known_dirs) {
                        if let SkillEvent::DirDeleted(ref gone) = event {
                            // The entry no longer names a directory; stop
                            // treating a future same-named path as one.
                            known_dirs.remove(gone);
                        }
                        if tx.send(event).is_err() {
                            return; // receiver dropped
                        }
                    }
                }
            }
        }
    }
}

/// One-time type seed: the immediate-child directories of the source root.
///
/// A moved-away path cannot be restated, so the move-out arm below depends
/// on this retained type. Seeding at watcher start covers directories that
/// predate the watcher; regular files are deliberately absent (their move
/// must stay silent, however skill-shaped their name).
fn seed_known_dirs(source: &Path) -> HashSet<PathBuf> {
    std::fs::read_dir(source)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
                .map(|entry| entry.path())
                .take(MAX_TRACKED_DIRS)
                .collect()
        })
        .unwrap_or_default()
}

/// Record one notify event into the debounce map.
///
/// `Modify(Name(RenameMode::Both))` carries both sides of a rename (old
/// first, new second) in a single event. Split it so each pending path
/// keeps a side-specific kind: the old path is recorded as `From`, the new
/// path as `To`. Without the split, per-path classification cannot tell
/// which side of the rename a path is, and inotify moves — the ONLY way
/// `mv` on a skill directory is ever reported — were classified as `None`.
///
/// For a paired rename the new side already exists when the event arrives,
/// so its type is authoritative for BOTH sides: a directory rename records
/// the old and the new path, a regular-file rename records neither (its
/// `From` side must stay silent). This correlation also makes the outcome
/// independent of the (arbitrary) order in which the debounce map flushes
/// the two sides.
fn debounce_insert(
    pending: &mut std::collections::HashMap<PathBuf, (std::time::Instant, notify::EventKind)>,
    event: &notify::Event,
    known_dirs: &mut HashSet<PathBuf>,
) {
    let both = matches!(
        event.kind,
        notify::EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both
        ))
    ) && event.paths.len() == 2;
    if both && event.paths[1].is_dir() {
        known_dirs.insert(event.paths[0].clone());
        known_dirs.insert(event.paths[1].clone());
    }
    for (index, path) in event.paths.iter().enumerate() {
        let kind = if both {
            if index == 0 {
                notify::EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::From,
                ))
            } else {
                notify::EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::To,
                ))
            }
        } else {
            event.kind
        };
        pending.insert(path.clone(), (std::time::Instant::now(), kind));
    }
}

/// Classify a filesystem event into a SkillEvent, filtering irrelevant files.
///
/// **Coverage.** This intentionally limits emission to two narrow shapes:
///
/// * `<source>/…/SKILL.md` — manifest create/modify/remove at **any depth
///   under the source** (used by the skill manifest tracking pipeline),
///   except under `.skill-meta` (see below). The store loads both the
///   flat (`<source>/<skill>/SKILL.md`) and the categorized
///   (`<source>/<category>/<skill>/SKILL.md`) layout first-class, so
///   manifest events from either layout must be surfaced; downstream
///   `DriftEvent::classify` routes deep manifests to
///   `InsideSourceOutsideSkill`, so consuming them is safe. A `SKILL.md`
///   directly at the source root is not a manifest in any loaded layout
///   and stays unclassified.
/// * `<source>/<skill>` — immediate skill-directory create/remove, including
///   inotify move events: `RenameMode::To` (move-in / rename target) restats
///   the path like `Create`; `RenameMode::From` (move-out / rename source)
///   is emitted only for paths whose directory-ness the watcher has
///   retained — from the startup seed of the source root, from a restat of
///   an earlier event, or from the correlated new side of a paired rename —
///   because the moved-away path itself can no longer be restated, and a
///   skill-shaped regular *file* moved out must never be reported as a
///   skill-directory deletion.
///
/// Arbitrary non-manifest files inside a skill (`scripts/run.sh`,
/// `notes.txt`, `.skill-meta/manifest.json`) are **not** surfaced, and
/// neither is anything under a `.skill-meta` directory: version snapshots
/// and other store-internal state live there (e.g.
/// `<source>/<skill>/.skill-meta/versions/<v>.snapshot/SKILL.md`), and
/// observing the store's own writes would only emit drift noise about
/// its internals. The W1 drift runtime in `skillfs-fuse` therefore
/// observes manifest- and skill-directory-level drift, mirroring this
/// helper's scope.
fn classify_event(
    source: &Path,
    path: &Path,
    kind: notify::EventKind,
    known_dirs: &HashSet<PathBuf>,
) -> Option<SkillEvent> {
    use notify::EventKind;

    let is_skill_md = path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md");

    // `.skill-meta` at any depth of the path relative to the source marks
    // store-internal snapshot state (both the flat
    // `<source>/<skill>/.skill-meta/…` and the categorized
    // `<source>/<category>/<skill>/.skill-meta/…` layout), never a
    // user-edited manifest.
    let inside_skill_meta = path
        .strip_prefix(source)
        .map(|rel| {
            rel.components()
                .any(|c| c.as_os_str().to_str() == Some(".skill-meta"))
        })
        .unwrap_or(false);

    // Manifest scope: any SKILL.md below the source root (depth >= 2 in
    // both the flat and the categorized layout) except store-internal
    // `.skill-meta` snapshots. `starts_with` is a component-wise prefix,
    // so sibling roots like `<source>-other` do not match.
    let is_manifest_under_source = is_skill_md
        && !inside_skill_meta
        && path.parent().map(|p| p != source).unwrap_or(false)
        && path.starts_with(source);

    let is_immediate_child = path.parent().map(|p| p == source).unwrap_or(false);

    if is_manifest_under_source {
        match kind {
            EventKind::Create(_) => Some(SkillEvent::Created(path.to_path_buf())),
            EventKind::Modify(_) => Some(SkillEvent::Modified(path.to_path_buf())),
            EventKind::Remove(_) => Some(SkillEvent::Deleted(path.to_path_buf())),
            _ => None,
        }
    } else if is_immediate_child {
        use notify::event::{ModifyKind, RenameMode};
        match kind {
            EventKind::Create(_) if path.is_dir() => {
                Some(SkillEvent::DirCreated(path.to_path_buf()))
            }
            // Removed paths no longer exist. Use the event's original object
            // kind instead of inspecting the filesystem after deletion.
            EventKind::Remove(notify::event::RemoveKind::Folder) => {
                Some(SkillEvent::DirDeleted(path.to_path_buf()))
            }
            // MOVED_TO: the new name of a rename inside the source, or a
            // move-in from outside. inotify does not tag the event with
            // the object type, so restat (the Create arm already does).
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) if path.is_dir() => {
                Some(SkillEvent::DirCreated(path.to_path_buf()))
            }
            // MOVED_FROM: the old name of a rename, or a move-out. The path
            // no longer exists, so its type must come from the retained
            // memory (`known_dirs`, seeded at startup and refreshed by
            // restats and paired-rename correlation). A path that is a
            // known directory reports its departure; anything else — a
            // regular file with a skill-shaped name included — stays
            // silent, so a file move can never masquerade as a
            // skill-directory deletion downstream.
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) if known_dirs.contains(path) => {
                Some(SkillEvent::DirDeleted(path.to_path_buf()))
            }
            _ => None,
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_in_directory_to_side_reports_dir_created() {
        let source = tempfile::tempdir().expect("source directory");
        let new_name = source.path().join("beta");
        std::fs::create_dir(&new_name).expect("moved-in directory");

        let event = classify_event(
            source.path(),
            &new_name,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::To,
            )),
            &HashSet::new(),
        );
        assert!(matches!(event, Some(SkillEvent::DirCreated(path)) if path == new_name));
    }

    #[test]
    fn moved_in_file_to_side_is_not_dir_created() {
        let source = tempfile::tempdir().expect("source directory");
        let file = source.path().join("notes.txt");
        std::fs::write(&file, "x").expect("moved-in file");

        let event = classify_event(
            source.path(),
            &file,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::To,
            )),
            &HashSet::new(),
        );
        assert!(
            event.is_none(),
            "a moved-in regular file is not a DirCreated"
        );
    }

    #[test]
    fn moved_out_tracked_directory_reports_dir_deleted_without_restat() {
        let source = tempfile::tempdir().expect("source directory");
        let old_name = source.path().join("alpha");
        // The directory was observed while the watcher ran (startup seed
        // or an earlier restat); a move-out then leaves the path absent,
        // so the From side cannot restat it and consults the type memory.
        let known_dirs: HashSet<PathBuf> = HashSet::from([old_name.clone()]);

        let event = classify_event(
            source.path(),
            &old_name,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From,
            )),
            &known_dirs,
        );
        assert!(matches!(event, Some(SkillEvent::DirDeleted(path)) if path == old_name));
    }

    #[test]
    fn moved_out_skill_shaped_file_is_silent() {
        // Review regression: a regular file with a valid skill name moved
        // out of the source must never surface as a skill-directory
        // deletion — the drift conversion maps DirDeleted straight into a
        // skill-scoped deletion event. The seed records directories only,
        // so the file's From side classifies to None.
        let source = tempfile::tempdir().expect("source directory");
        let file = source.path().join("scratch");
        std::fs::write(&file, "a skill-shaped regular file").expect("file");
        let known_dirs = seed_known_dirs(source.path());
        assert!(
            !known_dirs.contains(&file),
            "the seed records immediate-child directories only"
        );

        let event = classify_event(
            source.path(),
            &file,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From,
            )),
            &known_dirs,
        );
        assert!(event.is_none(), "a moved-out file is not a DirDeleted");
    }

    #[test]
    fn seed_records_immediate_child_directories_only() {
        let source = tempfile::tempdir().expect("source directory");
        std::fs::create_dir(source.path().join("alpha")).expect("directory");
        std::fs::write(source.path().join("scratch"), "file").expect("file");
        std::fs::write(source.path().join("SKILL.md"), "root manifest").expect("file");

        let known = seed_known_dirs(source.path());
        assert!(known.contains(&source.path().join("alpha")));
        assert!(!known.contains(&source.path().join("scratch")));
        assert!(!known.contains(&source.path().join("SKILL.md")));
    }

    #[test]
    fn paired_directory_rename_correlates_the_type_to_both_sides() {
        let source = tempfile::tempdir().expect("source directory");
        let old = source.path().join("alpha");
        let new = source.path().join("beta");
        // The rename has completed: the new side exists and is the
        // authoritative type witness for both sides of the pair.
        std::fs::create_dir(&new).expect("new side");

        let mut pending = std::collections::HashMap::new();
        let mut known_dirs = HashSet::new();
        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both,
            )),
            paths: vec![old.clone(), new.clone()],
            attrs: Default::default(),
        };
        debounce_insert(&mut pending, &event, &mut known_dirs);
        assert!(known_dirs.contains(&old), "the From side inherits the type");
        assert!(known_dirs.contains(&new));

        let classified = classify_event(
            source.path(),
            &old,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From,
            )),
            &known_dirs,
        );
        assert!(matches!(classified, Some(SkillEvent::DirDeleted(p)) if p == old));
    }

    #[test]
    fn paired_regular_file_rename_records_no_type() {
        // A regular file renamed within the source records neither side,
        // so its From side stays silent even for a skill-shaped name.
        let source = tempfile::tempdir().expect("source directory");
        let old = source.path().join("scratch");
        let new = source.path().join("scratch2");
        std::fs::write(&new, "a file").expect("new side");

        let mut pending = std::collections::HashMap::new();
        let mut known_dirs = HashSet::new();
        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both,
            )),
            paths: vec![old.clone(), new.clone()],
            attrs: Default::default(),
        };
        debounce_insert(&mut pending, &event, &mut known_dirs);
        assert!(
            known_dirs.is_empty(),
            "a file rename records no directory type"
        );

        let classified = classify_event(
            source.path(),
            &old,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From,
            )),
            &known_dirs,
        );
        assert!(
            classified.is_none(),
            "a renamed file's old name stays silent"
        );
    }

    #[test]
    fn both_events_are_split_per_side_by_the_debounce_map() {
        let mut pending = std::collections::HashMap::new();
        let old = PathBuf::from("/source/alpha");
        let new = PathBuf::from("/source/beta");
        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both,
            )),
            paths: vec![old.clone(), new.clone()],
            attrs: Default::default(),
        };
        debounce_insert(&mut pending, &event, &mut HashSet::new());
        assert!(matches!(
            pending[&old],
            (
                _,
                notify::EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::From
                ))
            )
        ));
        assert!(matches!(
            pending[&new],
            (
                _,
                notify::EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::To
                ))
            )
        ));

        // Single-path events keep their kind verbatim.
        pending.clear();
        let file = PathBuf::from("/source/alpha/SKILL.md");
        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![file.clone()],
            attrs: Default::default(),
        };
        debounce_insert(&mut pending, &event, &mut HashSet::new());
        assert!(matches!(
            pending[&file],
            (_, notify::EventKind::Modify(notify::event::ModifyKind::Any))
        ));
    }

    #[test]
    fn skill_md_move_from_side_still_reports_modified() {
        let source = tempfile::tempdir().expect("source directory");
        let manifest = source.path().join("alpha/SKILL.md");
        std::fs::create_dir_all(manifest.parent().expect("skill dir")).expect("skill directory");
        std::fs::write(&manifest, "x").expect("manifest");

        // The SKILL.md arm maps any Modify to Modified — the rename sub-kind
        // included — so a moved manifest still reports Modified for the old
        // path (unchanged behavior, pinned).
        let event = classify_event(
            source.path(),
            &manifest,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From,
            )),
            &HashSet::new(),
        );
        assert!(matches!(event, Some(SkillEvent::Modified(path)) if path == manifest));
    }

    #[test]
    fn file_rename_from_side_stays_silent() {
        // A top-level regular file renamed within the source: its From
        // side cannot be restated (the path is gone) and inotify does not
        // tag the object type, so the untracked path must stay silent.
        // This is the pre-move-coverage behavior for files, retained on
        // purpose (see moved_out_skill_shaped_file_is_silent).
        let source = tempfile::tempdir().expect("source directory");
        let old_name = source.path().join("scratch");

        let event = classify_event(
            source.path(),
            &old_name,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From,
            )),
            &HashSet::new(),
        );
        assert!(
            event.is_none(),
            "an untracked From path is not a DirDeleted"
        );
    }

    #[test]
    fn removed_immediate_directory_is_classified_without_restat() {
        let source = tempfile::tempdir().expect("source directory");
        let child = source.path().join("alpha");
        std::fs::create_dir(&child).expect("skill directory");
        std::fs::remove_dir(&child).expect("remove skill directory");

        let event = classify_event(
            source.path(),
            &child,
            notify::EventKind::Remove(notify::event::RemoveKind::Folder),
            &HashSet::new(),
        );
        assert!(matches!(event, Some(SkillEvent::DirDeleted(path)) if path == child));
    }

    #[test]
    fn categorized_layout_manifest_events_are_classified() {
        // The store loads `<source>/<category>/<skill>/SKILL.md` first
        // class; a manifest event at that depth must be surfaced so the
        // drift pipeline can observe it. Downstream
        // `DriftEvent::classify` routes deep manifests to
        // `InsideSourceOutsideSkill`, so emitting them is safe.
        let source = tempfile::tempdir().expect("source directory");
        let skill_md = source.path().join("tools").join("alpha").join("SKILL.md");
        std::fs::create_dir_all(skill_md.parent().expect("skill dir")).expect("category dirs");
        std::fs::write(&skill_md, "---\nname: alpha\n---\n").expect("manifest");

        let event = classify_event(
            source.path(),
            &skill_md,
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            &HashSet::new(),
        );
        assert!(
            matches!(event, Some(SkillEvent::Modified(ref path)) if path == &skill_md),
            "categorized-layout manifest edit must classify, got {event:?}"
        );

        // Non-manifest files at the same depth stay unsurfaced.
        let other = source.path().join("tools").join("alpha").join("notes.txt");
        std::fs::write(&other, "notes").expect("non-manifest file");
        assert!(
            classify_event(
                source.path(),
                &other,
                notify::EventKind::Modify(notify::event::ModifyKind::Any),
                &HashSet::new()
            )
            .is_none(),
            "non-manifest files must stay outside the manifest scope"
        );
    }

    #[test]
    fn skill_meta_snapshot_manifests_are_not_classified() {
        // `.skill-meta` directories hold store-internal snapshot state
        // (e.g. `<source>/<skill>/.skill-meta/versions/<v>.snapshot/
        // SKILL.md` in the flat layout, one level deeper in the
        // categorized layout). Surfacing them would emit drift noise
        // about the store's own writes, so they must classify to None —
        // the operator-facing `.skill-meta/**` non-observation contract.
        let source = tempfile::tempdir().expect("source directory");
        let snapshot_md = source
            .path()
            .join("alpha")
            .join(".skill-meta")
            .join("versions")
            .join("v1.snapshot")
            .join("SKILL.md");
        std::fs::create_dir_all(snapshot_md.parent().expect("snapshot dir")).expect("meta dirs");
        std::fs::write(&snapshot_md, "---\nname: alpha\n---\n").expect("snapshot manifest");

        for kind in [
            notify::EventKind::Create(notify::event::CreateKind::Any),
            notify::EventKind::Modify(notify::event::ModifyKind::Any),
            notify::EventKind::Remove(notify::event::RemoveKind::Any),
        ] {
            assert!(
                classify_event(source.path(), &snapshot_md, kind, &HashSet::new()).is_none(),
                "store-internal .skill-meta snapshot SKILL.md must stay unobserved ({kind:?})"
            );
        }

        // Same shape one level deeper (categorized layout) stays excluded.
        let categorized_snapshot = source
            .path()
            .join("tools")
            .join("alpha")
            .join(".skill-meta")
            .join("versions")
            .join("v1.snapshot")
            .join("SKILL.md");
        std::fs::create_dir_all(categorized_snapshot.parent().expect("snapshot dir"))
            .expect("meta dirs");
        std::fs::write(&categorized_snapshot, "---\nname: alpha\n---\n").expect("snapshot");
        assert!(
            classify_event(
                source.path(),
                &categorized_snapshot,
                notify::EventKind::Modify(notify::event::ModifyKind::Any),
                &HashSet::new()
            )
            .is_none(),
            "categorized-layout .skill-meta snapshots must stay unobserved"
        );

        // A real user manifest at the same depths keeps classifying.
        let user_md = source.path().join("tools").join("beta").join("SKILL.md");
        std::fs::create_dir_all(user_md.parent().expect("skill dir")).expect("skill dir");
        std::fs::write(&user_md, "---\nname: beta\n---\n").expect("manifest");
        assert!(
            matches!(
                classify_event(
                    source.path(),
                    &user_md,
                    notify::EventKind::Modify(notify::event::ModifyKind::Any),
                    &HashSet::new()
                ),
                Some(SkillEvent::Modified(_))
            ),
            "user manifests outside .skill-meta must keep classifying"
        );
    }

    #[test]
    fn removed_files_and_unknown_objects_are_not_directory_events() {
        let source = tempfile::tempdir().expect("source directory");
        let file = source.path().join("README.md");
        std::fs::write(&file, "readme").expect("top-level file");
        std::fs::remove_file(&file).expect("remove top-level file");

        for kind in [
            notify::event::RemoveKind::File,
            notify::event::RemoveKind::Any,
            notify::event::RemoveKind::Other,
        ] {
            assert!(
                classify_event(
                    source.path(),
                    &file,
                    notify::EventKind::Remove(kind),
                    &HashSet::new()
                )
                .is_none(),
                "{kind:?} must not be attributed as a removed directory"
            );
        }
    }

    #[test]
    fn directory_removal_outside_immediate_children_is_ignored() {
        let source = tempfile::tempdir().expect("source directory");
        let nested = source.path().join("alpha/scripts");
        std::fs::create_dir_all(&nested).expect("nested directory");
        std::fs::remove_dir(&nested).expect("remove nested directory");

        for path in [nested.as_path(), source.path(), Path::new("/outside/alpha")] {
            assert!(
                classify_event(
                    source.path(),
                    path,
                    notify::EventKind::Remove(notify::event::RemoveKind::Folder),
                    &HashSet::new()
                )
                .is_none(),
                "directory deletion must stay within the immediate-child scope"
            );
        }
    }

    /// Missing source paths must surface as `PathNotFound` synchronously,
    /// before any background watcher task is spawned. Predates W1 but pinned
    /// here to lock in the watcher's startup-error contract that the W1
    /// drift runtime now depends on.
    #[tokio::test]
    async fn missing_source_returns_path_not_found_synchronously() {
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs-watcher-readiness");
        let err = watch_source(bogus.clone(), 50)
            .await
            .expect_err("missing source must error");
        match err {
            WatchError::PathNotFound(p) => assert_eq!(p, bogus),
            other => panic!("expected PathNotFound, got {other:?}"),
        }
    }

    /// Real source directories must succeed. The future only resolves
    /// after the underlying notify watcher has actually attached, so a
    /// successful return implies a live receiver. We do not exercise
    /// real filesystem events here (those tests live in
    /// `crates/skillfs-core/tests/watcher_tests.rs` and remain
    /// `#[ignore]`-marked for CI flakiness reasons); the readiness
    /// contract is structural.
    #[tokio::test]
    async fn existing_source_dir_returns_ok_after_watcher_attaches() {
        let dir = tempfile::tempdir().expect("temp source dir");
        let rx = watch_source(dir.path().to_path_buf(), 50)
            .await
            .expect("real source dir must produce a ready watcher");
        // Receiver must be live and unattached drops cleanly.
        drop(rx);
    }

    /// Same readiness contract for the explicit-handle variant: real
    /// source directories must succeed only after the underlying notify
    /// watcher has attached, and the returned handle must be live.
    #[tokio::test]
    async fn watch_source_with_handle_readiness_contract_unchanged() {
        let dir = tempfile::tempdir().expect("temp source dir");
        let (rx, handle) = watch_source_with_handle(dir.path().to_path_buf(), 50)
            .await
            .expect("real source dir must produce a ready watcher with handle");
        drop(rx);
        // Drop path must be safe even when shutdown is not awaited.
        drop(handle);
    }

    /// Missing-source paths must surface synchronously through the
    /// handle entry point too — no orphan task is spawned, no shutdown
    /// signal is left dangling.
    #[tokio::test]
    async fn watch_source_with_handle_missing_source_returns_path_not_found() {
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs-watcher-handle-readiness");
        let err = watch_source_with_handle(bogus.clone(), 50)
            .await
            .expect_err("missing source must error before spawning");
        match err {
            WatchError::PathNotFound(p) => assert_eq!(p, bogus),
            other => panic!("expected PathNotFound, got {other:?}"),
        }
    }

    /// Explicit shutdown must complete promptly without depending on a
    /// filesystem event firing. We pin a generous upper bound (a couple
    /// of debounce windows) so the test stays robust on slow CI hosts
    /// while still failing if shutdown silently waits for a real event.
    #[tokio::test]
    async fn explicit_shutdown_completes_without_filesystem_event() {
        let dir = tempfile::tempdir().expect("temp source dir");
        let (rx, handle) = watch_source_with_handle(dir.path().to_path_buf(), 100)
            .await
            .expect("watcher must attach");

        // No filesystem activity. Shutdown must still return promptly.
        let shutdown =
            tokio::time::timeout(std::time::Duration::from_secs(2), handle.shutdown()).await;
        assert!(
            shutdown.is_ok(),
            "explicit shutdown must complete without waiting for a filesystem event"
        );

        // Receiver outlives the handle (deliberately) so we can confirm
        // the channel ends up closed once the task has exited. Drain any
        // pre-shutdown events; the channel must end (recv() returns
        // None) because the task has dropped its sender.
        let mut rx = rx;
        while rx.try_recv().is_ok() {}
        // After shutdown the task is gone; the next blocking recv must
        // observe channel close rather than hang.
        let close = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("recv must not hang after shutdown");
        assert!(
            close.is_none(),
            "receiver must observe channel close after explicit shutdown"
        );
    }

    /// Repeated start/stop cycles must succeed. Each cycle attaches a
    /// fresh notify watcher, signals shutdown, and awaits completion.
    /// This pins the embedder use case: a process that mounts and
    /// unmounts SkillFS multiple times in the same runtime must not
    /// leak watcher tasks or fail on the second start.
    #[tokio::test]
    async fn repeated_start_and_shutdown_cycles_succeed() {
        let dir = tempfile::tempdir().expect("temp source dir");
        for _ in 0..3 {
            let (rx, handle) = watch_source_with_handle(dir.path().to_path_buf(), 50)
                .await
                .expect("each cycle must attach a fresh watcher");
            drop(rx);
            handle.shutdown().await;
        }
    }

    #[tokio::test]
    async fn dropped_receivers_stop_quiet_watchers_without_waiting_for_debounce() {
        let dir = tempfile::tempdir().expect("temp source dir");
        for explicit_handle in [false, true] {
            for _ in 0..3 {
                let (shutdown_tx, shutdown_rx) = oneshot::channel();
                let shutdown_rx = explicit_handle.then_some(shutdown_rx);
                let (rx, mut join) = start_watcher(dir.path().to_path_buf(), 60_000, shutdown_rx)
                    .await
                    .expect("watcher must attach");
                drop(rx);

                // Keep the explicit shutdown sender live: receiver closure alone
                // must release the task and its native watcher on a quiet source.
                let completed =
                    tokio::time::timeout(std::time::Duration::from_secs(2), &mut join).await;
                if completed.is_err() {
                    join.abort();
                    let _ = join.await;
                }
                drop(shutdown_tx);
                assert!(
                    completed.is_ok(),
                    "dropped receiver left its watcher running"
                );
                assert!(completed.unwrap().is_ok(), "watcher task must exit cleanly");
            }
        }
    }
}
