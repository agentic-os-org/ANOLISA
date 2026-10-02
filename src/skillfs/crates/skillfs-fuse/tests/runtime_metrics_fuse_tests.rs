//! FUSE integration tests for real-time runtime metric delta events.
//!
//! A `RuntimeMetricsSink` is threaded into the mount via `MountConfig` and
//! pointed at a pre-created temp JSONL file. Reading a mounted skill through the
//! FUSE layer must emit `skill_hit` and — when an active resolver made a real
//! security decision — the corresponding `policy_*` delta record.
//!
//! Gated with `fuse_available()`: skips cleanly when `/dev/fuse` / `fusermount3`
//! are absent.

#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    ActiveSkillResolver, LedgerResolveResult, RuntimeMetricsSink, RuntimeMetricsWriter,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use crate::common::{create_skill_dir, fuse_available, telemetry_enabled};

/// Normal-mode mount that injects a `RuntimeMetricsSink` (and optionally an
/// `ActiveSkillResolver`) and points the metrics writer at `metrics_path`.
struct MetricsMount {
    // Held only to keep the temp source dir alive for the mount's lifetime.
    _source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl MetricsMount {
    fn new<S, R>(metrics_path: &Path, seed: S, resolver_builder: R) -> Self
    where
        S: FnOnce(&Path),
        R: FnOnce(&Path) -> Option<Arc<ActiveSkillResolver>>,
    {
        // Source and mountpoint under /tmp so FUSE has a real backing dir.
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let resolver = resolver_builder(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let sink = Arc::new(RuntimeMetricsSink::new(
            RuntimeMetricsWriter::new(metrics_path),
            "test-session".to_string(),
            "agent".to_string(),
        ));

        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            shared,
            MountOptions::default(),
            false, // normal mode
            MountConfig {
                active_resolver: resolver,
                runtime_metrics: Some(sink),
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));

        Self {
            _source: source,
            mountpoint,
            handle: Some(handle),
        }
    }

    fn skills_dir(&self) -> PathBuf {
        self.mountpoint.path().join("skills")
    }

    fn skill_md(&self, name: &str) -> PathBuf {
        self.skills_dir().join(name).join("SKILL.md")
    }
}

impl Drop for MetricsMount {
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

/// Pre-create the deployment-owned metrics file so the writer appends to it.
fn make_metrics_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("metrics tempdir");
    let path = dir.path().join("skillfs.jsonl");
    std::fs::File::create(&path).expect("pre-create metrics file");
    (dir, path)
}

fn read_records(path: &Path) -> Vec<serde_json::Value> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON record"))
        .collect()
}

fn events_named<'a>(records: &'a [serde_json::Value], name: &str) -> Vec<&'a serde_json::Value> {
    records
        .iter()
        .filter(|r| r["record_type"] == "runtime_metric" && r["event_name"] == name)
        .collect()
}

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

fn hidden_result(skill: &str) -> LedgerResolveResult {
    let json = format!(
        r#"{{
            "schemaVersion": 1,
            "skillName": "{skill}",
            "status": "deny",
            "decision": "hidden",
            "reason": "no trusted version available"
        }}"#
    );
    LedgerResolveResult::from_json_str(&json).expect("hidden json")
}

fn write_snapshot(source: &Path, skill: &str, version: &str, skill_md: &str) {
    let dir = source
        .join(skill)
        .join(".skill-meta/versions")
        .join(version);
    std::fs::create_dir_all(&dir).expect("create snapshot dir");
    std::fs::write(dir.join("SKILL.md"), skill_md).expect("write snapshot SKILL.md");
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn skill_open_emits_skill_hit_without_resolver_but_no_policy() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    if !telemetry_enabled() {
        eprintln!("SKIP: telemetry disabled on host (/etc/anolisa/.telemetry_disabled present)");
        return;
    }

    let (_dir, metrics) = make_metrics_file();
    let mount = MetricsMount::new(
        &metrics,
        |src| create_skill_dir(src, "weather"),
        |_src| None, // no resolver → plain passthrough, not a security decision
    );

    let _ = std::fs::read_to_string(mount.skill_md("weather")).expect("read skill md");
    std::thread::sleep(Duration::from_millis(100));

    let records = read_records(&metrics);
    assert!(
        !events_named(&records, "skill_hit").is_empty(),
        "expected a skill_hit record, got {records:?}"
    );
    assert!(
        events_named(&records, "policy_allow").is_empty(),
        "plain passthrough (no resolver) must NOT emit policy_allow, got {records:?}"
    );
    let hit = events_named(&records, "skill_hit")[0];
    assert_eq!(hit["skill_hit_times"], 1);
    assert_eq!(hit["component.name"], "skillfs");
    assert_eq!(hit["session_id"], "test-session");
}

