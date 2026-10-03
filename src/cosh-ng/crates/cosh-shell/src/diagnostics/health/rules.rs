use std::collections::{BTreeMap, HashSet};

use crate::config::{HealthConfig, HealthServiceExpectedState};

use super::model::{
    HealthCollector, HealthFact, HealthFactValue, HealthFinding, HealthFindingCategory,
    HealthMessageId, HealthScanReport, HealthSeverity, HealthUnavailableReason,
};

pub(crate) fn apply_judgement_rules(report: &mut HealthScanReport, config: &HealthConfig) {
    report.findings = evaluate_judgement_rules(report, config);
    report.recompute_overall_severity();
}

pub(crate) fn evaluate_judgement_rules(
    report: &HealthScanReport,
    config: &HealthConfig,
) -> Vec<HealthFinding> {
    let mut findings = Vec::new();
    let facts = &report.facts;
    let memory_low = memory_low_level(facts);
    let service_issue = configured_service_issue(facts, config);
    let oom_age = number(facts, "kernel.oom_latest_age_seconds");
    let oom_recent_1h = oom_age.is_some_and(|age| age <= 3600.0);
    let oom_recent_24h = oom_age.is_some_and(|age| age <= 86400.0);
    let swap_pressure_context = memory_low.is_some() || oom_recent_1h || service_issue;

    if report.unavailable.iter().any(|item| {
        item.collector == HealthCollector::Host
            && item.reason == HealthUnavailableReason::Unsupported
    }) {
        findings.push(finding(
            "J01",
            HealthSeverity::Unavailable,
            HealthFindingCategory::CollectionGap,
            HealthMessageId::HealthFindingPlatformUnsupported,
            Vec::new(),
        ));
    }

    let core_collectors = [
        HealthCollector::Host,
        HealthCollector::Cpu,
        HealthCollector::Memory,
        HealthCollector::Disk,
    ];
    if report.unavailable.iter().any(|item| {
        core_collectors.contains(&item.collector)
            && matches!(
                item.reason,
                HealthUnavailableReason::Timeout
                    | HealthUnavailableReason::PermissionDenied
                    | HealthUnavailableReason::CommandMissing
            )
    }) {
        findings.push(finding(
            "J02",
            HealthSeverity::Degraded,
            HealthFindingCategory::CollectionGap,
            HealthMessageId::HealthFindingCoreCollectorUnavailable,
            Vec::new(),
        ));
    }

    if cpu_load_at_least(facts, 4.0, 2.0) {
        findings.push(finding(
            "J03",
            HealthSeverity::Critical,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingCpuLoadHigh,
            evidence_ids(facts, &["cpu.load_per_core_1m", "cpu.load_per_core_5m"]),
        ));
    } else if cpu_load_at_least(facts, 2.0, 1.0) {
        findings.push(finding(
            "J04",
            HealthSeverity::Warning,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingCpuLoadHigh,
            evidence_ids(facts, &["cpu.load_per_core_1m", "cpu.load_per_core_5m"]),
        ));
    }

    if memory_low == Some(HealthSeverity::Critical) {
        findings.push(finding(
            "J05",
            HealthSeverity::Critical,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingMemoryAvailableLow,
            evidence_ids(facts, &["memory.available_mib", "memory.available_ratio"]),
        ));
    } else if memory_low == Some(HealthSeverity::Warning) {
        findings.push(finding(
            "J06",
            HealthSeverity::Warning,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingMemoryAvailableLow,
            evidence_ids(facts, &["memory.available_mib", "memory.available_ratio"]),
        ));
    }

    if swap_used_at_least(facts, 0.90, 1024.0)
        && (memory_low.is_some() || oom_recent_24h || config.memory_sensitive)
    {
        findings.push(finding(
            "J07",
            HealthSeverity::Warning,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingSwapPressure,
            evidence_ids(facts, &["memory.swap_used_ratio", "memory.swap_used_mib"]),
        ));
    } else if swap_used_at_least(facts, 0.50, 1024.0) && swap_pressure_context {
        findings.push(finding(
            "J08",
            HealthSeverity::Warning,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingSwapPressure,
            evidence_ids(facts, &["memory.swap_used_ratio", "memory.swap_used_mib"]),
        ));
    }

    if disk_critical(facts) {
        findings.push(finding(
            "J09",
            HealthSeverity::Critical,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingDiskHigh,
            evidence_ids(
                facts,
                &[
                    "filesystem.max_used_ratio",
                    "filesystem.available_gib",
                    "filesystem.riskiest_mount",
                ],
            ),
        ));
    } else if disk_warning(facts, config) {
        findings.push(finding(
            "J10",
            HealthSeverity::Warning,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingDiskHigh,
            evidence_ids(
                facts,
                &[
                    "filesystem.max_used_ratio",
                    "filesystem.available_gib",
                    "filesystem.riskiest_mount",
                ],
            ),
        ));
    }

    if oom_age.is_some_and(|age| age <= 300.0) {
        findings.push(finding(
            "J11",
            HealthSeverity::Critical,
            HealthFindingCategory::RootCause,
            HealthMessageId::HealthFindingRecentOom,
            oom_evidence_ids(facts),
        ));
    } else if oom_age.is_some_and(|age| age <= 3600.0) {
        findings.push(finding(
            "J12",
            HealthSeverity::Warning,
            HealthFindingCategory::RootCause,
            HealthMessageId::HealthFindingRecentOom,
            oom_evidence_ids(facts),
        ));
    } else if oom_age.is_some_and(|age| age <= 86400.0)
        && (memory_low.is_some() || swap_pressure_context || service_issue)
    {
        findings.push(finding(
            "J13",
            HealthSeverity::Warning,
            HealthFindingCategory::Observation,
            HealthMessageId::HealthFindingRecentOom,
            oom_evidence_ids(facts),
        ));
    }

    if bool_value(facts, "kernel.panic_recent") == Some(true) {
        findings.push(finding(
            "J14",
            HealthSeverity::Critical,
            HealthFindingCategory::RootCause,
            HealthMessageId::HealthFindingKernelPanic,
            evidence_ids(facts, &["kernel.panic_recent"]),
        ));
    }

    for service in &config.services {
        let key = format!("service.{}.status", service.name);
        let Some(status) = string_value(facts, &key) else {
            continue;
        };
        if status == "failed" {
            let mut service_finding = finding(
                format!("J15:{}", service.name),
                HealthSeverity::Critical,
                HealthFindingCategory::RootCause,
                HealthMessageId::HealthFindingServiceFailed,
                evidence_ids(facts, &[&key]),
            );
            service_finding.detail_args =
                service_detail_args(&service.name, status, service.expected);
            findings.push(service_finding);
        } else if status == "inactive" && service.expected == HealthServiceExpectedState::Active {
            let mut service_finding = finding(
                format!("J16:{}", service.name),
                HealthSeverity::Warning,
                HealthFindingCategory::Anomaly,
                HealthMessageId::HealthFindingServiceInactive,
                evidence_ids(facts, &[&key]),
            );
            service_finding.detail_args =
                service_detail_args(&service.name, status, service.expected);
            findings.push(service_finding);
        }
    }

    findings.sort_by_key(|finding| {
        (
            std::cmp::Reverse(finding.severity.precedence()),
            finding.id.clone(),
        )
    });
    findings
}

