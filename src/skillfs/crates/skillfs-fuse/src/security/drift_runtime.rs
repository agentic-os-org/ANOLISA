//! Runtime watcher wiring for source drift observation (Package W1).
//!
//! Background. Package W0 added the [`super::drift::SourceDriftObserver`]
//! seam: a way to turn an out-of-band source-tree observation into a
//! normalized [`super::event::SkillEventKind::SourceChanged`] audit
//! record. W0 deliberately did not wire any producer. W1 fills that gap
//! with a small adapter that converts the existing
//! [`skillfs_core::watcher::SkillEvent`] vocabulary into
//! [`super::drift::DriftEvent`] and feeds it through the observer
//! pipeline.
//!
//! **Coverage scope (intentionally narrow).** The producer in
//! `skillfs-core::watcher::classify_event` only emits two shapes of
//! events:
//!
//! * `<source>/<skill>/SKILL.md` create / modify / delete; and
//! * `<source>/<skill>` immediate skill-directory create / delete.
//!
//! Everything else — arbitrary files inside a skill (`scripts/run.sh`,
//! `notes.txt`), `.skill-meta/**`, deeper nested layouts, top-level
//! files at the source root — does **not** flow through W1. The W1
//! adapter therefore only surfaces drift for the SKILL.md manifest and
//! the immediate skill-directory shape. Broader watcher coverage is a
//! deliberate non-goal of this package; future work that widens the
//! producer in `skillfs-core::watcher` will automatically widen what W1
//! observes without changing the adapter.
//!
//! Visibility-only contract. Nothing in this module blocks operations,
//! refreshes the [`skillfs_core::store::SkillStore`], quarantines content,
//! or transitions a lifecycle. A drift event is an audit record describing
//! an out-of-band observation; SkillFS does not enforce policy in response
//! to it. POSIX errno paths, compiled `SKILL.md` semantics,
//! `skill-discover` virtual semantics, and `.skill-meta` enforcement are
//! unchanged by W1.
//!
//! Watcher startup is fail-fast (since W1). [`watch_source_with_handle`]
//! in `skillfs-core` only resolves to `Ok((rx, handle))` after the
//! underlying notify watcher has actually attached to the source
//! directory. That means [`spawn_drift_watcher`] returns
//! `Ok(DriftWatcherHandle)` only after the producer is observing, and
//! `Err(WatchError)` synchronously on init failure — callers can
//! therefore order the FUSE mount `await` after the watcher `await` and
//! rely on the adapter being live before FUSE over-mounts the source
//! path.
//!
//! Explicit shutdown (Package W1.1). [`DriftWatcherHandle`] now wraps
//! the core [`skillfs_core::watcher::WatcherHandle`] and the drift
//! adapter task into a single composite. Calling
//! [`DriftWatcherHandle::shutdown`] signals the core watcher to exit,
//! awaits its task, and then awaits the drift adapter (which exits
//! naturally when the watcher's outbound channel closes). This is the
//! deterministic teardown path long-lived embedders need; the CLI uses
//! it on Ctrl+C / SIGTERM and on natural mount exit so the watcher does
//! not outlive the mount it was paired with. Dropping the handle without
//! calling `shutdown().await` falls back to the existing best-effort
//! abort path so misuse never deadlocks.
//!
//! Wiring shape. The runtime adapter is intentionally split into three
//! pieces so callers and tests can plug in at different layers without
//! pulling the real `notify` watcher into deterministic tests:
//!
//! 1. [`core_event_to_drift_event`] is a pure conversion. It runs the
//!    existing W0 lexical classifier through [`super::drift::DriftEvent::classify`]
//!    so flat-layout attribution and the categorized-layout safeguard
//!    (no fabricated `skill=<category>` for `<source>/<a>/<b>/SKILL.md`)
//!    keep their W0 behavior. No syscalls, no runtime dependency.
//! 2. [`drive_drift_watcher`] consumes a watcher channel — typically the
//!    [`tokio::sync::mpsc::UnboundedReceiver`] returned by
//!    [`skillfs_core::watcher::watch_source`] — and emits each event
//!    through an [`super::drift::SourceDriftObserver`]. Tests can drive
//!    this with a synthetic channel and verify the audit pipeline end to
//!    end without depending on real filesystem notifications.
//! 3. [`spawn_drift_watcher`] glues 1 + 2 to the real
//!    [`skillfs_core::watcher::watch_source`] and returns a
//!    [`tokio::task::JoinHandle`] so the caller can shut the producer down.
//!
//! Default behavior is still no-op. Nothing in this module is invoked
//! unless a caller explicitly spawns the watcher (the CLI does this only
//! when `--audit-log` is set; programmatic embedders opt in by calling
//! [`spawn_drift_watcher`] themselves). When no producer is started, the
//! audit/drift pipeline behaves exactly as it did under W0, and the FUSE
//! filesystem is untouched.
//!
//! Scope discipline (intentionally out of scope here, mirroring W0):
//!
//! * trusted-writer identity / process attribution;
//! * skill-ledger allowlists / capability enforcement;
//! * lifecycle namespaces, quarantine, scanner invocation;
//! * symlink, hardlink, xattr, `mknod`, `fallocate`, `lseek`,
//!   `copy_file_range`;
//! * broad watcher-driven hot sync / store reparse / view reload
//!   (W1 only emits audit observations; the store is not refreshed);
//! * `skillfs-views.toml` hot reload.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use skillfs_core::watcher::{
    SkillEvent as CoreWatcherEvent, WatchError, WatcherHandle, watch_source_with_handle,
};

