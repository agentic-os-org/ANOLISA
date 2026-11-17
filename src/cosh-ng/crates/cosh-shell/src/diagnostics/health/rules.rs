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
    let memory_pressure =
        memory_low.is_some() || bool_value(facts, "memory.swap_memory_pressure") == Some(true);
    let swap_pressure_context = memory_pressure || oom_recent_1h || service_issue;

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
        && (memory_pressure || oom_recent_24h || config.memory_sensitive)
    {
        findings.push(finding(
            "J07",
            HealthSeverity::Warning,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingSwapPressure,
            evidence_ids(
                facts,
                &[
                    "memory.swap_used_ratio",
                    "memory.swap_used_mib",
                    "memory.swap_memory_pressure",
                ],
            ),
        ));
    } else if swap_used_at_least(facts, 0.50, 1024.0) && swap_pressure_context {
        findings.push(finding(
            "J08",
            HealthSeverity::Warning,
            HealthFindingCategory::Anomaly,
            HealthMessageId::HealthFindingSwapPressure,
            evidence_ids(
                facts,
                &[
                    "memory.swap_used_ratio",
                    "memory.swap_used_mib",
                    "memory.swap_memory_pressure",
                ],
            ),
        ));
    }

    findings.extend(disk_findings(facts, config));

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
    memory_pressure_level(available_mib, available_ratio)
}

pub(super) fn memory_pressure_level(
    available_mib: f64,
    available_ratio: f64,
) -> Option<HealthSeverity> {
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

pub(super) fn swap_context_complete(report: &HealthScanReport, config: &HealthConfig) -> bool {
    if !swap_used_at_least(&report.facts, 0.50, 1024.0) {
        return true;
    }
    if bool_value(&report.facts, "memory.swap_memory_pressure") == Some(true)
        || configured_service_issue(&report.facts, config)
        || number(&report.facts, "kernel.oom_latest_age_seconds").is_some_and(|age| age <= 86400.0)
        || (config.memory_sensitive && swap_used_at_least(&report.facts, 0.90, 1024.0))
    {
        return true;
    }
    super::plan::judgement_context_available("resource.swap", report, config)
}

fn swap_used_at_least(facts: &[HealthFact], ratio: f64, used_mib: f64) -> bool {
    number(facts, "memory.swap_used_ratio").is_some_and(|value| value >= ratio)
        && number(facts, "memory.swap_used_mib").is_some_and(|value| value >= used_mib)
}

fn disk_severity(
    used_ratio: Option<f64>,
    available_gib: Option<f64>,
    mount: &str,
    config: &HealthConfig,
) -> Option<HealthSeverity> {
    if used_ratio.is_some_and(|value| value >= 0.95)
        || (available_gib.is_some_and(|value| value < 2.0)
            && (mount == "/"
                || is_critical_mount(mount, config)
                || used_ratio.is_some_and(|value| value >= 0.85)))
    {
        Some(HealthSeverity::Critical)
    } else if used_ratio.is_some_and(|value| value >= 0.90)
        || (used_ratio.is_some_and(|value| value >= 0.85)
            && available_gib.is_some_and(|value| value < 20.0)
            && is_critical_mount(mount, config))
    {
        Some(HealthSeverity::Warning)
    } else {
        None
    }
}

fn disk_findings(facts: &[HealthFact], config: &HealthConfig) -> Vec<HealthFinding> {
    let paths = facts
        .iter()
        .filter_map(|fact| {
            fact.key
                .strip_prefix("filesystem.mount.")
                .and_then(|suffix| suffix.strip_suffix(".path"))
        })
        .collect::<Vec<_>>();
    let mut findings = Vec::new();
    for path in &paths {
        let prefix = format!("filesystem.mount.{path}.");
        let used = format!("{prefix}used_ratio");
        let available = format!("{prefix}available_gib");
        if let Some(severity) = disk_severity(
            number(facts, &used),
            number(facts, &available),
            path,
            config,
        ) {
            let rule = if severity == HealthSeverity::Critical {
                "J09"
            } else {
                "J10"
            };
            let mut result = finding(
                if *path == "/" {
                    rule.to_string()
                } else {
                    format!("{rule}:{path}")
                },
                severity,
                HealthFindingCategory::Anomaly,
                HealthMessageId::HealthFindingDiskHigh,
                evidence_ids(facts, &[&format!("{prefix}path"), &used, &available]),
            );
            result.detail_args.insert("mount".into(), path.to_string());
            findings.push(result);
        }
    }
    if paths.is_empty() {
        let mount = string_value(facts, "filesystem.riskiest_mount").unwrap_or("/");
        let used = number(facts, "filesystem.max_used_ratio")
            .or_else(|| number(facts, "filesystem.root_used_ratio"));
        let available = number(facts, "filesystem.available_gib")
            .or_else(|| number(facts, "filesystem.root_available_gib"));
        if let Some(severity) = disk_severity(used, available, mount, config) {
            let mut result = finding(
                if severity == HealthSeverity::Critical {
                    "J09"
                } else {
                    "J10"
                },
                severity,
                HealthFindingCategory::Anomaly,
                HealthMessageId::HealthFindingDiskHigh,
                evidence_ids(
                    facts,
                    &[
                        "filesystem.max_used_ratio",
                        "filesystem.available_gib",
                        "filesystem.riskiest_mount",
                        "filesystem.root_used_ratio",
                        "filesystem.root_available_gib",
                    ],
                ),
            );
            result.detail_args.insert("mount".into(), mount.into());
            findings.push(result);
        }
    }
    findings
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

pub(super) fn number(facts: &[HealthFact], key: &str) -> Option<f64> {
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
mod tests;
