//! Package W1 source drift runtime wiring tests.
//!
//! These tests pin three properties that together make W1 a safe addition
//! on top of the W0 visibility-only drift seam:
//!
//! 1. when the runtime adapter is wired up against a real audit sink, an
//!    out-of-band source change observed through the watcher pipeline
//!    surfaces as a `source_changed` event with the same JSONL shape as
//!    on-FUSE-side audit emission;
//! 2. when no producer is wired, the FUSE filesystem behaves exactly as
//!    it did before W1 — no drift events appear and no audit log file is
//!    created;
//! 3. running the runtime adapter alongside a live FUSE mount must not
//!    disturb FUSE-served reads/writes in either direction (this is the
//!    invariant W1 inherits from W0: drift observation is decoupled from
//!    the FUSE event loop).
//!
//! The deterministic tests feed the adapter through a synthetic
//! `tokio::sync::mpsc` channel rather than starting the real `notify`
//! watcher. The skillfs-core watcher's own filesystem-event tests live in
//! `crates/skillfs-core/tests/watcher_tests.rs` and are `#[ignore]`-marked
//! because notify event timing is flaky in CI; the W1 adapter does not
//! need that fragility to be exercised here.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::watcher::SkillEvent as CoreWatcherEvent;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    AuditPathError, AuditRuntimeConfig, DriftChangeKind, DriftScope, DriftWatcherHandle,
    InMemoryEventSink, SkillEventAction, SkillEventKind, SkillEventSink, SourceDriftObserver,
    core_event_to_drift_event, drive_drift_watcher, spawn_drift_watcher,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

use common::{create_skill_dir, fuse_available};

macro_rules! skip_if_no_fuse {
    () => {
        if !fuse_available() {
            eprintln!(
                "SKIP {}: FUSE not available (no /dev/fuse or fusermount3)",
                ::std::module_path!()
            );
            return;
        }
    };
}

/// Pure adapter contract: each core watcher event variant maps to the
/// expected `(change_kind, scope)` tuple. Mirrors the unit tests inside
/// `drift_runtime.rs::tests` but lives here too so the integration suite
/// pins the public surface that downstream embedders see.
#[test]
fn core_event_to_drift_event_covers_every_core_variant() {
    let source = Path::new("/srv/skills");

    let cases: [(CoreWatcherEvent, DriftChangeKind, &str); 5] = [
        (
            CoreWatcherEvent::Created(source.join("alpha").join("SKILL.md")),
            DriftChangeKind::Created,
            "skill_md",
        ),
        (
            CoreWatcherEvent::Modified(source.join("alpha").join("SKILL.md")),
            DriftChangeKind::Modified,
            "skill_md",
        ),
        (
            CoreWatcherEvent::Deleted(source.join("alpha").join("SKILL.md")),
            DriftChangeKind::Deleted,
            "skill_md",
        ),
        (
            CoreWatcherEvent::DirCreated(source.join("alpha")),
            DriftChangeKind::Created,
            "skill_dir",
        ),
        (
            CoreWatcherEvent::DirDeleted(source.join("alpha")),
            DriftChangeKind::Deleted,
            "skill_dir",
        ),
    ];

    for (core, expected_kind, expected_scope_str) in cases {
        let drift = core_event_to_drift_event(source, &core);
        assert_eq!(
            drift.change_kind, expected_kind,
            "kind mismatch for {core:?}"
        );
        assert_eq!(
            drift.scope.as_str(),
            expected_scope_str,
            "scope mismatch for {core:?}"
        );
        // Skill name attribution is preserved when the lexical classifier
        // can derive it from the path.
        match (&drift.scope, expected_scope_str) {
            (DriftScope::SkillMd { skill_name }, "skill_md") => {
                assert_eq!(skill_name, "alpha");
            }
            (DriftScope::SkillDir { skill_name }, "skill_dir") => {
                assert_eq!(skill_name, "alpha");
            }
            other => panic!("unexpected scope variant {:?}", other),
        }
    }
}

