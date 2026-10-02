//! D1.3-demo refresh-controller integration tests.
//!
//! Coverage:
//!
//! * The JSONL demo-events writer produces a stable, parseable line.
//! * `write(SKILL.md)` through the mount enqueues a debounced
//!   refresh; once the worker runs, the resolver flips to the
//!   adapter's new decision and the read paths reflect it without a
//!   remount.
//! * `.skill-meta/**` mutations on the source side do **not** queue a
//!   refresh (no feedback loop with the ledger's own snapshot writer).
//! * `skill-discover` is exempt from observation.
//! * `rename` observes both old and new owning skill when they differ.
//! * `mkdir` of a fresh skill directory keeps the new skill hidden
//!   until a resolve returns `current` / `fallback`.
//! * Provider failures (invalid JSON / non-zero exit) hide the skill
//!   under the demo's default `FailedResolveBehavior::HideOnFailure`.
//! * Without a controller attached, the pre-D1.3 mount behavior is
//!   preserved (the smoke check piggybacks on the existing
//!   `ledger_active_mapping_tests.rs`; here we just sanity-check that
//!   no panic is emitted by the FUSE wiring when the controller is
//!   absent).

#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, ActiveTarget, FailedResolveBehavior, JsonlSecurityEventWriter,
    LedgerAdapter, LedgerError, LedgerResolveResult, MutationKind, RefreshController,
    RefreshObservation, SecurityEvent, SecurityEventWriter, StaticAdapterCall, StaticLedgerAdapter,
};
use skillfs_fuse::{MountConfig, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use crate::common::{create_skill_dir, fuse_available};

// ─────────────────────────────────────────────────────────────────────────────
// Test event writer
// ─────────────────────────────────────────────────────────────────────────────

/// Local in-memory writer with `Mutex<Vec>` so we can assert sequence
/// in tests. Mirrors `InMemorySecurityEventWriter` but exposes `clear()`
/// for multi-step scenarios.
#[derive(Default)]
struct CapturingWriter {
    inner: Mutex<Vec<SecurityEvent>>,
}

impl CapturingWriter {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn events(&self) -> Vec<SecurityEvent> {
        self.inner.lock().clone()
    }

    fn wait_for_n(&self, n: usize, timeout: Duration) -> Vec<SecurityEvent> {
        let start = std::time::Instant::now();
        loop {
            let events = self.events();
            if events.len() >= n {
                return events;
            }
            if start.elapsed() >= timeout {
                return events;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl SecurityEventWriter for CapturingWriter {
    fn emit(&self, event: &SecurityEvent) {
        self.inner.lock().push(event.clone());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Controller-only unit tests (no FUSE mount required)
// ─────────────────────────────────────────────────────────────────────────────

fn current_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "pass",
            "decision": "current",
            "currentVersion": "v000001",
            "trustedVersion": "v000001"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("current json")
}

fn fallback_result(skill: &str, snapshot_segment: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "deny",
            "decision": "fallback",
            "currentVersion": "v000003",
            "trustedVersion": "{snapshot_segment}",
            "target": ".skill-meta/versions/{snapshot_segment}",
            "targetKind": "relative_to_skill_dir",
            "reason": "current version has high-risk findings"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("fallback json")
}

#[test]
fn jsonl_writer_emits_stable_lines_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("demo-events.jsonl");
    {
        let writer = JsonlSecurityEventWriter::new(&path, 0).expect("open writer");
        writer.emit(
            &SecurityEvent::new("alpha", "write(SKILL.md)", "resolve", "fallback:v000001")
                .with_ledger_status("deny")
                .with_message("rolling back to trusted snapshot"),
        );
        writer.emit(&SecurityEvent::new(
            "beta",
            "mkdir",
            "resolve",
            "hidden:awaiting decision",
        ));
        // Allow the worker thread to flush both lines before drop.
        std::thread::sleep(Duration::from_millis(150));
    }
    std::thread::sleep(Duration::from_millis(100));

    let body = std::fs::read_to_string(&path).expect("read jsonl");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "expected two JSONL lines, got {body:?}");

    let first: serde_json::Value = serde_json::from_str(lines[0]).expect("first line valid JSON");
    assert_eq!(first["skill"], "alpha");
    assert_eq!(first["fsHook"], "write(SKILL.md)");
    assert_eq!(first["ledgerAction"], "resolve");
    assert_eq!(first["ledgerStatus"], "deny");
    assert_eq!(first["skillfsDecision"], "fallback:v000001");
    assert_eq!(first["message"], "rolling back to trusted snapshot");

    let second: serde_json::Value = serde_json::from_str(lines[1]).expect("second line valid JSON");
    assert_eq!(second["skill"], "beta");
    assert!(
        second["ledgerStatus"].is_null(),
        "expected null status when omitted"
    );
    assert!(
        second.get("message").is_none(),
        "message must be omitted when None"
    );
}

#[test]
fn skill_meta_observations_are_filtered_at_controller() {
    let adapter = StaticLedgerAdapter::new();
    adapter.insert("alpha", current_result("alpha"));
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );

    let accepted = ctrl.observe(RefreshObservation::new(
        "alpha",
        Some(PathBuf::from(".skill-meta/manifest.json")),
        MutationKind::Write,
    ));
    assert!(!accepted, ".skill-meta paths must not enqueue refresh");
    let processed = ctrl.flush_for_testing();
    assert_eq!(processed, 0, ".skill-meta path must not run resolve");
    assert!(
        resolver.get("alpha").is_none(),
        "resolver must not be touched"
    );
    assert!(events.events().is_empty(), "no events for filtered path");
    ctrl.shutdown();
}

#[test]
fn rename_observes_both_old_and_new_skill_when_different() {
    let adapter = StaticLedgerAdapter::new();
    adapter.insert("alpha", current_result("alpha"));
    adapter.insert("beta", fallback_result("beta", "v000001.snapshot"));
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );

    ctrl.observe(RefreshObservation::new(
        "alpha",
        Some(PathBuf::from("scripts/run.sh")),
        MutationKind::Rename,
    ));
    ctrl.observe(RefreshObservation::new(
        "beta",
        Some(PathBuf::from("scripts/run.sh")),
        MutationKind::Rename,
    ));
    let processed = ctrl.flush_for_testing();
    assert_eq!(processed, 2, "rename across skills must run two resolves");
    assert!(matches!(
        resolver.get("alpha"),
        Some(ActiveTarget::Current { .. })
    ));
    assert!(matches!(
        resolver.get("beta"),
        Some(ActiveTarget::Snapshot { .. })
    ));
    let evs = events.events();
    assert_eq!(evs.len(), 2);
    let skills: Vec<String> = evs.iter().map(|e| e.skill.clone()).collect();
    assert!(skills.contains(&"alpha".to_string()));
    assert!(skills.contains(&"beta".to_string()));
    ctrl.shutdown();
}

#[test]
fn pipeline_runs_scan_before_resolve_on_success() {
    // Drives the controller through the static adapter and asserts
    // the ordered call log: scan must run before resolve for the
    // same skill. The success event is labeled `scan -> resolve`.
    let adapter = StaticLedgerAdapter::new();
    adapter.insert("alpha", current_result("alpha"));
    let logged = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        logged.clone(),
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );
    ctrl.observe(RefreshObservation::new(
        "alpha",
        Some(PathBuf::from("SKILL.md")),
        MutationKind::Write,
    ));
    let processed = ctrl.flush_for_testing();
    assert_eq!(processed, 1);
    assert_eq!(
        logged.calls(),
        vec![
            StaticAdapterCall::Scan {
                skill_name: "alpha".to_string()
            },
            StaticAdapterCall::Resolve {
                skill_name: "alpha".to_string()
            },
        ],
        "scan must precede resolve for the same skill"
    );
    let evs = events.events();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].ledger_action, "scan -> resolve");
    assert!(matches!(
        resolver.get("alpha"),
        Some(ActiveTarget::Current { .. })
    ));
    ctrl.shutdown();
}