fn finding(
    id: impl Into<String>,
    severity: HealthSeverity,
    category: HealthFindingCategory,
    title_id: HealthMessageId,
    evidence_fact_ids: Vec<String>,
) -> HealthFinding {
    HealthFinding {
        id: id.into(),
        severity,
        category,
        title_id,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids,
        suggested_try_ids: Vec::new(),
    }
}

fn service_detail_args(
    service_name: &str,
    observed: &str,
    expected: HealthServiceExpectedState,
) -> BTreeMap<String, String> {
    let mut args = BTreeMap::new();
    args.insert("service".to_string(), service_name.to_string());
    args.insert("observed".to_string(), observed.to_string());
    args.insert(
        "expected".to_string(),
        expected_state_label(expected).to_string(),
    );
    args
}

fn expected_state_label(expected: HealthServiceExpectedState) -> &'static str {
    match expected {
        HealthServiceExpectedState::Active => "active",
        HealthServiceExpectedState::Inactive => "inactive",
    }
}

fn oom_evidence_ids(facts: &[HealthFact]) -> Vec<String> {
    evidence_ids(
        facts,
        &[
            "kernel.oom_latest_age_seconds",
            "kernel.oom_killed_process",
            "kernel.oom_latest_pid",
            "kernel.oom_latest_scope_label_id",
            "kernel.oom_latest_task_cgroup",
            "kernel.oom_latest_oom_cgroup",
            "kernel.oom_event_count_last_1h",
            "kernel.oom_event_count_last_24h",
            "kernel.oom_latest_confidence",
        ],
    )
}

