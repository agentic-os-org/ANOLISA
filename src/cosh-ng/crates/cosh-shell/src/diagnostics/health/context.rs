use super::model::{HealthFactValue, HealthMessageId, HealthScanReport, UnavailableCollector};

const MAX_FACTS: usize = 5;
const MAX_FINDINGS: usize = 4;
const MAX_UNAVAILABLE: usize = 3;

pub(crate) fn health_context_hint(report: &HealthScanReport) -> Option<String> {
    if report.scan_id.trim().is_empty() {
        return None;
    }

    let mut parts = vec![
        "health_scan".to_string(),
        format!("scan_id={}", safe_token(&report.scan_id)),
        format!("overall_severity={}", report.overall_severity.label()),
    ];
    if let Some(score) = report.health_score {
        parts.push(format!("health_score={score}"));
    }

    let facts = bounded_numeric_facts(report);
    if !facts.is_empty() {
        parts.push(format!("facts=[{}]", facts.join(",")));
    }

    let findings = bounded_findings(report);
    if !findings.is_empty() {
        parts.push(format!("findings=[{}]", findings.join(",")));
    }

    let unavailable = bounded_unavailable(report);
    if !unavailable.is_empty() {
        parts.push(format!("unavailable=[{}]", unavailable.join(",")));
    }

    parts.push("bounded_facts_only=true".to_string());
    parts.push("no_collector_stdout=true".to_string());
    Some(parts.join(" "))
}

fn bounded_numeric_facts(report: &HealthScanReport) -> Vec<String> {
    report
        .facts
        .iter()
        .filter_map(|fact| {
            numeric_value(&fact.value).map(|value| {
                format!(
                    "{}:{}={}",
                    safe_token(&fact.id),
                    safe_token(&fact.key),
                    value
                )
            })
        })
        .take(MAX_FACTS)
        .collect()
}

fn bounded_findings(report: &HealthScanReport) -> Vec<String> {
    let mut findings = report.findings.iter().collect::<Vec<_>>();
    findings.sort_by(|left, right| {
        right
            .severity
            .precedence()
            .cmp(&left.severity.precedence())
            .then_with(|| left.id.cmp(&right.id))
    });
    findings
        .into_iter()
        .take(MAX_FINDINGS)
        .map(|finding| {
            let evidence = if finding.evidence_fact_ids.is_empty() {
                "none".to_string()
            } else {
                finding
                    .evidence_fact_ids
                    .iter()
                    .map(|id| safe_token(id))
                    .collect::<Vec<_>>()
                    .join("|")
            };
            format!(
                "{}:{}:{}:evidence={}",
                safe_token(&finding.id),
                finding.severity.label(),
                health_message_token(finding.title_id),
                evidence
            )
        })
        .collect()
}

fn bounded_unavailable(report: &HealthScanReport) -> Vec<String> {
    report
        .unavailable
        .iter()
        .take(MAX_UNAVAILABLE)
        .map(unavailable_token)
        .collect()
}

fn unavailable_token(item: &UnavailableCollector) -> String {
    format!(
        "{:?}:{:?}:{}",
        item.collector,
        item.reason,
        item.severity.label()
    )
}

fn numeric_value(value: &HealthFactValue) -> Option<String> {
    match value {
        HealthFactValue::Integer(value) => Some(value.to_string()),
        HealthFactValue::Unsigned(value) => Some(value.to_string()),
        HealthFactValue::Float(value) => Some(format!("{value:.3}")),
        HealthFactValue::String(_) | HealthFactValue::Bool(_) => None,
    }
}

fn health_message_token(id: HealthMessageId) -> &'static str {
    match id {
        HealthMessageId::HealthFindingPlatformUnsupported => "HealthFindingPlatformUnsupported",
        HealthMessageId::HealthFindingCoreCollectorUnavailable => {
            "HealthFindingCoreCollectorUnavailable"
        }
        HealthMessageId::HealthFindingCpuLoadHigh => "HealthFindingCpuLoadHigh",
        HealthMessageId::HealthFindingMemoryAvailableLow => "HealthFindingMemoryAvailableLow",
        HealthMessageId::HealthFindingSwapPressure => "HealthFindingSwapPressure",
        HealthMessageId::HealthFindingDiskHigh => "HealthFindingDiskHigh",
        HealthMessageId::HealthFindingRecentOom => "HealthFindingRecentOom",
        HealthMessageId::HealthFindingKernelPanic => "HealthFindingKernelPanic",
        HealthMessageId::HealthFindingServiceFailed => "HealthFindingServiceFailed",
        HealthMessageId::HealthFindingServiceInactive => "HealthFindingServiceInactive",
        _ => "HealthMessage",
    }
}

fn safe_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | ':' | '#' | '/') {
                ch
            } else {
                '_'
            }
        })
        .take(96)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::diagnostics::health::{
        HealthCollector, HealthFact, HealthFactCategory, HealthFactSource, HealthFinding,
        HealthFindingCategory, HealthSeverity, HealthUnavailableReason,
    };

    #[test]
    fn health_context_hint_contains_only_bounded_facts() {
        let mut report = HealthScanReport::new("health-1", 0);
        report.overall_severity = HealthSeverity::Warning;
        report.health_score = Some(62);
        report.facts = vec![
            fact("memory.available_ratio", HealthFactValue::Float(0.08)),
            fact(
                "collector.stdout",
                HealthFactValue::String("full df output /tmp/cosh-secret".to_string()),
            ),
        ];
        report.findings = vec![HealthFinding {
            id: "J06".to_string(),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: vec!["memory.available_ratio".to_string()],
            suggested_try_ids: Vec::new(),
        }];
        report.unavailable.push(UnavailableCollector {
            collector: HealthCollector::KernelSignal,
            reason: HealthUnavailableReason::PermissionDenied,
            severity: HealthSeverity::Unavailable,
            elapsed_ms: 4,
        });

        let hint = health_context_hint(&report).expect("context hint");

        assert!(hint.contains("scan_id=health-1"), "{hint}");
        assert!(hint.contains("overall_severity=warning"), "{hint}");
        assert!(
            hint.contains("memory.available_ratio:memory.available_ratio=0.080"),
            "{hint}"
        );
        assert!(hint.contains("HealthFindingMemoryAvailableLow"), "{hint}");
        assert!(hint.contains("evidence=memory.available_ratio"), "{hint}");
        assert!(
            hint.contains("KernelSignal:PermissionDenied:unavailable"),
            "{hint}"
        );
        assert!(hint.contains("bounded_facts_only=true"), "{hint}");
        assert!(!hint.contains("full df output"), "{hint}");
        assert!(!hint.contains("/tmp/cosh-secret"), "{hint}");
    }

    fn fact(key: &str, value: HealthFactValue) -> HealthFact {
        HealthFact {
            id: key.to_string(),
            category: HealthFactCategory::Memory,
            key: key.to_string(),
            value,
            unit: None,
            source: HealthFactSource::Fixture,
            elapsed_ms: 0,
        }
    }
}