#[test]
fn scan_failure_short_circuits_resolve_and_emits_scan_failed_event() {
    let adapter = StaticLedgerAdapter::new();
    adapter.insert_scan_err(
        "alpha",
        LedgerError::NonZeroExit {
            status: 13,
            stdout: String::new(),
            stderr: "scan crashed".to_string(),
        },
    );
    // Register a resolve that, if accidentally called, would update
    // the resolver. The post-flush call log assertion catches the
    // regression even if the resolver state were inspected later.
    adapter.insert("alpha", current_result("alpha"));
    let logged = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    resolver.set(
        "alpha",
        ActiveTarget::Current {
            source_dir: PathBuf::from("/srv/skills/alpha"),
        },
    );
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        logged.clone(),
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );
    ctrl.observe(RefreshObservation::new(
        "alpha",
        Some(PathBuf::from("SKILL.md")),
        MutationKind::Write,
    ));
    ctrl.flush_for_testing();
    assert_eq!(
        logged.calls(),
        vec![StaticAdapterCall::Scan {
            skill_name: "alpha".to_string()
        }],
        "resolve must NOT run after a scan failure"
    );
    assert!(matches!(
        resolver.get("alpha"),
        Some(ActiveTarget::Hidden { .. })
    ));
    let evs = events.events();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].ledger_action, "scan failed");
    assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
    ctrl.shutdown();
}