fn cpu_load_at_least(facts: &[HealthFact], one_minute: f64, five_minutes: f64) -> bool {
    number(facts, "cpu.load_per_core_1m").is_some_and(|value| value >= one_minute)
        && number(facts, "cpu.load_per_core_5m").is_some_and(|value| value >= five_minutes)
}

fn memory_low_level(facts: &[HealthFact]) -> Option<HealthSeverity> {
    let available_mib = number(facts, "memory.available_mib")?;
    let available_ratio = number(facts, "memory.available_ratio")?;
    if available_mib < 512.0 || (available_ratio < 0.05 && available_mib < 2048.0) {
        return Some(HealthSeverity::Critical);
    }
    if available_mib < 1024.0
        || (available_ratio < 0.10 && available_mib < 8192.0)
        || (available_ratio < 0.15 && available_mib < 2048.0)
    {
        return Some(HealthSeverity::Warning);
    }
    None
}

fn swap_used_at_least(facts: &[HealthFact], ratio: f64, used_mib: f64) -> bool {
    number(facts, "memory.swap_used_ratio").is_some_and(|value| value >= ratio)
        && number(facts, "memory.swap_used_mib").is_some_and(|value| value >= used_mib)
}

fn disk_critical(facts: &[HealthFact]) -> bool {
    number(facts, "filesystem.max_used_ratio").is_some_and(|value| value >= 0.95)
        || number(facts, "filesystem.available_gib").is_some_and(|value| value < 2.0)
}

fn disk_warning(facts: &[HealthFact], config: &HealthConfig) -> bool {
    let used_ratio = number(facts, "filesystem.max_used_ratio").unwrap_or_default();
    if used_ratio >= 0.90 {
        return true;
    }
    let Some(available_gib) = number(facts, "filesystem.available_gib") else {
        return false;
    };
    used_ratio >= 0.85
        && available_gib < 20.0
        && string_value(facts, "filesystem.riskiest_mount")
            .is_some_and(|mount| is_critical_mount(mount, config))
}

fn configured_service_issue(facts: &[HealthFact], config: &HealthConfig) -> bool {
    config.services.iter().any(|service| {
        let key = format!("service.{}.status", service.name);
        string_value(facts, &key).is_some_and(|status| {
            status == "failed"
                || (status == "inactive" && service.expected == HealthServiceExpectedState::Active)
        })
    })
}

fn is_critical_mount(mount: &str, config: &HealthConfig) -> bool {
    let critical = config
        .critical_mounts
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    critical.contains(mount)
}

fn evidence_ids(facts: &[HealthFact], keys: &[&str]) -> Vec<String> {
    keys.iter()
        .filter_map(|key| fact(facts, key))
        .map(|fact| fact.id.clone())
        .collect()
}

fn number(facts: &[HealthFact], key: &str) -> Option<f64> {
    match &fact(facts, key)?.value {
        HealthFactValue::Integer(value) => Some(*value as f64),
        HealthFactValue::Unsigned(value) => Some(*value as f64),
        HealthFactValue::Float(value) => Some(*value),
        _ => None,
    }
}

fn string_value<'a>(facts: &'a [HealthFact], key: &str) -> Option<&'a str> {
    match &fact(facts, key)?.value {
        HealthFactValue::String(value) => Some(value.as_str()),
        _ => None,
    }
}

fn bool_value(facts: &[HealthFact], key: &str) -> Option<bool> {
    match &fact(facts, key)?.value {
        HealthFactValue::Bool(value) => Some(*value),
        _ => None,
    }
}

fn fact<'a>(facts: &'a [HealthFact], key: &str) -> Option<&'a HealthFact> {
    facts.iter().find(|fact| fact.key == key)
}

#[cfg(test)]
mod tests {
    use crate::config::{HealthServiceConfig, HealthServiceExpectedState};

    use super::*;
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
        let mut report =
            report_with_facts(vec![string_fact("service.redis.service.status", "failed")]);
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
}