use super::drift::{DriftChangeKind, DriftEvent, SourceDriftObserver};

/// Composite shutdown handle for the W1 drift runtime.
///
/// Wraps the core [`WatcherHandle`] (which owns the underlying notify
/// watcher) and the [`tokio::task::JoinHandle`] of the drift adapter
/// task spawned by [`spawn_drift_watcher`]. Calling
/// [`DriftWatcherHandle::shutdown`] tears both down deterministically:
///
/// 1. [`WatcherHandle::shutdown`] signals the core event loop to exit
///    and awaits its task. When the task exits the watcher's outbound
///    `mpsc::UnboundedSender<SkillEvent>` is dropped.
/// 2. [`drive_drift_watcher`] running on the adapter task observes that
///    sender drop, its `recv()` returns `None`, and the loop exits. The
///    handle then awaits the adapter join.
///
/// This is the path long-lived embedders should use to stop the producer
/// without depending on runtime teardown or relying on the next outbound
/// send-failure to break the loop. The CLI calls it on Ctrl+C, SIGTERM,
/// and on natural mount exit so the watcher is always paired with the
/// mount it was started for.
///
/// Dropping the handle without `shutdown().await` is best-effort: the
/// inner [`WatcherHandle`] Drop signals + aborts the watcher task, and
/// the adapter join handle is dropped (which detaches it without
/// blocking). Prefer the explicit path when timing matters.
#[derive(Debug)]
pub struct DriftWatcherHandle {
    watcher: Option<WatcherHandle>,
    drive_join: Option<JoinHandle<()>>,
}

impl DriftWatcherHandle {
    /// Signal the core watcher to exit and await both the watcher task
    /// and the drift adapter task.
    ///
    /// Returns once both tasks have completed. Ordering is significant:
    /// the watcher must shut down first so its outbound sender is
    /// dropped, which closes the channel feeding [`drive_drift_watcher`]
    /// and lets that loop exit on its own. Awaiting the adapter without
    /// closing the channel first would hang forever.
    pub async fn shutdown(mut self) {
        // Step 1: tear down the core watcher. Its `WatcherHandle::shutdown`
        // signals the event loop to exit and awaits the spawned task,
        // dropping the underlying notify watcher and the outbound sender.
        if let Some(w) = self.watcher.take() {
            w.shutdown().await;
        }
        // Step 2: drive_drift_watcher's `rx.recv()` now returns `None`,
        // so the loop exits. Await the adapter task to surface panics
        // and confirm full teardown.
        if let Some(h) = self.drive_join.take() {
            let _ = h.await;
        }
    }
}

