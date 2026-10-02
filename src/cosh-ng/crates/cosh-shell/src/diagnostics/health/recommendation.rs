use std::collections::{BTreeMap, HashSet};

use crate::config::HealthConfig;

use super::model::{
    HealthFact, HealthFactValue, HealthFinding, HealthMessageId, HealthScanReport, HealthSeverity,
    HealthTryItem, HealthTryKind,
};
use super::suppression::{metrics_for_report, HealthSuppressionStore, SuppressionDecision};

const MIN_TRY_SCORE: i32 = 60;
const MAX_TRY_ITEMS: usize = 3;

pub(crate) fn apply_try_recommendations(
    report: &mut HealthScanReport,
    config: &HealthConfig,
    suppression: &mut HealthSuppressionStore,
    host_id: &str,
    now_ms: u64,
) {
    let try_items = generate_try_recommendations_with_recording(
        report,
        config,
        suppression,
        host_id,
        now_ms,
        true,
    );
    apply_try_items(report, try_items);
}

pub(crate) fn preview_try_recommendations(
    report: &mut HealthScanReport,
    config: &HealthConfig,
    suppression: &mut HealthSuppressionStore,
    host_id: &str,
    now_ms: u64,
) {
    let try_items = generate_try_recommendations_with_recording(
        report,
        config,
        suppression,
        host_id,
        now_ms,
        false,
    );
    apply_try_items(report, try_items);
}

pub(crate) fn record_visible_try_recommendations(
    report: &HealthScanReport,
    suppression: &mut HealthSuppressionStore,
    host_id: &str,
    now_ms: u64,
) {
    if report.try_items.is_empty() {
        return;
    }
    let metrics = metrics_for_report(report);
    let finding_ids = report
        .try_items
        .iter()
        .map(|item| item.finding_id.as_str())
        .collect::<HashSet<_>>();
    for finding in &report.findings {
        if finding_ids.contains(finding.id.as_str()) {
            suppression.record_shown(host_id, finding, metrics.clone(), now_ms);
        }
    }
}

fn apply_try_items(report: &mut HealthScanReport, try_items: Vec<HealthTryItem>) {
    for finding in &mut report.findings {
        finding.suggested_try_ids = try_items
            .iter()
            .filter(|item| item.finding_id == finding.id)
            .map(|item| item.id.clone())
            .collect();
    }
    report.try_items = try_items;
}

pub(crate) fn generate_try_recommendations(
    report: &HealthScanReport,
    config: &HealthConfig,
    suppression: &mut HealthSuppressionStore,
    host_id: &str,
    now_ms: u64,
) -> Vec<HealthTryItem> {
    generate_try_recommendations_with_recording(report, config, suppression, host_id, now_ms, true)
}

fn generate_try_recommendations_with_recording(
    report: &HealthScanReport,
    config: &HealthConfig,
    suppression: &mut HealthSuppressionStore,
    host_id: &str,
    now_ms: u64,
    record_selected: bool,
) -> Vec<HealthTryItem> {
    if report.findings.is_empty() || report.overall_severity == HealthSeverity::Ok {
        return Vec::new();
    }
    let metrics = metrics_for_report(report);
    let mut candidates = Vec::new();
    let finding_ids = report
        .findings
        .iter()
        .map(|finding| finding.id.as_str())
        .collect::<HashSet<_>>();
    for finding in &report.findings {
        if suppression.should_show(host_id, finding, &metrics, now_ms)
            == SuppressionDecision::Suppress
        {
            continue;
        }
        candidates.extend(candidates_for_finding(
            finding,
            report,
            config,
            &finding_ids,
        ));
    }
    candidates.sort_by_key(|item| (std::cmp::Reverse(item.score), item.id.clone()));

    let mut selected = Vec::new();
    let mut selected_categories = HashSet::new();
    for item in candidates {
        if item.score < MIN_TRY_SCORE {
            continue;
        }
        let category = try_category(&item.id);
        let critical = report
            .findings
            .iter()
            .find(|finding| finding.id == item.finding_id)
            .is_some_and(|finding| finding.severity == HealthSeverity::Critical);
        if !critical && !selected_categories.insert(category.to_string()) {
            continue;
        }
        let selected_finding = record_selected
            .then(|| {
                report
                    .findings
                    .iter()
                    .find(|finding| finding.id == item.finding_id)
            })
            .flatten();
        if let Some(finding) = selected_finding {
            suppression.record_shown(host_id, finding, metrics.clone(), now_ms);
        }
        selected.push(item);
        if selected.len() == MAX_TRY_ITEMS {
            break;
        }
    }
    selected
}