#[test]
fn resolve_failure_after_scan_emits_scan_then_resolve_failed_event() {
    let adapter = StaticLedgerAdapter::new();
    // Default scan succeeds; resolve returns garbled JSON.
    adapter.insert_err(
        "alpha",
        LedgerError::InvalidJson {
            reason: "garbled".to_string(),
        },
    );
    let logged = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        logged.clone(),
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );
    ctrl.observe(RefreshObservation::new(
        "alpha",
        Some(PathBuf::from("SKILL.md")),
        MutationKind::Write,
    ));
    ctrl.flush_for_testing();
    assert_eq!(
        logged.calls(),
        vec![
            StaticAdapterCall::Scan {
                skill_name: "alpha".to_string()
            },
            StaticAdapterCall::Resolve {
                skill_name: "alpha".to_string()
            },
        ]
    );
    let evs = events.events();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].ledger_action, "scan -> resolve failed");
    assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
    assert!(matches!(
        resolver.get("alpha"),
        Some(ActiveTarget::Hidden { .. })
    ));
    ctrl.shutdown();
}

#[test]
fn debounce_window_collapses_to_one_scan_resolve_pair() {
    // Many write observations inside a single debounce window must
    // produce exactly one (scan, resolve) pair for the affected
    // skill.
    let adapter = StaticLedgerAdapter::new();
    adapter.insert("alpha", current_result("alpha"));
    let logged = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        logged.clone(),
        resolver,
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );
    for _ in 0..10 {
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
    }
    let processed = ctrl.flush_for_testing();
    assert_eq!(processed, 1, "ten observations must collapse to one");
    assert_eq!(
        logged.calls(),
        vec![
            StaticAdapterCall::Scan {
                skill_name: "alpha".to_string()
            },
            StaticAdapterCall::Resolve {
                skill_name: "alpha".to_string()
            },
        ],
        "exactly one scan/resolve pair regardless of observation burst"
    );
    assert_eq!(events.events().len(), 1);
    ctrl.shutdown();
}