impl Drop for DriftWatcherHandle {
    fn drop(&mut self) {
        // Best-effort fallback when the embedder forgot to call
        // `shutdown().await`. Dropping the inner `WatcherHandle` runs
        // its own Drop which signals shutdown and aborts the watcher
        // task; the adapter join handle is dropped (detached) without
        // blocking. The receiver-drop chain still closes the adapter
        // channel and the loop exits on its own shortly after.
        self.watcher.take();
        self.drive_join.take();
    }
}

/// Convert a [`skillfs_core::watcher::SkillEvent`] into a [`DriftEvent`] by
/// running the path through the W0 lexical classifier.
///
/// The mapping is:
///
/// * `Created(p)` / `DirCreated(p)` → [`DriftChangeKind::Created`];
/// * `Modified(p)`                  → [`DriftChangeKind::Modified`];
/// * `Deleted(p)` / `DirDeleted(p)` → [`DriftChangeKind::Deleted`].
///
/// The observed path is forwarded verbatim into
/// [`DriftEvent::classify`], which means W0's flat-layout attribution and
/// categorized-layout safeguard apply unchanged: paths under
/// `<source>/<skill>/SKILL.md` produce a [`super::drift::DriftScope::SkillMd`]
/// scope, paths at `<source>/<skill>` produce a
/// [`super::drift::DriftScope::SkillDir`] scope when the component is a
/// valid skill name, invalid skill-name components route to
/// [`super::drift::DriftScope::InsideSourceOutsideSkill`], and a stray
/// nested manifest under `<source>/<a>/<b>/.../SKILL.md` is also routed to
/// [`super::drift::DriftScope::InsideSourceOutsideSkill`] rather than
/// fabricating a `skill=<category>` attribution.
///
/// This conversion is pure: no syscalls, no runtime dependency, no
/// allocation beyond the [`PathBuf`] clone the observed path requires.
pub fn core_event_to_drift_event(source_root: &Path, event: &CoreWatcherEvent) -> DriftEvent {
    match event {
        CoreWatcherEvent::Created(p) => {
            DriftEvent::classify(source_root, p.clone(), DriftChangeKind::Created)
        }
        CoreWatcherEvent::Modified(p) => {
            DriftEvent::classify(source_root, p.clone(), DriftChangeKind::Modified)
        }
        CoreWatcherEvent::Deleted(p) => {
            DriftEvent::classify(source_root, p.clone(), DriftChangeKind::Deleted)
        }
        CoreWatcherEvent::DirCreated(p) => {
            DriftEvent::classify(source_root, p.clone(), DriftChangeKind::Created)
        }
        CoreWatcherEvent::DirDeleted(p) => {
            DriftEvent::classify(source_root, p.clone(), DriftChangeKind::Deleted)
        }
    }
}

/// Drive a watcher channel to completion, converting each event into a
/// [`DriftEvent`] and emitting it through `observer`.
///
/// Returns when `rx` is closed (i.e. every sender has been dropped). Each
/// emission is best-effort: the underlying [`super::event::SkillEventSink`]
/// implementation decides what happens when the audit pipeline is
/// saturated, exactly as on the FUSE side. The loop never blocks longer
/// than `recv()` plus a single sink emission.
///
/// Tests that want to verify the runtime adapter without depending on
/// the real `notify` watcher can construct a
/// [`tokio::sync::mpsc::unbounded_channel`] and feed events directly.
pub async fn drive_drift_watcher(
    observer: Arc<SourceDriftObserver>,
    mut rx: UnboundedReceiver<CoreWatcherEvent>,
) {
    while let Some(event) = rx.recv().await {
        let drift = core_event_to_drift_event(observer.source_root(), &event);
        observer.observe_event(&drift);
    }
}

