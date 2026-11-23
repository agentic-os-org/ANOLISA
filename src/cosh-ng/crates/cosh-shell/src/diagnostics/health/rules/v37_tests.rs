use super::*;
use crate::diagnostics::health::model::{HealthFactCategory, HealthFactSource, HealthScanReport};

fn fact(key: &str, value: HealthFactValue) -> HealthFact {
    HealthFact {
        id: format!("id-{key}"),
        key: key.to_string(),
        category: HealthFactCategory::Disk,
        value,
        unit: None,
        source: HealthFactSource::Statfs,
        elapsed_ms: 0,
    }
}

fn report_with(facts: Vec<HealthFact>) -> HealthScanReport {
    let mut report = HealthScanReport::new("v37-test", 0);
    report.facts = facts;
    report
}

fn volume_facts(used: u64, bavail: u64, files: u64, favail: u64) -> Vec<HealthFact> {
    vec![
        fact(
            "filesystem.mount./.volume_proven",
            HealthFactValue::Bool(true),
        ),
        fact(
            "filesystem.mount./.volume_used_bytes",
            HealthFactValue::Unsigned(used),
        ),
        fact(
            "filesystem.mount./.volume_bavail_bytes",
            HealthFactValue::Unsigned(bavail),
        ),
        fact(
            "filesystem.mount./.volume_files",
            HealthFactValue::Unsigned(files),
        ),
        fact(
            "filesystem.mount./.volume_favail",
            HealthFactValue::Unsigned(favail),
        ),
        fact(
            "filesystem.mount./.volume_inode_applicable",
            HealthFactValue::Bool(true),
        ),
        fact(
            "filesystem.mount./.volume_readonly",
            HealthFactValue::Bool(false),
        ),
        fact(
            "filesystem.mount./.volume_fs_type",
            HealthFactValue::String("ext4".into()),
        ),
        fact(
            "filesystem.mount./.volume_device",
            HealthFactValue::String("253:0".into()),
        ),
        fact(
            "filesystem.mount./.volume_fsid",
            HealthFactValue::String("1:2".into()),
        ),
    ]
}

fn ids(report: &HealthScanReport) -> Vec<String> {
    evaluate_judgement_rules(report, &crate::config::HealthConfig::default())
        .into_iter()
        .map(|finding| finding.id)
        .collect()
}

const GIB: u64 = 1024 * 1024 * 1024;

#[test]
fn zero_available_bytes_is_critical_even_with_healthy_inodes() {
    let report = report_with(volume_facts(99 * GIB, 0, 100000, 50000));
    assert_eq!(ids(&report), vec!["J09"]);
}

#[test]
fn warning_only_volume_stays_silent() {
    let report = report_with(volume_facts(91 * GIB, 9 * GIB, 100000, 50000));
    assert!(ids(&report).is_empty());
}

#[test]
fn near_full_with_huge_free_stays_silent() {
    let report = report_with(volume_facts(96 * GIB, 100 * GIB, 100000, 50000));
    assert!(ids(&report).is_empty());
}

#[test]
fn unknown_volume_suppresses_legacy_df_card() {
    let facts = vec![
        fact(
            "filesystem.mount./.volume_proven",
            HealthFactValue::Bool(false),
        ),
        fact(
            "filesystem.mount./.volume_unknown_reason",
            HealthFactValue::String("resolve_failed".into()),
        ),
        fact(
            "filesystem.mount./.path",
            HealthFactValue::String("/".into()),
        ),
        fact(
            "filesystem.mount./.used_ratio",
            HealthFactValue::Float(0.99),
        ),
        fact(
            "filesystem.mount./.available_gib",
            HealthFactValue::Float(0.1),
        ),
    ];
    let report = report_with(facts);
    assert!(ids(&report).is_empty());
}

#[test]
fn legacy_df_path_unchanged_without_volume_facts() {
    let report = report_with(vec![
        fact(
            "filesystem.mount./.path",
            HealthFactValue::String("/".into()),
        ),
        fact(
            "filesystem.mount./.used_ratio",
            HealthFactValue::Float(0.96),
        ),
        fact(
            "filesystem.mount./.available_gib",
            HealthFactValue::Float(1.0),
        ),
    ]);
    assert_eq!(ids(&report), vec!["J09"]);
}

#[test]
fn inode_exhaustion_with_ample_bytes_uses_j17() {
    let report = report_with(volume_facts(50 * GIB, 50 * GIB, 10000, 0));
    assert_eq!(ids(&report), vec!["J17"]);
}

#[test]
fn byte_and_inode_exhaustion_merge_into_one_card() {
    let report = report_with(volume_facts(99 * GIB, 0, 10000, 0));
    let findings = evaluate_judgement_rules(&report, &crate::config::HealthConfig::default());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J09");
    assert_eq!(
        findings[0].detail_args.get("members").unwrap(),
        "bytes+inode"
    );
}