fn candidates_for_finding(
    finding: &HealthFinding,
    report: &HealthScanReport,
    config: &HealthConfig,
    finding_ids: &HashSet<&str>,
) -> Vec<HealthTryItem> {
    let mut items = Vec::new();
    let low_confidence_single = is_low_confidence_single_signal(finding, report);
    if low_confidence_single && !config.verbose {
        return items;
    }
    if matches!(finding.id.as_str(), "J05" | "J06") {
        items.push(try_item(
            "T01",
            finding,
            HealthMessageId::HealthTryAnalyzeMemoryPressure,
            HealthMessageId::HealthTryReasonMemoryLow,
            BTreeMap::new(),
            base_score(finding) + evidence_weight(finding) + actionability_weight(),
        ));
    }
    if matches!(finding.id.as_str(), "J07" | "J08") {
        items.push(try_item(
            "T02",
            finding,
            HealthMessageId::HealthTryCheckSwapPressure,
            HealthMessageId::HealthTryReasonSwapWithContext,
            BTreeMap::new(),
            base_score(finding)
                + evidence_weight(finding)
                + actionability_weight()
                + multi_signal_weight(finding_ids),
        ));
    }
    if matches!(finding.id.as_str(), "J11" | "J12")
        || (finding.id == "J13" && has_any(finding_ids, &["J05", "J06", "J07", "J08"]))
    {
        items.push(try_item(
            "T03",
            finding,
            HealthMessageId::HealthTryCheckRecentOom,
            HealthMessageId::HealthTryReasonRecentOom,
            BTreeMap::new(),
            base_score(finding)
                + evidence_weight(finding)
                + actionability_weight()
                + oom_freshness_weight(report),
        ));
    }
    if matches!(finding.id.as_str(), "J09" | "J10") {
        items.push(try_item(
            "T04",
            finding,
            HealthMessageId::HealthTryInspectDiskUsage,
            HealthMessageId::HealthTryReasonDiskHigh,
            BTreeMap::new(),
            base_score(finding) + evidence_weight(finding) + actionability_weight(),
        ));
    }
    if finding.id.starts_with("J15") || finding.id.starts_with("J16") {
        items.push(try_item(
            "T05",
            finding,
            HealthMessageId::HealthTryInspectServiceStatus,
            HealthMessageId::HealthTryReasonServiceState,
            BTreeMap::new(),
            base_score(finding) + evidence_weight(finding) + actionability_weight(),
        ));
    }
    if finding.id == "J03"
        || (finding.id == "J04" && (config.verbose || has_service_failure(finding_ids)))
    {
        items.push(try_item(
            "T06",
            finding,
            HealthMessageId::HealthTryInspectHighLoad,
            HealthMessageId::HealthTryReasonHighLoad,
            BTreeMap::new(),
            base_score(finding) + evidence_weight(finding) + actionability_weight()
                - low_confidence_penalty(low_confidence_single),
        ));
    }
    if matches!(finding.id.as_str(), "J11" | "J12" | "J13") && has_any(finding_ids, &["J05", "J06"])
    {
        if let Some(process) = string_fact(&report.facts, "kernel.oom_killed_process") {
            let mut args = BTreeMap::new();
            args.insert("process".to_string(), process.to_string());
            items.push(try_item(
                "T07",
                finding,
                HealthMessageId::HealthTryInspectProcessMemory,
                HealthMessageId::HealthTryReasonRecentOom,
                args,
                base_score(finding)
                    + evidence_weight(finding)
                    + actionability_weight()
                    + multi_signal_weight(finding_ids)
                    + role_match_weight(),
            ));
        }
    }
    if finding.id == "J02"
        && !report
            .findings
            .iter()
            .any(|item| item.severity.precedence() > HealthSeverity::Degraded.precedence())
    {
        items.push(try_item(
            "T08",
            finding,
            HealthMessageId::HealthTryReviewUnavailableChecks,
            HealthMessageId::HealthTryReasonMissingCoreCheck,
            BTreeMap::new(),
            base_score(finding) + actionability_weight() + missing_core_weight(),
        ));
    }
    items
}

