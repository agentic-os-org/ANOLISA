use std::collections::BTreeMap;

use super::*;
use crate::diagnostics::health::model::{HealthFindingCategory, HealthMessageId};

#[test]
fn unreadable_or_corrupt_health_history_does_not_reopen_invitations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .canonicalize()
        .unwrap()
        .join("health-suppression");
    std::fs::write(&path, "broken history\n").unwrap();
    let store = HealthSuppressionStore::load_from_path(&path);
    assert_eq!(
        store.should_show(
            "host",
            &finding("J06", HealthSeverity::Warning),
            &BTreeMap::new(),
            1000
        ),
        SuppressionDecision::Suppress
    );
}

#[test]
fn suppresses_same_session_and_recent_warning() {
    let mut store = HealthSuppressionStore::default();
    let finding = finding("J06", HealthSeverity::Warning);
    let metrics = metrics(&[("memory.available_ratio", 0.08)]);

    assert_eq!(
        store.should_show("host", &finding, &metrics, 1000),
        SuppressionDecision::Show
    );
    store.record_shown("host", &finding, metrics.clone(), 1000);

    assert_eq!(
        store.should_show("host", &finding, &metrics, 2000),
        SuppressionDecision::Suppress
    );
    let reloaded = HealthSuppressionStore {
        entries: store.entries.clone(),
        session_seen: HashSet::new(),
        unavailable: false,
    };
    assert_eq!(
        reloaded.should_show("host", &finding, &metrics, 2000),
        SuppressionDecision::Suppress
    );
}

#[test]
fn severity_upgrade_and_metric_worsening_bypass_ttl() {
    let mut store = HealthSuppressionStore::default();
    let warning = finding("J06", HealthSeverity::Warning);
    let critical = finding("J06", HealthSeverity::Critical);
    store.record_shown(
        "host",
        &warning,
        metrics(&[
            ("memory.available_mib", 1200.0),
            ("memory.available_ratio", 0.12),
            ("memory.used_ratio", 0.50),
        ]),
        1000,
    );
    let reloaded = HealthSuppressionStore {
        entries: store.entries.clone(),
        session_seen: HashSet::new(),
        unavailable: false,
    };

    assert_eq!(
        reloaded.should_show(
            "host",
            &critical,
            &metrics(&[
                ("memory.available_mib", 1200.0),
                ("memory.available_ratio", 0.12)
            ]),
            2000,
        ),
        SuppressionDecision::Show
    );
    assert_eq!(
        reloaded.should_show(
            "host",
            &warning,
            &metrics(&[
                ("memory.available_mib", 500.0),
                ("memory.available_ratio", 0.12)
            ]),
            2000,
        ),
        SuppressionDecision::Show
    );
    assert_eq!(
        reloaded.should_show(
            "host",
            &warning,
            &metrics(&[
                ("memory.available_mib", 1200.0),
                ("memory.available_ratio", 0.22)
            ]),
            2000,
        ),
        SuppressionDecision::Suppress
    );
    assert_eq!(
        reloaded.should_show(
            "host",
            &warning,
            &metrics(&[
                ("memory.available_mib", 1200.0),
                ("memory.available_ratio", 0.04)
            ]),
            2000,
        ),
        SuppressionDecision::Show
    );
    assert_eq!(
        reloaded.should_show(
            "host",
            &warning,
            &metrics(&[
                ("memory.available_mib", 1200.0),
                ("memory.available_ratio", 0.12),
                ("memory.used_ratio", 0.62)
            ]),
            2000,
        ),
        SuppressionDecision::Show
    );
}

#[test]
fn host_id_includes_machine_id_when_available() {
    let mut report = HealthScanReport::new("health", 0);
    report.host = Some("same-hostname".to_string());

    let without_machine = host_id_for_report(&report, None);
    let first_machine = host_id_for_report(&report, Some("machine-a"));
    let second_machine = host_id_for_report(&report, Some("machine-b"));

    assert_ne!(without_machine, first_machine);
    assert_ne!(first_machine, second_machine);
}