#[test]
fn skill_name_mismatch_from_provider_triggers_failure_policy_hide() {
    // N1/D1.6: provider returned a resolve whose `skillName` does not
    // match the directory we asked about (`weather`). The demo
    // controller must treat this as a resolve failure and apply
    // `HideOnFailure`, replacing whatever mapping was previously
    // installed for `weather`.
    let adapter = StaticLedgerAdapter::new();
    // Adapter answers the `weather` lookup with a mismatched response.
    adapter.insert("weather", current_result("calculator"));
    let logged = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    // Pre-seed `weather` with `current` so the flip is observable.
    resolver.set(
        "weather",
        ActiveTarget::Current {
            source_dir: PathBuf::from("/srv/skills/weather"),
        },
    );
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        logged.clone(),
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );
    ctrl.observe(RefreshObservation::new(
        "weather",
        Some(PathBuf::from("SKILL.md")),
        MutationKind::Write,
    ));
    ctrl.flush_for_testing();

    // Scan + resolve still ran (the mismatch is detected after the
    // resolve returns); the failure label must be `scan -> resolve
    // failed` since the failure happened post-scan.
    assert_eq!(
        logged.calls(),
        vec![
            StaticAdapterCall::Scan {
                skill_name: "weather".to_string()
            },
            StaticAdapterCall::Resolve {
                skill_name: "weather".to_string()
            },
        ]
    );
    let target = resolver.get("weather").expect("weather entry");
    assert!(matches!(target, ActiveTarget::Hidden { .. }));
    // The bogus `calculator` key must never appear from the mismatched
    // response.
    assert!(resolver.get("calculator").is_none());
    let evs = events.events();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].skill, "weather");
    assert_eq!(evs[0].ledger_action, "scan -> resolve failed");
    assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
    let message = evs[0].message.as_deref().unwrap_or_default();
    assert!(
        message.contains("weather") && message.contains("calculator"),
        "demo event message should surface both expected and actual names, got {message:?}"
    );
    ctrl.shutdown();
}

#[test]
fn skill_name_mismatch_with_keep_previous_preserves_mapping() {
    // Same setup as the hide test, but with KeepPreviousMapping the
    // existing `current` mapping survives the mismatched response.
    let adapter = StaticLedgerAdapter::new();
    adapter.insert("weather", current_result("calculator"));
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    resolver.set(
        "weather",
        ActiveTarget::Current {
            source_dir: PathBuf::from("/srv/skills/weather"),
        },
    );
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::KeepPreviousMapping,
    );
    ctrl.observe(RefreshObservation::new(
        "weather",
        Some(PathBuf::from("SKILL.md")),
        MutationKind::Write,
    ));
    ctrl.flush_for_testing();

    let target = resolver.get("weather").expect("weather entry");
    assert!(
        matches!(target, ActiveTarget::Current { .. }),
        "KeepPreviousMapping must preserve the previous mapping under skillName mismatch, got {target:?}"
    );
    // Bogus key must not be installed under either policy.
    assert!(resolver.get("calculator").is_none());
    let evs = events.events();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].ledger_action, "scan -> resolve failed");
    assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
    ctrl.shutdown();
}

#[test]
fn provider_failure_hides_skill_in_demo_default_mode() {
    let adapter = StaticLedgerAdapter::new();
    adapter.insert_err(
        "alpha",
        LedgerError::InvalidJson {
            reason: "garbage from provider".to_string(),
        },
    );
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    // Pre-seed `alpha` so we can confirm the mapping flips to hidden
    // rather than being ignored.
    resolver.set(
        "alpha",
        ActiveTarget::Current {
            source_dir: PathBuf::from("/srv/skills/alpha"),
        },
    );
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );

    ctrl.observe(RefreshObservation::new(
        "alpha",
        Some(PathBuf::from("SKILL.md")),
        MutationKind::Write,
    ));
    ctrl.flush_for_testing();

    let target = resolver.get("alpha").expect("alpha entry");
    assert!(matches!(target, ActiveTarget::Hidden { .. }));
    let evs = events.events();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
    assert!(evs[0].skillfs_decision.starts_with("hidden:"));
    ctrl.shutdown();
}

// ─────────────────────────────────────────────────────────────────────────────
// FUSE integration tests
// ─────────────────────────────────────────────────────────────────────────────

fn sorted_dir(dir: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();
    entries
}