fn try_item(
    rule_id: &str,
    finding: &HealthFinding,
    label_id: HealthMessageId,
    reason_id: HealthMessageId,
    label_args: BTreeMap<String, String>,
    score: i32,
) -> HealthTryItem {
    HealthTryItem {
        id: format!("{rule_id}:{}", finding.id),
        prompt_id: prompt_id_for_label(label_id),
        prompt_args: label_args.clone(),
        label_id,
        label_args,
        kind: HealthTryKind::AskAgent,
        command: None,
        reason_id,
        reason_args: BTreeMap::new(),
        score,
        finding_id: finding.id.clone(),
    }
}

fn prompt_id_for_label(label_id: HealthMessageId) -> Option<HealthMessageId> {
    match label_id {
        HealthMessageId::HealthTryAnalyzeMemoryPressure => {
            Some(HealthMessageId::HealthPromptAnalyzeMemoryPressure)
        }
        HealthMessageId::HealthTryCheckSwapPressure => {
            Some(HealthMessageId::HealthPromptCheckSwapPressure)
        }
        HealthMessageId::HealthTryCheckRecentOom => {
            Some(HealthMessageId::HealthPromptCheckRecentOom)
        }
        HealthMessageId::HealthTryInspectDiskUsage => {
            Some(HealthMessageId::HealthPromptInspectDiskUsage)
        }
        HealthMessageId::HealthTryInspectServiceStatus => {
            Some(HealthMessageId::HealthPromptInspectServiceStatus)
        }
        HealthMessageId::HealthTryInspectHighLoad => {
            Some(HealthMessageId::HealthPromptInspectHighLoad)
        }
        HealthMessageId::HealthTryInspectProcessMemory => {
            Some(HealthMessageId::HealthPromptInspectProcessMemory)
        }
        HealthMessageId::HealthTryReviewUnavailableChecks => {
            Some(HealthMessageId::HealthPromptReviewUnavailableChecks)
        }
        _ => None,
    }
}

fn base_score(finding: &HealthFinding) -> i32 {
    match finding.severity {
        HealthSeverity::Critical => 100,
        HealthSeverity::Warning => 60,
        HealthSeverity::Degraded | HealthSeverity::Unavailable => 35,
        HealthSeverity::Ok => 0,
    }
}

fn evidence_weight(finding: &HealthFinding) -> i32 {
    if finding.evidence_fact_ids.is_empty() {
        0
    } else {
        15
    }
}

fn actionability_weight() -> i32 {
    20
}

fn multi_signal_weight(finding_ids: &HashSet<&str>) -> i32 {
    if has_any(finding_ids, &["J05", "J06"])
        && has_any(finding_ids, &["J07", "J08", "J11", "J12", "J13"])
    {
        25
    } else {
        0
    }
}

fn oom_freshness_weight(report: &HealthScanReport) -> i32 {
    let Some(age) = number_fact(&report.facts, "kernel.oom_latest_age_seconds") else {
        return 0;
    };
    if age <= 300.0 {
        25
    } else if age <= 3600.0 {
        15
    } else if age <= 86400.0 {
        5
    } else {
        0
    }
}

fn role_match_weight() -> i32 {
    15
}

fn missing_core_weight() -> i32 {
    15
}

fn low_confidence_penalty(low_confidence: bool) -> i32 {
    if low_confidence {
        20
    } else {
        0
    }
}

fn is_low_confidence_single_signal(finding: &HealthFinding, report: &HealthScanReport) -> bool {
    report.findings.len() == 1 && finding.id == "J04"
}

fn has_service_failure(finding_ids: &HashSet<&str>) -> bool {
    finding_ids.iter().any(|id| id.starts_with("J15"))
}

fn has_any(finding_ids: &HashSet<&str>, ids: &[&str]) -> bool {
    ids.iter().any(|id| finding_ids.contains(id))
}

fn try_category(id: &str) -> &str {
    id.split_once(':')
        .map(|(category, _)| category)
        .unwrap_or(id)
}

