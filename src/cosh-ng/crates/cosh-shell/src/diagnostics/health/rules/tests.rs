use crate::config::{HealthServiceConfig, HealthServiceExpectedState};

use super::*;

#[test]
fn bounded_negative_oom_evidence_does_not_complete_swap_context() {
    use crate::diagnostics::health::model::{HealthFactCoverage, HealthFactScope};
    use crate::diagnostics::health::HealthReportBuilder;
    let mut builder = HealthReportBuilder::for_started_at(1);
    builder.start_check_id(
        "kernel.oom",
        HealthCollector::KernelSignal,
        HealthFactScope::Host,
        HealthFactCoverage::ReadWindow {
            since_ms: None,
            until_ms: Some(100),
            max_records: 200,
            complete: false,
        },
        1,
    );
    builder
        .add_fact(
            HealthFactCategory::Kernel,
            "kernel.log_source",
            HealthFactValue::String("journalctl".into()),
            None,
            HealthFactSource::JournalctlK,
            0,
        )
        .add_check_done("kernel.oom")
        .complete_check();
    builder
        .add_fact(
            HealthFactCategory::Memory,
            "memory.swap_used_ratio",
            HealthFactValue::Float(0.8),
            None,
            HealthFactSource::Fixture,
            0,
        )
        .add_fact(
            HealthFactCategory::Memory,
            "memory.swap_used_mib",
            HealthFactValue::Unsigned(2048),
            None,
            HealthFactSource::Fixture,
            0,
        )
        .add_fact(
            HealthFactCategory::Memory,
            "memory.swap_memory_pressure",
            HealthFactValue::Bool(false),
            None,
            HealthFactSource::Fixture,
            0,
        );
    assert!(!swap_context_complete(
        &builder.finish(2),
        &HealthConfig::default()
    ));
}

#[test]
fn disk_findings_reference_the_specific_mount() {
    let disk = crate::diagnostics::health::collectors::parse_df_p("Filesystem Type 1024-blocks Used Available Capacity Mounted on\n/dev/a xfs 100000000 20000000 80000000 20% /\n/dev/b xfs 100000000 97000000 3000000 97% /data\n", &["/".into()]).unwrap();
    let mut builder = crate::diagnostics::health::builder::HealthReportBuilder::for_started_at(1);
    disk.record(&mut builder, 0);
    let mut report = builder.finish(2);
    apply_judgement_rules(&mut report, &HealthConfig::default());
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.id == "J09:/data")
        .expect("extra mount has its own finding");
    assert_eq!(
        finding.detail_args.get("mount").map(String::as_str),
        Some("/data")
    );
    assert!(finding
        .evidence_fact_ids
        .iter()
        .all(|id| id.starts_with("filesystem.mount./data.")));
    assert!(!report.findings.iter().any(|finding| finding.id == "J09"));
}
use crate::diagnostics::health::model::{
    HealthFactCategory, HealthFactSource, UnavailableCollector,
};

#[test]
fn healthy_and_low_confidence_signals_do_not_create_findings() {
    let mut report = report_with_facts(vec![
        float_fact("cpu.load_per_core_1m", 3.0),
        float_fact("cpu.load_per_core_5m", 0.4),
        float_fact("memory.available_mib", 32768.0),
        float_fact("memory.available_ratio", 0.40),
        float_fact("memory.swap_used_ratio", 0.55),
        float_fact("memory.swap_used_mib", 4096.0),
        float_fact("filesystem.max_used_ratio", 0.80),
        float_fact("filesystem.available_gib", 1024.0),
        string_fact("filesystem.riskiest_mount", "/data"),
    ]);

    apply_judgement_rules(&mut report, &HealthConfig::default());

    assert!(report.findings.is_empty());
    assert_eq!(report.overall_severity, HealthSeverity::Ok);
}

#[test]
fn core_collector_unavailable_degrades_report_without_claiming_ok() {
    let mut report = HealthScanReport::new("health-1", 0);
    report.unavailable.push(UnavailableCollector {
        collector: HealthCollector::Memory,
        reason: HealthUnavailableReason::Timeout,
        severity: HealthSeverity::Unavailable,
        elapsed_ms: 100,
    });

    apply_judgement_rules(&mut report, &HealthConfig::default());

    assert_eq!(report.findings[0].id, "J02");
    assert_eq!(report.findings[0].severity, HealthSeverity::Degraded);
    assert_eq!(report.overall_severity, HealthSeverity::Degraded);
}

#[test]
fn unsupported_platform_is_unavailable() {
    let mut report = HealthScanReport::new("health-1", 0);
    report.unavailable.push(UnavailableCollector {
        collector: HealthCollector::Host,
        reason: HealthUnavailableReason::Unsupported,
        severity: HealthSeverity::Unavailable,
        elapsed_ms: 1,
    });

    apply_judgement_rules(&mut report, &HealthConfig::default());

    assert_eq!(report.findings[0].id, "J01");
    assert_eq!(report.findings[0].severity, HealthSeverity::Unavailable);
    assert_eq!(report.overall_severity, HealthSeverity::Unavailable);
}