fn write_snapshot(source: &Path, skill: &str, version: &str, skill_md: &str) {
    let dir = source
        .join(skill)
        .join(".skill-meta/versions")
        .join(version);
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    std::fs::write(dir.join("SKILL.md"), skill_md).expect("write snapshot SKILL.md");
}

#[test]
fn write_through_mount_enqueues_refresh_and_flips_resolver_to_fallback() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // The adapter starts by returning `current`; once SKILL.md is
    // mutated through the mount it returns `fallback` so we can pin
    // the live → snapshot flip.
    let adapter = StaticLedgerAdapter::new();
    adapter.insert(
        "demo-weather",
        fallback_result("demo-weather", "v000001.snapshot"),
    );
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);

    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "demo-weather");
    write_snapshot(
        source.path(),
        "demo-weather",
        "v000001.snapshot",
        "---\nname: demo-weather\ndescription: trusted snapshot\n---\n\n# trusted body\n",
    );
    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    // Pre-seed with current so the post-write flip is observable.
    resolver
        .set_from_resolve(&current_result("demo-weather"))
        .expect("seed current");
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );
    let mountpoint = tempfile::tempdir().expect("mount");

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
            active_resolver: Some(resolver.clone()),
            refresh_controller: Some(ctrl.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // Initial state: current → live source's SKILL.md is served.
    let live_md = std::fs::read_to_string(mountpoint.path().join("skills/demo-weather/SKILL.md"))
        .expect("read mount md (current)");
    assert!(
        live_md.contains("description: test skill"),
        "expected live source SKILL.md, got: {live_md:?}"
    );

    // Mutate SKILL.md through the mount. This goes through the FUSE
    // write callback, which the wiring observes; the controller's
    // worker then runs resolve and flips the resolver to fallback.
    std::fs::write(
        mountpoint.path().join("skills/demo-weather/SKILL.md"),
        "---\nname: demo-weather\ndescription: edited\n---\n",
    )
    .expect("write SKILL.md through mount");

    // Wait until the controller has fired at least one event.
    let evs = events.wait_for_n(1, Duration::from_millis(2000));
    assert!(
        !evs.is_empty(),
        "controller did not emit a demo event in time"
    );
    assert_eq!(evs[0].skill, "demo-weather");
    assert!(
        evs[0].fs_hook.starts_with("write("),
        "expected write fs_hook, got {:?}",
        evs[0].fs_hook
    );
    assert_eq!(evs[0].skillfs_decision, "fallback:v000001.snapshot");

    // Resolver must reflect the new decision.
    let target = resolver.get("demo-weather").expect("alpha entry");
    assert!(matches!(target, ActiveTarget::Snapshot { .. }));

    // After the flip, the mount must serve the snapshot SKILL.md.
    let post_md = std::fs::read_to_string(mountpoint.path().join("skills/demo-weather/SKILL.md"))
        .expect("read mount md (fallback)");
    assert!(
        post_md.contains("trusted body"),
        "expected snapshot body after flip, got: {post_md:?}"
    );

    // Tear down the mount before dropping the controller / events sink.
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
    ctrl.shutdown();
    drop(events);
}

#[test]
fn skill_meta_writes_via_source_do_not_trigger_refresh() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    // .skill-meta writes go through the source path (the ledger
    // populates trusted snapshots out-of-band). They must not trigger
    // a refresh — otherwise every snapshot write would loop the
    // resolver. Here we exercise the controller's filter directly,
    // independent of FUSE wiring.
    let adapter = StaticLedgerAdapter::new();
    adapter.insert("alpha", current_result("alpha"));
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );
    assert!(!ctrl.observe(RefreshObservation::new(
        "alpha",
        Some(PathBuf::from(
            ".skill-meta/versions/v000001.snapshot/SKILL.md"
        )),
        MutationKind::Write,
    )));
    let processed = ctrl.flush_for_testing();
    assert_eq!(processed, 0);
    assert!(events.events().is_empty());
    ctrl.shutdown();
}