fn number_fact(facts: &[HealthFact], key: &str) -> Option<f64> {
    match &facts.iter().find(|fact| fact.key == key)?.value {
        HealthFactValue::Integer(value) => Some(*value as f64),
        HealthFactValue::Unsigned(value) => Some(*value as f64),
        HealthFactValue::Float(value) => Some(*value),
        _ => None,
    }
}

fn string_fact<'a>(facts: &'a [HealthFact], key: &str) -> Option<&'a str> {
    match &facts.iter().find(|fact| fact.key == key)?.value {
        HealthFactValue::String(value) => Some(value.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{HealthServiceConfig, HealthServiceExpectedState};
    use crate::diagnostics::health::builder::HealthReportBuilder;
    use crate::diagnostics::health::model::{
        HealthFactCategory, HealthFactSource, HealthFindingCategory,
    };
    use crate::diagnostics::health::rules::apply_judgement_rules;

    use super::*;

    #[test]
    fn memory_and_swap_generate_sorted_try_items() {
        let mut report = report_with_memory_pressure();
        apply_judgement_rules(&mut report, &HealthConfig::default());
        let mut suppression = HealthSuppressionStore::default();

        apply_try_recommendations(
            &mut report,
            &HealthConfig::default(),
            &mut suppression,
            "host",
            1,
        );

        assert_eq!(report.try_items.len(), 2);
        assert_eq!(
            report.try_items[0].label_id,
            HealthMessageId::HealthTryCheckSwapPressure
        );
        assert_eq!(
            report.try_items[1].label_id,
            HealthMessageId::HealthTryAnalyzeMemoryPressure
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| !finding.suggested_try_ids.is_empty()));
    }

    #[test]
    fn single_cpu_warning_is_suppressed_unless_verbose() {
        let mut report = HealthReportBuilder::for_started_at(1);
        report
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_1m",
                HealthFactValue::Float(2.5),
                None,
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_5m",
                HealthFactValue::Float(1.2),
                None,
                HealthFactSource::Fixture,
                0,
            );
        let mut report = report.finish(2);
        apply_judgement_rules(&mut report, &HealthConfig::default());
        let mut suppression = HealthSuppressionStore::default();

        apply_try_recommendations(
            &mut report,
            &HealthConfig::default(),
            &mut suppression,
            "host",
            1,
        );
        assert!(report.try_items.is_empty());

        let config = HealthConfig {
            verbose: true,
            ..HealthConfig::default()
        };
        apply_try_recommendations(
            &mut report,
            &config,
            &mut HealthSuppressionStore::default(),
            "host2",
            1,
        );
        assert_eq!(
            report.try_items[0].label_id,
            HealthMessageId::HealthTryInspectHighLoad
        );
    }

    #[test]
    fn cpu_warning_with_oom_context_does_not_generate_high_load_try() {
        let mut report = HealthReportBuilder::for_started_at(1);
        report
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_1m",
                HealthFactValue::Float(3.0),
                None,
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_5m",
                HealthFactValue::Float(1.5),
                None,
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_age_seconds",
                HealthFactValue::Unsigned(900),
                Some("seconds".to_string()),
                HealthFactSource::Fixture,
                0,
            );
        let mut report = report.finish(2);
        apply_judgement_rules(&mut report, &HealthConfig::default());

        apply_try_recommendations(
            &mut report,
            &HealthConfig::default(),
            &mut HealthSuppressionStore::default(),
            "host",
            1,
        );

        assert!(report.findings.iter().any(|finding| finding.id == "J04"));
        assert!(report.findings.iter().any(|finding| finding.id == "J12"));
        assert!(
            report
                .try_items
                .iter()
                .all(|item| item.label_id != HealthMessageId::HealthTryInspectHighLoad),
            "{:?}",
            report.try_items
        );
    }

    #[test]
    fn cpu_warning_with_service_failure_generates_high_load_try() {
        let mut report = HealthReportBuilder::for_started_at(1);
        report
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_1m",
                HealthFactValue::Float(3.0),
                None,
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_5m",
                HealthFactValue::Float(1.5),
                None,
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Service,
                "service.cosh-failed-probe.service.status",
                HealthFactValue::String("failed".to_string()),
                None,
                HealthFactSource::Fixture,
                0,
            );
        let mut report = report.finish(2);
        let mut config = HealthConfig::default();
        config.services.push(HealthServiceConfig {
            name: "cosh-failed-probe.service".to_string(),
            expected: HealthServiceExpectedState::Active,
        });
        apply_judgement_rules(&mut report, &config);

        apply_try_recommendations(
            &mut report,
            &config,
            &mut HealthSuppressionStore::default(),
            "host",
            1,
        );

        assert!(report.findings.iter().any(|finding| finding.id == "J04"));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.id.starts_with("J15")));
        assert!(
            report
                .try_items
                .iter()
                .any(|item| item.label_id == HealthMessageId::HealthTryInspectHighLoad),
            "{:?}",
            report.try_items
        );
    }

    #[test]
    fn recent_oom_with_process_adds_process_specific_try() {
        let mut builder = HealthReportBuilder::for_started_at(1);
        add_memory_low(&mut builder);
        builder
            .add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_age_seconds",
                HealthFactValue::Unsigned(120),
                Some("seconds".to_string()),
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_killed_process",
                HealthFactValue::String("mysql".to_string()),
                None,
                HealthFactSource::Fixture,
                0,
            );
        let mut report = builder.finish(2);
        apply_judgement_rules(&mut report, &HealthConfig::default());

        apply_try_recommendations(
            &mut report,
            &HealthConfig::default(),
            &mut HealthSuppressionStore::default(),
            "host",
            1,
        );

        let process_try = report
            .try_items
            .iter()
            .find(|item| item.id.starts_with("T07"))
            .expect("process try");
        assert_eq!(
            process_try.label_id,
            HealthMessageId::HealthTryInspectProcessMemory
        );
        assert_eq!(
            process_try.label_args.get("process").map(String::as_str),
            Some("mysql")
        );
    }

    #[test]
    fn suppression_store_hides_recent_duplicate_try() {
        let mut report = report_with_memory_pressure();
        apply_judgement_rules(&mut report, &HealthConfig::default());
        let mut suppression = HealthSuppressionStore::default();

        apply_try_recommendations(
            &mut report,
            &HealthConfig::default(),
            &mut suppression,
            "host",
            1,
        );
        assert!(!report.try_items.is_empty());
        apply_try_recommendations(
            &mut report,
            &HealthConfig::default(),
            &mut suppression,
            "host",
            2,
        );
        assert!(report.try_items.is_empty());
    }

    #[test]
    fn degraded_only_generates_unavailable_checks_try() {
        let mut report = HealthScanReport::new("health-1", 1);
        report.findings.push(HealthFinding {
            id: "J02".to_string(),
            severity: HealthSeverity::Degraded,
            category: HealthFindingCategory::CollectionGap,
            title_id: HealthMessageId::HealthFindingCoreCollectorUnavailable,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: Vec::new(),
            suggested_try_ids: Vec::new(),
        });
        report.recompute_overall_severity();

        apply_try_recommendations(
            &mut report,
            &HealthConfig::default(),
            &mut HealthSuppressionStore::default(),
            "host",
            1,
        );

        assert_eq!(
            report.try_items[0].label_id,
            HealthMessageId::HealthTryReviewUnavailableChecks
        );
    }

    fn report_with_memory_pressure() -> HealthScanReport {
        let mut builder = HealthReportBuilder::for_started_at(1);
        add_memory_low(&mut builder);
        builder
            .add_fact(
                HealthFactCategory::Memory,
                "memory.swap_used_ratio",
                HealthFactValue::Float(0.55),
                None,
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.swap_used_mib",
                HealthFactValue::Unsigned(2048),
                Some("MiB".to_string()),
                HealthFactSource::Fixture,
                0,
            );
        builder.finish(2)
    }

    fn add_memory_low(builder: &mut HealthReportBuilder) {
        builder
            .add_fact(
                HealthFactCategory::Memory,
                "memory.available_mib",
                HealthFactValue::Unsigned(700),
                Some("MiB".to_string()),
                HealthFactSource::Fixture,
                0,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.available_ratio",
                HealthFactValue::Float(0.08),
                None,
                HealthFactSource::Fixture,
                0,
            );
    }
}