/// Spawn a background watcher task that observes `source_root` and emits
/// drift events through `observer`. Must be called from within a tokio
/// runtime context.
///
/// On startup failure (missing source path, notify initialization error,
/// or `watcher.watch(...)` rejection by the kernel) returns the underlying
/// [`WatchError`] **without** spawning anything. The
/// [`watch_source_with_handle`](skillfs_core::watcher::watch_source_with_handle)
/// readiness contract guarantees that startup-time failures are surfaced
/// before this future resolves, so callers can `.await` this function and
/// treat its return value the same way they would treat any other
/// fallible startup step. Drift observation is a visibility-only audit
/// aid, so callers should log and continue rather than aborting the FUSE
/// mount when the result is `Err`.
///
/// On success returns a [`DriftWatcherHandle`] that owns both the core
/// watcher and the drift adapter task. The watcher is already attached to
/// `source_root` by the time this future resolves, so it is safe for the
/// caller to proceed with subsequent setup (e.g. starting a FUSE
/// over-mount on the same path) without racing the watcher's attach
/// phase. Calling [`DriftWatcherHandle::shutdown`] tears both halves down
/// deterministically; dropping the handle is best-effort fallback only.
pub async fn spawn_drift_watcher(
    source_root: PathBuf,
    observer: Arc<SourceDriftObserver>,
    debounce_ms: u64,
) -> Result<DriftWatcherHandle, WatchError> {
    let (rx, watcher) = watch_source_with_handle(source_root, debounce_ms).await?;
    let drive_join = tokio::spawn(async move {
        drive_drift_watcher(observer, rx).await;
    });
    Ok(DriftWatcherHandle {
        watcher: Some(watcher),
        drive_join: Some(drive_join),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::drift::DriftScope;
    use crate::security::event::{InMemoryEventSink, SkillEventAction, SkillEventKind};
    use std::path::Path;

    fn observer_with_in_memory_sink(
        source_root: &Path,
    ) -> (Arc<SourceDriftObserver>, Arc<InMemoryEventSink>) {
        let sink = Arc::new(InMemoryEventSink::new());
        let observer = Arc::new(SourceDriftObserver::new(source_root, sink.clone()));
        (observer, sink)
    }

    #[test]
    fn core_created_skill_md_classifies_as_skill_md_with_created_kind() {
        let source = Path::new("/srv/skills");
        let event = CoreWatcherEvent::Created(source.join("alpha").join("SKILL.md"));
        let drift = core_event_to_drift_event(source, &event);

        assert_eq!(drift.change_kind, DriftChangeKind::Created);
        assert_eq!(
            drift.scope,
            DriftScope::SkillMd {
                skill_name: "alpha".to_string()
            }
        );
        assert_eq!(drift.original_path, source.join("alpha").join("SKILL.md"));
    }

    #[test]
    fn core_modified_skill_md_classifies_as_skill_md_with_modified_kind() {
        let source = Path::new("/srv/skills");
        let event = CoreWatcherEvent::Modified(source.join("alpha").join("SKILL.md"));
        let drift = core_event_to_drift_event(source, &event);

        assert_eq!(drift.change_kind, DriftChangeKind::Modified);
        assert_eq!(
            drift.scope,
            DriftScope::SkillMd {
                skill_name: "alpha".to_string()
            }
        );
    }

    #[test]
    fn core_deleted_skill_md_classifies_as_skill_md_with_deleted_kind() {
        let source = Path::new("/srv/skills");
        let event = CoreWatcherEvent::Deleted(source.join("alpha").join("SKILL.md"));
        let drift = core_event_to_drift_event(source, &event);

        assert_eq!(drift.change_kind, DriftChangeKind::Deleted);
        assert_eq!(
            drift.scope,
            DriftScope::SkillMd {
                skill_name: "alpha".to_string()
            }
        );
    }

    #[test]
    fn core_dir_created_classifies_as_skill_dir_with_created_kind() {
        let source = Path::new("/srv/skills");
        let event = CoreWatcherEvent::DirCreated(source.join("alpha"));
        let drift = core_event_to_drift_event(source, &event);

        assert_eq!(drift.change_kind, DriftChangeKind::Created);
        assert_eq!(
            drift.scope,
            DriftScope::SkillDir {
                skill_name: "alpha".to_string()
            }
        );
    }

    #[test]
    fn core_dir_deleted_classifies_as_skill_dir_with_deleted_kind() {
        let source = Path::new("/srv/skills");
        let event = CoreWatcherEvent::DirDeleted(source.join("alpha"));
        let drift = core_event_to_drift_event(source, &event);

        assert_eq!(drift.change_kind, DriftChangeKind::Deleted);
        assert_eq!(
            drift.scope,
            DriftScope::SkillDir {
                skill_name: "alpha".to_string()
            }
        );
    }

    #[test]
    fn invalid_skill_name_routes_through_inside_source_outside_skill() {
        // The W0 classifier rejects non-kebab top-level components so the
        // adapter must not fabricate `skill=Alpha` attribution. Also pin
        // the categorized-layout safeguard: a stray nested manifest does
        // not invent `skill=tools`.
        let source = Path::new("/srv/skills");
        let bad_dir = CoreWatcherEvent::DirCreated(source.join("Alpha"));
        let drift = core_event_to_drift_event(source, &bad_dir);
        assert!(
            matches!(drift.scope, DriftScope::InsideSourceOutsideSkill { .. }),
            "expected InsideSourceOutsideSkill, got {:?}",
            drift.scope
        );

        let nested =
            CoreWatcherEvent::Modified(source.join("tools").join("alpha").join("SKILL.md"));
        let drift = core_event_to_drift_event(source, &nested);
        assert!(
            matches!(drift.scope, DriftScope::InsideSourceOutsideSkill { .. }),
            "expected InsideSourceOutsideSkill for nested manifest, got {:?}",
            drift.scope
        );
    }

    #[test]
    fn paths_outside_source_route_to_outside_source_scope() {
        let source = Path::new("/srv/skills");
        let event = CoreWatcherEvent::Modified(PathBuf::from("/etc/passwd"));
        let drift = core_event_to_drift_event(source, &event);
        assert_eq!(drift.scope, DriftScope::OutsideSource);
        assert_eq!(drift.change_kind, DriftChangeKind::Modified);
    }

    #[tokio::test]
    async fn drive_drift_watcher_emits_each_received_event_through_observer() {
        let source = Path::new("/srv/skills");
        let (observer, sink) = observer_with_in_memory_sink(source);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(CoreWatcherEvent::Created(
            source.join("alpha").join("SKILL.md"),
        ))
        .unwrap();
        tx.send(CoreWatcherEvent::Modified(
            source.join("alpha").join("SKILL.md"),
        ))
        .unwrap();
        tx.send(CoreWatcherEvent::DirCreated(source.join("beta")))
            .unwrap();
        // Closing the sender lets `drive_drift_watcher` return.
        drop(tx);

        drive_drift_watcher(observer, rx).await;

        let recorded = sink.events();
        assert_eq!(recorded.len(), 3, "expected three drift events");

        // Event 1: SKILL.md create — scope=SkillMd, change=created.
        assert_eq!(recorded[0].kind, SkillEventKind::SourceChanged);
        assert_eq!(recorded[0].action, Some(SkillEventAction::Allowed));
        assert_eq!(recorded[0].skill_name.as_deref(), Some("alpha"));
        assert_eq!(
            recorded[0].relative_path.as_deref(),
            Some(Path::new("SKILL.md"))
        );
        let detail = recorded[0].detail.as_deref().unwrap();
        assert!(detail.contains("change=created"), "{detail}");
        assert!(detail.contains("scope=skill_md"), "{detail}");

        // Event 2: SKILL.md modify.
        assert_eq!(recorded[1].kind, SkillEventKind::SourceChanged);
        let detail = recorded[1].detail.as_deref().unwrap();
        assert!(detail.contains("change=modified"), "{detail}");
        assert!(detail.contains("scope=skill_md"), "{detail}");

        // Event 3: skill dir create — scope=SkillDir, no relative_path.
        assert_eq!(recorded[2].kind, SkillEventKind::SourceChanged);
        assert_eq!(recorded[2].skill_name.as_deref(), Some("beta"));
        assert_eq!(recorded[2].relative_path, None);
        let detail = recorded[2].detail.as_deref().unwrap();
        assert!(detail.contains("scope=skill_dir"), "{detail}");
        assert!(detail.contains("change=created"), "{detail}");
    }

    #[tokio::test]
    async fn drive_drift_watcher_returns_when_channel_closes_with_no_events() {
        // Closing the channel before sending anything must not produce
        // events and must terminate cleanly.
        let source = Path::new("/srv/skills");
        let (observer, sink) = observer_with_in_memory_sink(source);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<CoreWatcherEvent>();
        drop(tx);

        drive_drift_watcher(observer, rx).await;

        assert!(
            sink.is_empty(),
            "no events expected when channel closes empty"
        );
    }
}