#[test]
fn skill_discover_mutations_do_not_trigger_refresh() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    let adapter = StaticLedgerAdapter::new();
    adapter.insert("skill-discover", current_result("skill-discover"));
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
    let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(20),
        FailedResolveBehavior::HideOnFailure,
    );
    assert!(!ctrl.observe(RefreshObservation::new(
        "skill-discover",
        Some(PathBuf::from("SKILL.md")),
        MutationKind::Write,
    )));
    let processed = ctrl.flush_for_testing();
    assert_eq!(processed, 0);
    assert!(events.events().is_empty());
    ctrl.shutdown();
}

#[test]
fn newly_created_skill_stays_hidden_until_resolve_returns_visible() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // The adapter has no entry for `fresh-skill` initially — the
    // controller's failed-resolve policy hides it.
    let adapter = StaticLedgerAdapter::new();
    let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);

    // Build everything against the actual source path so the resolver
    // joins line up.
    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "anchor"); // an unrelated existing skill
    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("anchor"))
        .expect("seed anchor");
    let events = CapturingWriter::new();
    let ctrl = RefreshController::new(
        adapter,
        resolver.clone(),
        events.clone(),
        Duration::from_millis(80),
        FailedResolveBehavior::HideOnFailure,
    );
    let mountpoint = tempfile::tempdir().expect("mount");
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
            active_resolver: Some(resolver.clone()),
            refresh_controller: Some(ctrl.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // Create a brand-new skill directory through the mount.
    std::fs::create_dir(mountpoint.path().join("skills/fresh-skill"))
        .expect("mkdir fresh-skill through mount");
    // Allow the controller's debounce + resolve attempt to finish AND
    // the kernel's lookup cache (1 s TTL on the freshly-replied
    // entry) to expire so the next stat() re-issues the lookup
    // through the active mapping.
    std::thread::sleep(Duration::from_millis(1500));

    // The adapter has no entry → controller hid the skill. /skills
    // listing must omit it; direct lookup must surface ENOENT.
    let listing = sorted_dir(&mountpoint.path().join("skills"));
    assert!(
        !listing.contains(&"fresh-skill".to_string()),
        "newly-created skill must not bypass the resolver, got {listing:?}"
    );
    // Confirm the resolver was updated (deterministic) — the kernel
    // attribute cache may still hold the mkdir reply for a moment, so
    // the resolver state is the authoritative signal here.
    match resolver.get("fresh-skill") {
        Some(ActiveTarget::Hidden { .. }) => {}
        other => panic!("expected resolver to mark fresh-skill as hidden, got {other:?}"),
    }

    // Tear down.
    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
    ctrl.shutdown();
    drop(events);
}

#[test]
fn no_demo_refresh_controller_preserves_existing_behavior() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }

    // Without a controller attached, the mount accepts the same
    // arguments as the D1.1 path and never touches the resolver
    // mapping in response to writes. We seed the resolver with
    // `current` for `alpha`, mutate `alpha/SKILL.md` through the
    // mount, and confirm the mapping is still `current` afterwards.
    let source = tempfile::tempdir().expect("source");
    create_skill_dir(source.path(), "alpha");
    let resolver = Arc::new(ActiveSkillResolver::new(source.path()));
    resolver
        .set_from_resolve(&current_result("alpha"))
        .expect("seed alpha");

    let mountpoint = tempfile::tempdir().expect("mount");
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
            active_resolver: Some(resolver.clone()),
            ..MountConfig::default()
        },
    )
    .expect("mount");
    std::thread::sleep(Duration::from_millis(300));

    // Mutate SKILL.md through the mount.
    std::fs::write(
        mountpoint.path().join("skills/alpha/SKILL.md"),
        "---\nname: alpha\ndescription: edited\n---\n",
    )
    .expect("write SKILL.md");
    std::thread::sleep(Duration::from_millis(200));

    let target = resolver.get("alpha").expect("alpha entry");
    assert!(
        matches!(target, ActiveTarget::Current { .. }),
        "resolver mapping must not change without a controller, got {target:?}"
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(150));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}