/// Drive the adapter end-to-end through the W0 audit pipeline using an
/// `InMemoryEventSink`. This is the deterministic equivalent of "real
/// watcher fired an event": we synthesize a stream of core watcher events,
/// pass them through `drive_drift_watcher`, and assert the sink received
/// `SourceChanged` records with the same JSONL-equivalent shape that the
/// W0 unit tests pin.
#[tokio::test]
async fn runtime_emits_source_changed_events_through_audit_pipeline() {
    let source = Path::new("/srv/skills");
    let sink = Arc::new(InMemoryEventSink::new());
    let observer = Arc::new(SourceDriftObserver::new(source, sink.clone()));

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
    tx.send(CoreWatcherEvent::DirDeleted(source.join("alpha")))
        .unwrap();
    drop(tx);

    drive_drift_watcher(observer, rx).await;

    let recorded = sink.events();
    assert_eq!(recorded.len(), 4, "expected exactly four drift events");
    for event in &recorded {
        assert_eq!(event.kind, SkillEventKind::SourceChanged);
        assert_eq!(event.action, Some(SkillEventAction::Allowed));
    }

    // Spot-check the populated skill attribution and detail shape on the
    // first record, which is the most representative case.
    let first = &recorded[0];
    assert_eq!(first.skill_name.as_deref(), Some("alpha"));
    assert_eq!(first.relative_path.as_deref(), Some(Path::new("SKILL.md")));
    let detail = first.detail.as_deref().unwrap();
    assert!(detail.contains("change=created"), "{detail}");
    assert!(detail.contains("scope=skill_md"), "{detail}");

    // SkillDir scopes omit relative_path because the directory itself
    // changed (no path inside the skill is implied).
    let dir_created = &recorded[2];
    assert_eq!(dir_created.skill_name.as_deref(), Some("beta"));
    assert!(dir_created.relative_path.is_none());
    let detail = dir_created.detail.as_deref().unwrap();
    assert!(detail.contains("scope=skill_dir"), "{detail}");
    assert!(detail.contains("change=created"), "{detail}");
}