#[test]
fn critical_load_and_recent_oom_sort_before_warnings() {
    let mut report = report_with_facts(vec![
        float_fact("cpu.load_per_core_1m", 4.0),
        float_fact("cpu.load_per_core_5m", 2.0),
        float_fact("kernel.oom_latest_age_seconds", 120.0),
        float_fact("filesystem.max_used_ratio", 0.90),
        float_fact("filesystem.available_gib", 30.0),
    ]);

    apply_judgement_rules(&mut report, &HealthConfig::default());

    let ids = finding_ids(&report);
    assert_eq!(ids[0], "J03");
    assert_eq!(ids[1], "J11");
    assert!(ids.contains(&"J10".to_string()));
    assert_eq!(report.overall_severity, HealthSeverity::Critical);
}

#[test]
fn warning_disk_requires_high_ratio_or_critical_mount_with_low_space() {
    let mut report = report_with_facts(vec![
        float_fact("filesystem.max_used_ratio", 0.86),
        float_fact("filesystem.available_gib", 8.0),
        string_fact("filesystem.riskiest_mount", "/"),
    ]);

    apply_judgement_rules(&mut report, &HealthConfig::default());

    assert_eq!(report.findings[0].id, "J10");
    assert_eq!(report.findings[0].severity, HealthSeverity::Warning);

    let config = HealthConfig::default();
    for (mount, ratio, available) in [
        ("/boot/efi", 0.04, 0.189),
        ("/sys/firmware/efi/efivars", 0.03, 0.0002),
        ("/small-data", 0.40, 0.8),
    ] {
        assert_eq!(
            disk_severity(Some(ratio), Some(available), mount, &config),
            None
        );
    }

    let mut config = HealthConfig::default();
    assert_eq!(
        disk_severity(Some(0.96), Some(0.189), "/boot/efi", &config),
        Some(HealthSeverity::Critical)
    );
    config.critical_mounts.push("/boot/efi".into());
    assert_eq!(
        disk_severity(Some(0.04), Some(0.189), "/boot/efi", &config),
        Some(HealthSeverity::Critical)
    );

    let config = HealthConfig {
        critical_mounts: vec!["/data".into()],
        ..HealthConfig::default()
    };
    assert_eq!(
        disk_severity(Some(0.04), Some(1.0), "/", &config),
        Some(HealthSeverity::Critical)
    );
}

#[test]
fn swap_history_without_memory_or_oom_context_is_suppressed() {
    let mut report = report_with_facts(vec![
        float_fact("memory.available_mib", 16384.0),
        float_fact("memory.available_ratio", 0.30),
        float_fact("memory.swap_used_ratio", 0.90),
        float_fact("memory.swap_used_mib", 4096.0),
    ]);

    apply_judgement_rules(&mut report, &HealthConfig::default());

    assert!(report.findings.is_empty());
}

#[test]
fn memory_low_closes_swap_pressure_context() {
    let mut report = report_with_facts(vec![
        float_fact("memory.available_mib", 700.0),
        float_fact("memory.available_ratio", 0.08),
        float_fact("memory.swap_used_ratio", 0.55),
        float_fact("memory.swap_used_mib", 4096.0),
    ]);

    apply_judgement_rules(&mut report, &HealthConfig::default());

    let ids = finding_ids(&report);
    assert!(ids.contains(&"J06".to_string()));
    assert!(ids.contains(&"J08".to_string()));
}

#[test]
fn configured_service_inactive_only_warns_when_expected_active() {
    let mut report = report_with_facts(vec![string_fact("service.redis.status", "inactive")]);
    let mut config = HealthConfig::default();
    config.services.push(HealthServiceConfig {
        name: "redis".to_string(),
        expected: HealthServiceExpectedState::Inactive,
    });

    apply_judgement_rules(&mut report, &config);
    assert!(report.findings.is_empty());

    config.services[0].expected = HealthServiceExpectedState::Active;
    apply_judgement_rules(&mut report, &config);
    assert_eq!(report.findings[0].id, "J16:redis");
}

#[test]
fn configured_service_failed_records_unit_and_status_detail() {
    let mut report = report_with_facts(vec![string_fact("service.redis.service.status", "failed")]);
    let mut config = HealthConfig::default();
    config.services.push(HealthServiceConfig {
        name: "redis.service".to_string(),
        expected: HealthServiceExpectedState::Active,
    });

    apply_judgement_rules(&mut report, &config);

    let finding = &report.findings[0];
    assert_eq!(finding.id, "J15:redis.service");
    assert_eq!(
        finding.detail_args.get("service").map(String::as_str),
        Some("redis.service")
    );
    assert_eq!(
        finding.detail_args.get("observed").map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        finding.detail_args.get("expected").map(String::as_str),
        Some("active")
    );
}

fn report_with_facts(facts: Vec<HealthFact>) -> HealthScanReport {
    let mut report = HealthScanReport::new("health-1", 0);
    report.facts = facts;
    report
}

fn finding_ids(report: &HealthScanReport) -> Vec<String> {
    report
        .findings
        .iter()
        .map(|finding| finding.id.clone())
        .collect()
}

fn float_fact(key: &str, value: f64) -> HealthFact {
    fact_with_value(key, HealthFactValue::Float(value))
}

fn string_fact(key: &str, value: &str) -> HealthFact {
    fact_with_value(key, HealthFactValue::String(value.to_string()))
}

fn fact_with_value(key: &str, value: HealthFactValue) -> HealthFact {
    HealthFact {
        id: key.to_string(),
        category: HealthFactCategory::Host,
        key: key.to_string(),
        value,
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 0,
    }
}