fn oom_facts(age: Option<u64>, confidence: &str) -> Vec<HealthFact> {
    let mut facts = vec![
        fact(
            "kernel.current_cgroup_version",
            HealthFactValue::Unsigned(2),
        ),
        fact(
            "kernel.current_cgroup_path",
            HealthFactValue::String("/user.slice/session-2.scope".into()),
        ),
        fact(
            "kernel.oom_attributed_confidence",
            HealthFactValue::String(confidence.into()),
        ),
    ];
    if let Some(age) = age {
        facts.push(fact(
            "kernel.oom_attributed_age_seconds",
            HealthFactValue::Unsigned(age),
        ));
        facts.push(fact(
            "kernel.oom_attributed_pid",
            HealthFactValue::Unsigned(123),
        ));
        facts.push(fact(
            "kernel.oom_attributed_event_identity",
            HealthFactValue::String("uptime-ns:100".into()),
        ));
        facts.push(fact(
            "kernel.oom_attributed_scope",
            HealthFactValue::String("current-cgroup".into()),
        ));
    }
    facts
}

#[test]
fn attributed_oom_age_tiers_are_300_and_3600() {
    assert_eq!(ids(&report_with(oom_facts(Some(300), "high"))), vec!["J11"]);
    assert_eq!(ids(&report_with(oom_facts(Some(301), "high"))), vec!["J12"]);
    assert_eq!(
        ids(&report_with(oom_facts(Some(3600), "high"))),
        vec!["J12"]
    );
    assert!(ids(&report_with(oom_facts(Some(3601), "high"))).is_empty());
    assert!(ids(&report_with(oom_facts(Some(100), "low"))).is_empty());
    assert!(ids(&report_with(oom_facts(None, "high"))).is_empty());
}

#[test]
fn counter_increment_tiers_use_window_span() {
    let mut facts = vec![
        fact(
            "kernel.current_cgroup_version",
            HealthFactValue::Unsigned(2),
        ),
        fact(
            "kernel.cgroup_oom_kill_increment",
            HealthFactValue::Unsigned(2),
        ),
        fact(
            "kernel.cgroup_oom_kill_window_start_epoch",
            HealthFactValue::Unsigned(1000),
        ),
        fact(
            "kernel.cgroup_oom_kill_window_end_epoch",
            HealthFactValue::Unsigned(1200),
        ),
    ];
    assert_eq!(ids(&report_with(facts.clone())), vec!["J11"]);
    facts.retain(|f| f.key != "kernel.cgroup_oom_kill_window_end_epoch");
    facts.push(fact(
        "kernel.cgroup_oom_kill_window_end_epoch",
        HealthFactValue::Unsigned(4700),
    ));
    assert!(ids(&report_with(facts)).is_empty());
}

#[test]
fn legacy_oom_tiers_remain_without_cgroup_collection() {
    let mut facts = vec![fact(
        "kernel.oom_latest_age_seconds",
        HealthFactValue::Unsigned(100),
    )];
    assert_eq!(ids(&report_with(facts.clone())), vec!["J11"]);
    facts = vec![
        fact(
            "kernel.oom_latest_age_seconds",
            HealthFactValue::Unsigned(80000),
        ),
        fact("memory.available_mib", HealthFactValue::Float(100.0)),
        fact("memory.available_ratio", HealthFactValue::Float(0.01)),
    ];
    assert_eq!(ids(&report_with(facts)), vec!["J05", "J13"]);
}

fn pressure_notice_facts() -> Vec<HealthFact> {
    vec![
        fact(
            "host.pressure_memory_full_avg10",
            HealthFactValue::Float(6.0),
        ),
        fact(
            "host.pressure_memory_full_avg60",
            HealthFactValue::Float(2.0),
        ),
        fact(
            "kernel.current_cgroup_id",
            HealthFactValue::String("1:2".into()),
        ),
        fact(
            "kernel.cgroup_memory_object_id",
            HealthFactValue::String("1:2".into()),
        ),
        fact(
            "kernel.cgroup_psi_object_id",
            HealthFactValue::String("1:2".into()),
        ),
        fact(
            "kernel.cgroup_memory_current_bytes",
            HealthFactValue::Unsigned(96 * 1024 * 1024),
        ),
        fact(
            "kernel.cgroup_memory_max_bytes",
            HealthFactValue::Unsigned(100 * 1024 * 1024),
        ),
        fact(
            "kernel.cgroup_psi_memory_full_avg10",
            HealthFactValue::Float(25.0),
        ),
        fact(
            "kernel.cgroup_psi_memory_full_avg60",
            HealthFactValue::Float(12.0),
        ),
    ]
}

fn host_qualified_report(facts: Vec<HealthFact>) -> HealthScanReport {
    let mut report = report_with(facts);
    report
        .check_results
        .push(crate::diagnostics::health::model::HealthCheckResult {
            id: "host.uptime".into(),
            collector: Some(crate::diagnostics::health::model::HealthCollector::Host),
            status: crate::diagnostics::health::model::HealthCheckStatus::Ok,
            fact_ids: Vec::new(),
            gaps: Vec::new(),
            elapsed_ms: 0,
        });
    report
}

