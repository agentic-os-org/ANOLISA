use crate::config::{HealthServiceConfig, HealthServiceExpectedState};
use crate::diagnostics::health::builder::HealthReportBuilder;
use crate::diagnostics::health::model::{
    HealthFactCategory, HealthFactSource, HealthFindingCategory,
};
use crate::diagnostics::health::rules::apply_judgement_rules;

use super::*;

#[test]
fn service_try_prompt_identifies_service_in_both_languages() {
    use crate::config::Language;
    use crate::diagnostics::health::prompt_selection::startup_prompt_suggestions;
    use crate::I18n;

    for status in ["failed", "inactive"] {
        let service = "example-worker.service";
        let mut builder = HealthReportBuilder::for_started_at(1);
        builder.add_fact(
            HealthFactCategory::Service,
            format!("service.{service}.status"),
            HealthFactValue::String(status.into()),
            None,
            HealthFactSource::Fixture,
            0,
        );
        let mut report = builder.finish(2);
        let config = HealthConfig {
            services: vec![HealthServiceConfig {
                name: service.into(),
                expected: HealthServiceExpectedState::Active,
            }],
            ..HealthConfig::default()
        };
        apply_judgement_rules(&mut report, &config);
        apply_try_candidates(&mut report, &config);
        let item = report
            .try_items
            .iter()
            .find(|item| item.id.starts_with("T05:"))
            .unwrap();
        assert_eq!(
            item.prompt_args.get("service").map(String::as_str),
            Some(service)
        );
        for (language, verify) in [(Language::EnUs, "Verify"), (Language::ZhCn, "核实")] {
            let suggestions = startup_prompt_suggestions(&report, I18n::new(language), 3);
            let prompt = &suggestions.iter().find(|(id, _)| id == &item.id).unwrap().1;
            assert!(prompt.contains(service), "{prompt}");
            assert!(prompt.contains(verify), "{prompt}");
            assert!(!prompt.contains("{service}"), "{prompt}");
        }
    }
}

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