#[test]
fn expired_warning_shows_again() {
    let mut store = HealthSuppressionStore::default();
    let finding = finding("J10", HealthSeverity::Warning);
    let metrics = metrics(&[("filesystem.max_used_ratio", 0.90)]);
    store.record_shown("host", &finding, metrics.clone(), 1000);
    let reloaded = HealthSuppressionStore {
        entries: store.entries.clone(),
        session_seen: HashSet::new(),
        unavailable: false,
    };

    assert_eq!(
        reloaded.should_show("host", &finding, &metrics, 1000 + WARNING_TTL_MS + 1),
        SuppressionDecision::Show
    );
}

#[test]
fn corrupted_store_lines_are_ignored_and_write_errors_degrade() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let path = root.join("corrupted");
    std::fs::write(&path, "bad\nmissing\tparts\n").expect("write corrupted");

    let store = HealthSuppressionStore::load_from_path(&path);
    assert!(store.entries.is_empty());

    let dir_path = root.join("dir-target");
    std::fs::create_dir_all(&dir_path).expect("create dir");
    let err = store
        .write_to_path(&dir_path)
        .expect_err("write to dir fails");
    assert!(err.contains("write health suppression store failed"));

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&dir_path);
}

#[test]
fn persisted_mount_metric_keys_round_trip_without_poisoning_history() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().canonicalize().unwrap().join("history");
    let finding = finding("J06", HealthSeverity::Warning);
    let metrics = metrics(&[
        ("filesystem.mount./data,backup.used_ratio", 0.91),
        ("filesystem.mount./data=backup.available_gib", 12.5),
        ("filesystem.mount./data%2Cbackup.used_ratio", 0.82),
        ("filesystem.mount./数据.used_ratio", 0.73),
    ]);
    let mut store = HealthSuppressionStore::default();
    store
        .record_and_persist(&path, "host", &finding, metrics.clone(), 1000)
        .unwrap();
    let loaded = HealthSuppressionStore::load_from_path(&path);
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.entries[0].metrics, metrics);
    assert_eq!(
        loaded.should_show("host", &finding, &metrics, 1001),
        SuppressionDecision::Suppress
    );
    assert_eq!(
        loaded.should_show("host", &finding, &metrics, 1000 + WARNING_TTL_MS),
        SuppressionDecision::Show
    );
}

#[test]
fn metric_history_preserves_legacy_keys_and_rejects_corrupt_old_metrics() {
    let entry = parse_entry_line(
        "host\tJ06:Anomaly\twarning\t1000\tmemory.available_ratio=0.08,filesystem.mount./data%2Cbackup.used_ratio=0.9,filesystem.mount./数据.used_ratio=0.7",
    )
    .unwrap();
    assert_eq!(
        entry.metrics,
        metrics(&[
            ("memory.available_ratio", 0.08),
            ("filesystem.mount./data%2Cbackup.used_ratio", 0.9),
            ("filesystem.mount./数据.used_ratio", 0.7),
        ])
    );
    for invalid in [
        "memory.available_ratio=0.08,broken",
        "memory.available_ratio=NaN",
    ] {
        assert!(
            parse_entry_line(&format!("host\tJ06:Anomaly\twarning\t1000\t{invalid}")).is_none()
        );
    }
}

#[test]
fn persists_only_bounded_metadata() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().canonicalize().unwrap().join("persist");
    let mut store = HealthSuppressionStore::default();
    let finding = finding("J11", HealthSeverity::Critical);
    store
        .record_and_persist(
            &path,
            "host",
            &finding,
            metrics(&[("kernel.oom_latest_age_seconds", 120.0)]),
            1000,
        )
        .expect("persist");

    let content = std::fs::read_to_string(&path).expect("read store");
    assert!(content.contains("kernel.oom_latest_age_seconds"));
    assert!(!content.contains("journalctl"));
    assert!(!content.contains("/tmp/"));
    let loaded = HealthSuppressionStore::load_from_path(&path);
    assert_eq!(loaded.entries.len(), 1);

    let _ = std::fs::remove_file(path);
}

fn finding(id: &str, severity: HealthSeverity) -> HealthFinding {
    HealthFinding {
        id: id.to_string(),
        severity,
        category: HealthFindingCategory::Anomaly,
        title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids: Vec::new(),
        suggested_try_ids: Vec::new(),
    }
}

fn metrics(items: &[(&str, f64)]) -> BTreeMap<String, f64> {
    items
        .iter()
        .map(|(key, value)| ((*key).to_string(), *value))
        .collect()
}
