//! Source drift observer integration tests.
//!
//! These tests pin two properties:
//!
//! 1. `SourceDriftObserver` produces normalized
//!    [`SkillEventKind::SourceChanged`] records through an injected sink,
//!    using the same pipeline as FUSE-side audit emission.
//! 2. Calling the observer does **not** affect filesystem operations —
//!    nothing in this module is wired into the FUSE runtime, so a real
//!    SkillFS mount keeps serving reads and writes unchanged whether or
//!    not drift events are being produced alongside it.
//!
//! Tests that interact with a real FUSE mount use the shared `common`
//! fixture and skip cleanly when FUSE is unavailable. Tests that only
//! exercise the observer plumbing run unconditionally.

mod common;

use std::path::Path;
use std::sync::Arc;

use skillfs_fuse::security::{
    DriftChangeKind, DriftEvent, DriftScope, InMemoryEventSink, SkillEventAction, SkillEventKind,
    SourceDriftObserver,
};

use common::{MountFixture, create_skill_dir};

#[test]
fn observer_emits_source_changed_event_with_skill_attribution() {
    // Pure plumbing test: no FUSE required. Pin the JSONL-equivalent
    // shape of the SourceChanged event the observer produces for the
    // most common case (a manifest write under a known skill).
    let source = tempfile::tempdir().expect("source tempdir");
    create_skill_dir(source.path(), "alpha");

    let sink = Arc::new(InMemoryEventSink::new());
    let observer = SourceDriftObserver::new(source.path(), sink.clone());

    let manifest = source.path().join("alpha").join("SKILL.md");
    let drift = observer.observe(&manifest, DriftChangeKind::Modified);

    assert!(matches!(drift.scope, DriftScope::SkillMd { .. }));
    let recorded = sink.events();
    assert_eq!(recorded.len(), 1, "expected exactly one drift event");
    let event = &recorded[0];
    assert_eq!(event.kind, SkillEventKind::SourceChanged);
    assert_eq!(event.action, Some(SkillEventAction::Allowed));
    assert_eq!(event.skill_name.as_deref(), Some("alpha"));
    assert_eq!(event.relative_path.as_deref(), Some(Path::new("SKILL.md")));
    let detail = event.detail.as_deref().expect("detail must be set");
    assert!(detail.contains("change=modified"), "{detail}");
    assert!(detail.contains("scope=skill_md"), "{detail}");
}

#[test]
fn observer_emits_unattributed_event_for_outside_source_change() {
    // Drift can land outside the configured source root; the observer
    // must still emit a SourceChanged record but omit skill/path so
    // downstream consumers do not infer attribution that does not
    // exist.
    let source = tempfile::tempdir().expect("source tempdir");
    let sink = Arc::new(InMemoryEventSink::new());
    let observer = SourceDriftObserver::new(source.path(), sink.clone());

    let _drift = observer.observe(Path::new("/etc/passwd"), DriftChangeKind::Modified);

    let recorded = sink.events();
    assert_eq!(recorded.len(), 1);
    let event = &recorded[0];
    assert_eq!(event.kind, SkillEventKind::SourceChanged);
    assert_eq!(event.skill_name, None);
    assert_eq!(event.relative_path, None);
    let detail = event.detail.as_deref().unwrap();
    assert!(detail.contains("scope=outside_source"), "{detail}");
    assert!(detail.contains("path=/etc/passwd"), "{detail}");
}

#[test]
fn observer_passes_through_pre_classified_events() {
    // Producers that already know the scope (e.g. an external watcher
    // attribution mechanism) can short-circuit the classifier. The
    // observer must emit faithfully without re-running classification.
    let source = tempfile::tempdir().expect("source tempdir");
    let sink = Arc::new(InMemoryEventSink::new());
    let observer = SourceDriftObserver::new(source.path(), sink.clone());

    let drift = DriftEvent::with_scope(
        DriftChangeKind::Created,
        DriftScope::SkillDir {
            skill_name: "beta".to_string(),
        },
        "/elsewhere/beta",
    );
    observer.observe_event(&drift);

    let recorded = sink.events();
    assert_eq!(recorded.len(), 1);
    let event = &recorded[0];
    assert_eq!(event.skill_name.as_deref(), Some("beta"));
    let detail = event.detail.as_deref().unwrap();
    assert!(detail.contains("scope=skill_dir"), "{detail}");
    assert!(detail.contains("path=/elsewhere/beta"), "{detail}");
}

#[test]
fn observer_does_not_disturb_live_mount_filesystem_operations() {
    // The integration boundary we care about: while a real FUSE mount is
    // serving a passthrough file, calling SourceDriftObserver::observe
    // through the same sink shape an audit consumer would use must not
    // affect what userspace sees through the mount. SkillFS does not
    // wire the observer into any callback; this test pins that invariant
    // by exercising both sides side-by-side.
    skip_if_no_fuse!();

    let fixture = MountFixture::normal(|src| {
        create_skill_dir(src, "alpha");
        std::fs::write(src.join("alpha").join("notes.txt"), b"hello\n")
            .expect("seed passthrough file");
    });

    let sink = Arc::new(InMemoryEventSink::new());
    let observer = SourceDriftObserver::new(fixture.source(), sink.clone());

    // Read a passthrough file through the FUSE mount before drift
    // emission so we have a baseline behavior to compare against.
    let passthrough = fixture.passthrough_path("alpha", "notes.txt");
    let before = std::fs::read(&passthrough).expect("read before");

    // Emit a drift event for an out-of-band write to the same skill.
    // We pretend the file changed on the source side to simulate the
    // expected use case for this seam.
    let physical = fixture.source().join("alpha").join("notes.txt");
    let drift = observer.observe(&physical, DriftChangeKind::Modified);

    // Drift emission classifies into InsideSkill / SkillMd / SkillDir
    // depending on path; for `notes.txt` it must land in InsideSkill.
    assert!(
        matches!(drift.scope, DriftScope::InsideSkill { ref skill_name, .. } if skill_name == "alpha"),
        "expected InsideSkill scope, got {:?}",
        drift.scope
    );

    // Sink saw exactly the drift event the observer emitted.
    let recorded = sink.events();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].kind, SkillEventKind::SourceChanged);

    // FUSE mount is still serving the same content. Nothing about the
    // observer pipeline reaches into the filesystem.
    let after = std::fs::read(&passthrough).expect("read after");
    assert_eq!(before, after, "FUSE-served content must be unchanged");

    // And a fresh write through the mount still works — drift emission
    // is fully decoupled from passthrough write.
    std::fs::write(&passthrough, b"updated\n").expect("write through mount");
    let updated = std::fs::read(&passthrough).expect("read updated");
    assert_eq!(updated, b"updated\n");
}