#[test]
fn pressure_notices_default_off_keeps_findings_silent() {
    let report = host_qualified_report(pressure_notice_facts());
    let findings = evaluate_judgement_rules(&report, &crate::config::HealthConfig::default());
    assert!(findings.is_empty());
}

#[test]
fn pressure_notices_enabled_emits_combo_and_psi_warnings() {
    let report = host_qualified_report(pressure_notice_facts());
    let config = crate::config::HealthConfig {
        pressure_notices: true,
        ..Default::default()
    };
    let mut ids: Vec<String> = evaluate_judgement_rules(&report, &config)
        .into_iter()
        .map(|finding| finding.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["J19", "J20", "J20:cgroup"]);
    for finding in evaluate_judgement_rules(&report, &config) {
        assert_eq!(finding.severity, HealthSeverity::Warning);
    }
}

#[test]
fn unqualified_host_keeps_cgroup_combo_but_drops_host_psi() {
    let report = report_with(pressure_notice_facts());
    let config = crate::config::HealthConfig {
        pressure_notices: true,
        ..Default::default()
    };
    let ids: Vec<String> = evaluate_judgement_rules(&report, &config)
        .into_iter()
        .map(|finding| finding.id)
        .collect();
    assert_eq!(ids, vec!["J19", "J20:cgroup"]);
}

fn conntrack_facts(count: u64, max: u64) -> Vec<HealthFact> {
    vec![
        fact("host.conntrack_count", HealthFactValue::Unsigned(count)),
        fact("host.conntrack_max", HealthFactValue::Unsigned(max)),
        fact(
            "host.conntrack_netns_id",
            HealthFactValue::String("4:5".into()),
        ),
    ]
}

#[test]
fn network_capacity_notices_gate_conntrack_findings() {
    let report = host_qualified_report(conntrack_facts(999_000, 1_000_000));
    assert!(evaluate_judgement_rules(&report, &crate::config::HealthConfig::default()).is_empty());
    let config = crate::config::HealthConfig {
        network_capacity_notices: true,
        ..Default::default()
    };
    let findings = evaluate_judgement_rules(&report, &config);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].id, "J23");
    assert_eq!(findings[0].severity, HealthSeverity::Warning);
    let report90 = host_qualified_report(conntrack_facts(900_000, 1_000_000));
    assert!(evaluate_judgement_rules(&report90, &config).is_empty());
    let unqualified = report_with(conntrack_facts(999_000, 1_000_000));
    assert!(evaluate_judgement_rules(&unqualified, &config).is_empty());
}

#[test]
fn file_nr_and_capacity_events_stay_diagnostics_only() {
    let facts = vec![
        fact("host.file_nr_allocated", HealthFactValue::Unsigned(1000)),
        fact("host.file_nr_unused", HealthFactValue::Unsigned(0)),
        fact("host.file_nr_max", HealthFactValue::Unsigned(1000)),
        fact(
            "host.file_nr_level",
            HealthFactValue::String("critical".into()),
        ),
        fact(
            "kernel.vfs_filemax_latest_age_seconds",
            HealthFactValue::Unsigned(100),
        ),
        fact(
            "kernel.vfs_filemax_event_identity",
            HealthFactValue::String("uptime-ns:1".into()),
        ),
        fact(
            "kernel.conntrack_full_latest_age_seconds",
            HealthFactValue::Unsigned(100),
        ),
    ];
    let report = host_qualified_report(facts);
    let config = crate::config::HealthConfig {
        network_capacity_notices: true,
        pressure_notices: true,
        ..Default::default()
    };
    assert!(evaluate_judgement_rules(&report, &config).is_empty());
}

library_tests! {
fn set_readonly(facts: &mut [HealthFact], readonly: bool) {
    for fact in facts.iter_mut() {
        if fact.key == "filesystem.mount./.volume_readonly" {
            fact.value = HealthFactValue::Bool(readonly);
        }
    }
}

#[test]
fn readonly_volume_with_critical_stats_stays_silent() {
    let mut facts = volume_facts(99 * GIB, 0, 100000, 50000);
    set_readonly(&mut facts, true);
    let report = report_with(facts);
    assert!(ids(&report).is_empty());
}

#[test]
fn readonly_view_holds_existing_capacity_member() {
    let rw = volume_facts(99 * GIB, 0, 100000, 50000);
    let judged = super::volume::evaluate_volumes(&rw);
    assert_eq!(
        judged[0].bytes.entry,
        Some(super::volume::MemberLevel::Critical)
    );
    let mut ro = rw.clone();
    set_readonly(&mut ro, true);
    let held = super::volume::evaluate_volumes(&ro);
    assert_eq!(held[0].bytes.entry, None);
    assert!(!held[0].bytes.exit);
    assert!(held[0].bytes.valid);
    assert_eq!(held[0].inodes.entry, None);
    assert!(!held[0].inodes.exit);
}
}
