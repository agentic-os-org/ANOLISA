use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

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
    let mut pending: HashMap<PathBuf, (Instant, notify::EventKind)> = HashMap::new();

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
                for path in &event.paths {
                    pending.insert(path.clone(), (Instant::now(), event.kind));
                }
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
                    if let Some(event) = classify_event(&source, &path, kind) {
                        if tx.send(event).is_err() {
                            return; // receiver dropped
                        }
                    }
                }
            }
        }
    }
}

/// Classify a filesystem event into a SkillEvent, filtering irrelevant files.
///
/// **Coverage.** This intentionally limits emission to two narrow shapes:
///
/// * `<source>/<skill>/SKILL.md` — file create/modify/remove (used by the
///   skill manifest tracking pipeline);
/// * `<source>/<skill>` — immediate skill-directory create/remove.
///
/// Arbitrary files inside a skill (`scripts/run.sh`, `notes.txt`,
/// `.skill-meta/manifest.json`) and nested layouts deeper than depth 2
/// are **not** surfaced. The W1 drift runtime in `skillfs-fuse` therefore
/// only observes manifest- and skill-directory-level drift, mirroring this
/// helper's intentional scope.
fn classify_event(source: &Path, path: &Path, kind: notify::EventKind) -> Option<SkillEvent> {
    use notify::EventKind;

    let is_skill_md = path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md");

    let is_in_skill_dir = path
        .parent()
        .and_then(|p| p.parent())
        .map(|pp| pp == source)
        .unwrap_or(false);

    let is_immediate_child = path.parent().map(|p| p == source).unwrap_or(false);

    if is_skill_md && is_in_skill_dir {
        match kind {
            EventKind::Create(_) => Some(SkillEvent::Created(path.to_path_buf())),
            EventKind::Modify(_) => Some(SkillEvent::Modified(path.to_path_buf())),
            EventKind::Remove(_) => Some(SkillEvent::Deleted(path.to_path_buf())),
            _ => None,
        }
    } else if is_immediate_child {
        match kind {
            EventKind::Create(_) if path.is_dir() => {
                Some(SkillEvent::DirCreated(path.to_path_buf()))
            }
            // Removed paths no longer exist. Use the event's original object
            // kind instead of inspecting the filesystem after deletion.
            EventKind::Remove(notify::event::RemoveKind::Folder) => {
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
    fn removed_immediate_directory_is_classified_without_restat() {
        let source = tempfile::tempdir().expect("source directory");
        let child = source.path().join("alpha");
        std::fs::create_dir(&child).expect("skill directory");
        std::fs::remove_dir(&child).expect("remove skill directory");

        let event = classify_event(
            source.path(),
            &child,
            notify::EventKind::Remove(notify::event::RemoveKind::Folder),
        );
        assert!(matches!(event, Some(SkillEvent::DirDeleted(path)) if path == child));
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
                classify_event(source.path(), &file, notify::EventKind::Remove(kind)).is_none(),
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