/// Drift observations emitted through the W1 adapter must reach the same
/// `JsonlFileAuditSink` that audit logging uses, with the existing
/// `source_changed` JSONL kind label and stable optional fields. This
/// proves the wiring shares one pipeline with FUSE-side emission rather
/// than introducing a parallel one.
#[tokio::test]
async fn runtime_writes_source_changed_events_to_audit_jsonl_log() {
    let log_dir = tempfile::tempdir().expect("audit log dir");
    let log_path = log_dir.path().join("audit.jsonl");

    let runtime = AuditRuntimeConfig::enabled(&log_path);
    let sink = runtime
        .build_sink()
        .expect("audit sink must build")
        .expect("enabled config must yield Some(sink)");

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let observer = Arc::new(SourceDriftObserver::new(source_dir.path(), sink.clone()));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(CoreWatcherEvent::Modified(
        source_dir.path().join("alpha").join("SKILL.md"),
    ))
    .unwrap();
    drop(tx);

    drive_drift_watcher(observer, rx).await;

    // Drop the sink so the writer thread observes channel close, flushes,
    // and exits before we read the on-disk log.
    drop(sink);

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut content = String::new();
    loop {
        content.clear();
        if let Ok(c) = std::fs::read_to_string(&log_path) {
            if !c.is_empty() {
                content = c;
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "drift event was not written to audit log at {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut saw_source_changed = false;
    for line in content.lines() {
        let v: serde_json::Value =
            serde_json::from_str(line).expect("each audit line must be valid JSON");
        assert!(v.get("kind").is_some(), "missing kind in {line}");
        assert!(
            v.get("ts_unix_ms").is_some(),
            "missing ts_unix_ms in {line}"
        );
        if v["kind"] == "source_changed" {
            assert_eq!(v["action"], "allowed");
            assert_eq!(v["skill"], "alpha");
            assert_eq!(v["path"], "SKILL.md");
            let detail = v["detail"].as_str().expect("detail must be set");
            assert!(detail.contains("change=modified"), "{detail}");
            assert!(detail.contains("scope=skill_md"), "{detail}");
            saw_source_changed = true;
        }
    }
    assert!(
        saw_source_changed,
        "expected a source_changed JSONL line; got: {content}"
    );
}

/// Mount fixture mirroring the one in `audit_runtime_tests.rs`, but
/// extended to optionally attach a drift observer to the same audit sink
/// the FUSE side uses. Default-off keeps the pre-W1 behavior.
struct DriftMount {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl DriftMount {
    fn new(seed: impl FnOnce(&Path), sink: Option<Arc<dyn SkillEventSink>>) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false,
            MountConfig {
                event_sink: sink,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");

        std::thread::sleep(Duration::from_millis(300));

        Self {
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    fn passthrough(&self, skill: &str, rel: &str) -> std::path::PathBuf {
        self.mountpoint.path().join("skills").join(skill).join(rel)
    }

    fn source_path(&self) -> &Path {
        self.source.path()
    }
}

impl Drop for DriftMount {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        let mp = self.mountpoint.path().to_path_buf();
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &mp.to_string_lossy()])
            .output();
    }
}

/// W1 default behavior: when no operator opt-in happened (no audit sink
/// attached, no drift watcher spawned), a regular FUSE mount must not
/// produce any drift records, must not create a watcher thread, and must
/// not change FUSE I/O behavior. This is the "no `--audit-log` keeps
/// pre-W1 behavior" invariant.
#[test]
fn default_runtime_does_not_emit_source_changed_events() {
    skip_if_no_fuse!();

    // Attach an in-memory sink only so we can prove no drift events fire,
    // not so we can actually opt in. The drift watcher is **not** spawned
    // here.
    let sink = Arc::new(InMemoryEventSink::new());
    let typed_sink: Arc<dyn SkillEventSink> = sink.clone();

    let mount = DriftMount::new(
        |dir| {
            create_skill_dir(dir, "alpha");
        },
        Some(typed_sink),
    );

    // Trigger ordinary passthrough activity. Because no W1 producer is
    // wired up, no SourceChanged events should appear even when the
    // physical source file changes outside the FUSE mount.
    let p = mount.passthrough("alpha", "notes.txt");
    std::fs::write(&p, b"hello").expect("plain write through FUSE");
    std::fs::write(
        mount
            .source_path()
            .join("alpha")
            .join("notes-out-of-band.txt"),
        b"out of band",
    )
    .expect("plain write directly to source");

    // Allow the FUSE thread to settle so any S2 events have time to land.
    std::thread::sleep(Duration::from_millis(150));

    let source_changed = sink.of_kind(SkillEventKind::SourceChanged);
    assert!(
        source_changed.is_empty(),
        "no W1 producer wired, but SourceChanged events appeared: {source_changed:?}"
    );
}

/// Running the runtime adapter alongside a live FUSE mount must not affect
/// FUSE-served reads or writes. We exercise the adapter through the same
/// audit sink the FUSE side uses, then prove FUSE I/O is identical.
#[test]
fn runtime_adapter_does_not_disturb_live_fuse_io() {
    skip_if_no_fuse!();

    let sink = Arc::new(InMemoryEventSink::new());
    let typed_sink: Arc<dyn SkillEventSink> = sink.clone();

    let mount = DriftMount::new(
        |dir| {
            create_skill_dir(dir, "alpha");
            std::fs::write(dir.join("alpha").join("notes.txt"), b"hello\n")
                .expect("seed passthrough file");
        },
        Some(typed_sink),
    );

    // Baseline: read through the FUSE mount before any drift activity.
    let passthrough = mount.passthrough("alpha", "notes.txt");
    let before = std::fs::read(&passthrough).expect("read before drift");

    // Drive the adapter through a synthetic channel — equivalent to the
    // real notify watcher firing, but deterministic. Crucially this runs
    // on a tokio runtime that is *not* the FUSE event loop, mirroring how
    // the CLI wires the producer in `cmd_mount`.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime for drift runtime test");

    runtime.block_on(async {
        let observer = Arc::new(SourceDriftObserver::new(
            mount.source_path().to_path_buf(),
            sink.clone() as Arc<dyn SkillEventSink>,
        ));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(CoreWatcherEvent::Modified(
            mount.source_path().join("alpha").join("notes.txt"),
        ))
        .unwrap();
        tx.send(CoreWatcherEvent::DirCreated(
            mount.source_path().join("beta"),
        ))
        .unwrap();
        drop(tx);
        drive_drift_watcher(observer, rx).await;
    });

    // The drift events made it through the shared sink.
    let drift = sink.of_kind(SkillEventKind::SourceChanged);
    assert_eq!(drift.len(), 2, "expected drift events to land in the sink");

    // FUSE-served content is unchanged: read returns the same bytes.
    let after = std::fs::read(&passthrough).expect("read after drift");
    assert_eq!(
        before, after,
        "FUSE-served content must be unchanged by drift"
    );

    // FUSE-served writes still succeed and are observable.
    std::fs::write(&passthrough, b"updated\n").expect("write through mount after drift");
    let updated = std::fs::read(&passthrough).expect("read updated through mount");
    assert_eq!(updated, b"updated\n");
}

/// W1 audit-path-vs-source guard: the runtime helper must reject an
/// `--audit-log` path that lands inside the SkillFS source tree before
/// the audit log file is created. Mirrors the CLI startup ordering
/// (validate → build_sink → mount) so a rejected configuration never
/// pollutes the source tree on disk.
#[test]
fn audit_log_inside_source_root_is_rejected_before_sink_construction() {
    let source = tempfile::tempdir().expect("source tempdir");
    let source_canon = source.path().canonicalize().expect("canonicalize source");
    let inside = source.path().join("audit.jsonl");
    let runtime = AuditRuntimeConfig::enabled(&inside);

    let err = runtime
        .validate_audit_path_outside_source(&source_canon)
        .expect_err("audit log inside source root must be rejected");
    assert!(
        matches!(err, AuditPathError::InsideSource { .. }),
        "expected InsideSource, got {err:?}"
    );

    // Defense in depth: the rejected configuration must not have produced
    // the audit log file. The CLI never reaches `build_sink` after the
    // guard fails, so the file should still be absent.
    assert!(
        !inside.exists(),
        "rejected audit log path must not be created on disk"
    );
}

/// W1 audit-path-vs-source guard: the same audit log path placed
/// **outside** the source tree must continue to be accepted, including
/// the SKILL.md-overwrite shape only when it is in a different directory.
#[test]
fn audit_log_outside_source_root_is_accepted() {
    let source = tempfile::tempdir().expect("source tempdir");
    let source_canon = source.path().canonicalize().expect("canonicalize source");
    let log_dir = tempfile::tempdir().expect("log dir");
    let outside = log_dir.path().join("audit.jsonl");
    let runtime = AuditRuntimeConfig::enabled(&outside);

    runtime
        .validate_audit_path_outside_source(&source_canon)
        .expect("disjoint audit log path must pass the W1 guard");
}

/// W1 readiness contract end-to-end through `spawn_drift_watcher`. The
/// returned future must only resolve `Ok` after the underlying notify
/// watcher has been attached to the source root. This is the property
/// the CLI relies on to safely await the watcher before the FUSE event
/// loop starts.
#[tokio::test]
async fn spawn_drift_watcher_returns_only_after_notify_watcher_attaches() {
    let source = tempfile::tempdir().expect("source tempdir");
    let sink = Arc::new(InMemoryEventSink::new());
    let observer = Arc::new(SourceDriftObserver::new(
        source.path().to_path_buf(),
        sink.clone() as Arc<dyn SkillEventSink>,
    ));

    let handle: DriftWatcherHandle = spawn_drift_watcher(source.path().to_path_buf(), observer, 50)
        .await
        .expect("real source dir must produce a ready watcher");

    // The handle is live; the explicit shutdown path tears down the
    // watcher task and the drift adapter deterministically. We do not
    // depend on real notify event delivery here — that is flaky in CI
    // and is covered by the (ignored) tests in
    // `crates/skillfs-core/tests/watcher_tests.rs`. The contract this
    // test pins is structural: `spawn_drift_watcher().await` either
    // succeeds with an attached watcher or fails synchronously.
    handle.shutdown().await;
}

/// W1.1: explicit shutdown through [`DriftWatcherHandle::shutdown`] must
/// complete without waiting for a real filesystem event. We pin a
/// generous upper bound (a couple of debounce windows) so the test
/// stays robust on slow CI hosts while still failing if the shutdown
/// silently waits for a filesystem notification.
#[tokio::test]
async fn drift_watcher_explicit_shutdown_completes_without_filesystem_event() {
    let source = tempfile::tempdir().expect("source tempdir");
    let sink = Arc::new(InMemoryEventSink::new());
    let observer = Arc::new(SourceDriftObserver::new(
        source.path().to_path_buf(),
        sink.clone() as Arc<dyn SkillEventSink>,
    ));

    let handle = spawn_drift_watcher(source.path().to_path_buf(), observer, 100)
        .await
        .expect("watcher must attach");

    // No filesystem activity: shutdown must still return promptly.
    let elapsed = tokio::time::timeout(Duration::from_secs(2), handle.shutdown()).await;
    assert!(
        elapsed.is_ok(),
        "explicit shutdown must complete without waiting for a filesystem event"
    );

    // No drift events should have been emitted because nothing changed
    // on disk. This proves shutdown does not synthesize trailing events.
    assert!(
        sink.is_empty(),
        "explicit shutdown must not emit drift events on its own"
    );
}

/// W1.1: long-lived embedders that repeatedly start and stop the drift
/// watcher must succeed every cycle. Each iteration attaches a fresh
/// notify watcher, signals shutdown, and awaits both the core watcher
/// and the drift adapter task. No leaks, no second-start failure.
#[tokio::test]
async fn drift_watcher_repeated_start_and_shutdown_cycles_succeed() {
    let source = tempfile::tempdir().expect("source tempdir");
    let sink = Arc::new(InMemoryEventSink::new());

    for _ in 0..3 {
        let observer = Arc::new(SourceDriftObserver::new(
            source.path().to_path_buf(),
            sink.clone() as Arc<dyn SkillEventSink>,
        ));
        let handle = spawn_drift_watcher(source.path().to_path_buf(), observer, 50)
            .await
            .expect("each cycle must attach a fresh watcher");
        // Explicit deterministic teardown — embedder use case.
        handle.shutdown().await;
    }

    // No filesystem activity occurred, so no drift events should have
    // been emitted across the cycles. This also pins that shutdown does
    // not race trailing events through the sink.
    assert!(
        sink.is_empty(),
        "no drift events should fire across pure start/stop cycles"
    );
}

/// W1 readiness contract: `spawn_drift_watcher` against a missing
/// source path returns `Err` synchronously and does not spawn an
/// orphan watcher task. This is the failure mode the original review
/// flagged ("watcher startup failure surfaces silently").
#[tokio::test]
async fn spawn_drift_watcher_returns_error_synchronously_for_missing_source() {
    let bogus = std::path::PathBuf::from("/nonexistent/skillfs-w1-spawn-readiness");
    let sink = Arc::new(InMemoryEventSink::new());
    let observer = Arc::new(SourceDriftObserver::new(
        bogus.clone(),
        sink.clone() as Arc<dyn SkillEventSink>,
    ));

    let err = spawn_drift_watcher(bogus, observer, 50)
        .await
        .expect_err("missing source must surface synchronously");
    // We don't pin the exact variant here (the underlying notify error
    // shape can vary by platform); structural readiness is what matters.
    let _ = err;
    // The sink must remain empty because no watcher was started.
    assert!(sink.is_empty());
}