#[test]
fn current_skill_open_emits_policy_allow() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    if !telemetry_enabled() {
        eprintln!("SKIP: telemetry disabled on host (/etc/anolisa/.telemetry_disabled present)");
        return;
    }

    let (_dir, metrics) = make_metrics_file();
    let mount = MetricsMount::new(
        &metrics,
        |src| create_skill_dir(src, "weather"),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&current_result("weather")).unwrap();
            Some(Arc::new(r))
        },
    );

    let _ = std::fs::read_to_string(mount.skill_md("weather")).expect("read skill md");
    std::thread::sleep(Duration::from_millis(100));

    let records = read_records(&metrics);
    assert!(
        !events_named(&records, "skill_hit").is_empty(),
        "expected skill_hit, got {records:?}"
    );
    assert!(
        !events_named(&records, "policy_allow").is_empty(),
        "resolver Current decision must emit policy_allow, got {records:?}"
    );
    assert_eq!(
        events_named(&records, "policy_allow")[0]["policy_allow_times"],
        1
    );
}

#[test]
fn snapshot_skill_open_emits_policy_fallback() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    if !telemetry_enabled() {
        eprintln!("SKIP: telemetry disabled on host (/etc/anolisa/.telemetry_disabled present)");
        return;
    }

    let (_dir, metrics) = make_metrics_file();
    let snapshot_md = "---\nname: weather\ndescription: trusted snapshot\n---\n\n# body\n";
    let mount = MetricsMount::new(
        &metrics,
        |src| {
            create_skill_dir(src, "weather");
            write_snapshot(src, "weather", "v000001.snapshot", snapshot_md);
        },
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&fallback_result("weather", "v000001.snapshot"))
                .unwrap();
            Some(Arc::new(r))
        },
    );

    let served = std::fs::read_to_string(mount.skill_md("weather")).expect("read snapshot md");
    assert!(
        served.contains("trusted snapshot"),
        "expected snapshot serve"
    );
    std::thread::sleep(Duration::from_millis(100));

    let records = read_records(&metrics);
    assert!(
        !events_named(&records, "policy_fallback").is_empty(),
        "resolver Snapshot decision must emit policy_fallback, got {records:?}"
    );
    assert_eq!(
        events_named(&records, "policy_fallback")[0]["policy_fallback_times"],
        1
    );
}

#[test]
fn hidden_skill_open_emits_policy_denied() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE not available");
        return;
    }
    if !telemetry_enabled() {
        eprintln!("SKIP: telemetry disabled on host (/etc/anolisa/.telemetry_disabled present)");
        return;
    }

    let (_dir, metrics) = make_metrics_file();
    let mount = MetricsMount::new(
        &metrics,
        |src| create_skill_dir(src, "weather"),
        |src_root| {
            let r = ActiveSkillResolver::new(src_root.to_path_buf());
            r.set_from_resolve(&hidden_result("weather")).unwrap();
            Some(Arc::new(r))
        },
    );

    // A hidden skill's SKILL.md open must fail (ENOENT) and record a denial.
    let _ = std::fs::read_to_string(mount.skill_md("weather"));
    std::thread::sleep(Duration::from_millis(100));

    let records = read_records(&metrics);
    assert!(
        !events_named(&records, "policy_denied").is_empty(),
        "resolver Hidden decision must emit policy_denied, got {records:?}"
    );
    assert_eq!(
        events_named(&records, "policy_denied")[0]["policy_denied_times"],
        1
    );
}
