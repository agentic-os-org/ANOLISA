use super::*;
use crate::diagnostics::health::model::{HealthFactCoverage, HealthFactMetadata, HealthFactScope};
use crate::diagnostics::health::{HealthFactCategory, HealthFactSource};

#[test]
fn health_banner_oom_count_is_only_a_lower_bound_for_a_limited_read_window() {
    let fact = HealthFact {
        id: "count".to_string(),
        category: HealthFactCategory::Kernel,
        key: "kernel.oom_event_count_last_24h".to_string(),
        value: HealthFactValue::Unsigned(2),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 1,
    };
    let mut report = HealthScanReport::new("oom-window", 1000);
    report.facts.push(fact.clone());
    for coverage in [
        None,
        Some(HealthFactCoverage::ReadWindow {
            since_ms: Some(0),
            until_ms: Some(1000),
            max_records: 200,
            complete: false,
        }),
    ] {
        report.fact_metadata.clear();
        if let Some(coverage) = coverage {
            report.fact_metadata.insert(
                fact.id.clone(),
                HealthFactMetadata {
                    scope: HealthFactScope::Host,
                    sampled_at_ms: 1000,
                    coverage,
                },
            );
        }
        let english =
            format_evidence_fact(&fact, &report, crate::I18n::new(crate::Language::EnUs)).unwrap();
        let chinese =
            format_evidence_fact(&fact, &report, crate::I18n::new(crate::Language::ZhCn)).unwrap();
        assert!(english.contains("at least 2"), "{english}");
        assert!(english.contains("read window"), "{english}");
        assert!(chinese.contains("至少 2"), "{chinese}");
        assert!(chinese.contains("已读范围"), "{chinese}");
    }
}

#[test]
fn health_banner_disk_finding_uses_its_own_path_instead_of_the_global_maximum() {
    let mut report = HealthScanReport::new("per-path", 1000);
    for (id, key, value, path) in [
        ("root-used", "filesystem.root_used_ratio", 0.92, Some("/")),
        ("root-free", "filesystem.root_available_gib", 3.1, Some("/")),
        (
            "data-used",
            "filesystem.mount./data.used_ratio",
            0.99,
            Some("/data"),
        ),
        (
            "data-free",
            "filesystem.mount./data.available_gib",
            0.1,
            Some("/data"),
        ),
        ("global-max", "filesystem.max_used_ratio", 0.99, None),
    ] {
        report.facts.push(HealthFact {
            id: id.to_string(),
            category: HealthFactCategory::Disk,
            key: key.to_string(),
            value: HealthFactValue::Float(value),
            unit: None,
            source: HealthFactSource::Fixture,
            elapsed_ms: 1,
        });
        if let Some(path) = path {
            report.fact_metadata.insert(
                id.to_string(),
                HealthFactMetadata {
                    scope: HealthFactScope::Path(path.to_string()),
                    sampled_at_ms: 1000,
                    coverage: HealthFactCoverage::Snapshot,
                },
            );
        }
    }
    report.facts.push(HealthFact {
        id: "riskiest".to_string(),
        category: HealthFactCategory::Disk,
        key: "filesystem.riskiest_mount".to_string(),
        value: HealthFactValue::String("/data".to_string()),
        unit: None,
        source: HealthFactSource::Fixture,
        elapsed_ms: 1,
    });
    let finding = HealthFinding {
        id: "J09:/".to_string(),
        severity: HealthSeverity::Warning,
        category: crate::diagnostics::health::HealthFindingCategory::RootCause,
        title_id: HealthMessageId::HealthFindingDiskHigh,
        detail_id: None,
        detail_args: Default::default(),
        evidence_fact_ids: vec!["root-used".to_string(), "root-free".to_string()],
        suggested_try_ids: vec![],
    };
    let text = evidence_text(
        &report,
        &finding,
        crate::I18n::new(crate::Language::EnUs),
        240,
    );
    assert!(text.contains("92%"), "{text}");
    assert!(text.contains("3.1"), "{text}");
    assert!(!text.contains("/data"), "{text}");
    assert!(!text.contains("99%"), "{text}");
}

#[test]
fn health_banner_swap_missing_context_keeps_its_check_label_and_is_not_unsupported() {
    use crate::diagnostics::health::model::{HealthCheckResult, HealthCheckStatus};
    use crate::diagnostics::health::{HealthCollector, UnavailableCollector};
    let mut report = HealthScanReport::new("swap-context", 1000);
    let gap = UnavailableCollector {
        elapsed_ms: 1,
        collector: HealthCollector::Memory,
        reason: HealthUnavailableReason::MissingContext,
        severity: HealthSeverity::Unavailable,
    };
    report.unavailable.push(gap.clone());
    report.check_results.push(HealthCheckResult {
        id: "resource.swap".to_string(),
        collector: Some(HealthCollector::Memory),
        status: HealthCheckStatus::Partial,
        fact_ids: vec![],
        gaps: vec![gap],
        elapsed_ms: 1,
    });
    let lines = unavailable_lines(&report, crate::I18n::new(crate::Language::EnUs), 1);
    let text = lines[0].plain_text();
    assert!(text.contains("Swap"), "{text}");
    assert!(!text.to_lowercase().contains("unsupported"), "{text}");
    assert!(!text.contains("Memory"), "{text}");
}

#[test]
fn summary_reserves_three_titles_before_evidence_within_five_body_rows() {
    let mut report = HealthScanReport::new("summary-budget", 1000);
    for index in 0..7 {
        report.findings.push(HealthFinding {
            id: format!("service-{index}"),
            severity: HealthSeverity::Critical,
            category: crate::diagnostics::health::HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingServiceFailed,
            detail_id: None,
            detail_args: BTreeMap::from([
                ("service".into(), format!("svc-{index}")),
                (
                    "observed".into(),
                    "failed after a long startup sequence".into(),
                ),
                ("expected".into(), "active and running".into()),
            ]),
            evidence_fact_ids: vec![],
            suggested_try_ids: vec![],
        });
    }
    for language in [crate::Language::ZhCn, crate::Language::EnUs] {
        for width in [36, 76, 116] {
            let original = report.clone();
            let lines = finding_insight_lines(&report, crate::I18n::new(language), 20, width);
            let text = lines
                .iter()
                .map(HealthBannerLine::plain_text)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(lines.len() <= 5, "{text}");
            assert_eq!(text.matches('•').count(), 3, "{text}");
            for index in 0..3 {
                assert!(text.contains(&format!("svc-{index}")), "{text}");
            }
            assert!(!text.contains("svc-3"), "{text}");
            assert!(lines
                .iter()
                .all(|line| display_width(&line.plain_text()) <= width));
            assert_eq!(report, original);
        }
    }
}
