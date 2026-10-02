//! Projection of health reports into the stable diagnostic bundle schema.

use serde_json::{json, Value};

use super::redact;
use crate::diagnostics::health::{
    HealthCollector, HealthFactValue, HealthFindingCategory, HealthScanReport, HealthTryKind,
    HealthUnavailableReason,
};

pub(super) fn health_json(report: HealthScanReport) -> Value {
    let facts = report
        .facts
        .into_iter()
        .map(|fact| {
            let value = match fact.value {
                HealthFactValue::String(value) => Value::String(redact(&value)),
                HealthFactValue::Integer(value) => json!(value),
                HealthFactValue::Unsigned(value) => json!(value),
                HealthFactValue::Float(value) => json!(value),
                HealthFactValue::Bool(value) => json!(value),
            };
            json!({
                "id": redact(&fact.id),
                "key": redact(&fact.key),
                "value": value,
                "unit": fact.unit.as_deref().map(redact),
            })
        })
        .collect::<Vec<_>>();
    let findings = report
        .findings
        .into_iter()
        .map(|finding| {
            json!({
                "id": redact(&finding.id),
                "severity": finding.severity.label(),
                "category": finding_category(finding.category),
                "detail_args": finding.detail_args.into_iter().map(|(key, value)| (redact(&key), redact(&value))).collect::<std::collections::BTreeMap<_, _>>(),
                "evidence_fact_ids": finding.evidence_fact_ids.into_iter().map(|value| redact(&value)).collect::<Vec<_>>(),
                "suggested_try_ids": finding.suggested_try_ids.into_iter().map(|value| redact(&value)).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let unavailable = report
        .unavailable
        .into_iter()
        .map(|item| {
            json!({
                "collector": collector_label(item.collector),
                "reason": unavailable_reason(item.reason),
                "severity": item.severity.label(),
                "elapsed_ms": item.elapsed_ms,
            })
        })
        .collect::<Vec<_>>();
    let try_items = report
        .try_items
        .into_iter()
        .map(|item| {
            json!({
                "id": redact(&item.id),
                "kind": try_kind(item.kind),
                "command": item.command.as_deref().map(redact),
                "score": item.score,
                "finding_id": redact(&item.finding_id),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "scan_id": redact(&report.scan_id),
        "host": report.host.as_deref().map(redact),
        "role": report.role.as_deref().map(redact),
        "started_at_ms": report.started_at_ms,
        "elapsed_ms": report.elapsed_ms,
        "overall_severity": report.overall_severity.label(),
        "health_score": report.health_score,
        "checks_done": report.checks_done.iter().map(|value| redact(value)).collect::<Vec<_>>(),
        "facts": facts,
        "findings": findings,
        "unavailable": unavailable,
        "try_items": try_items,
    })
}

fn finding_category(category: HealthFindingCategory) -> &'static str {
    match category {
        HealthFindingCategory::RootCause => "root_cause",
        HealthFindingCategory::Anomaly => "anomaly",
        HealthFindingCategory::Observation => "observation",
        HealthFindingCategory::CollectionGap => "collection_gap",
    }
}

fn collector_label(collector: HealthCollector) -> &'static str {
    match collector {
        HealthCollector::Host => "host",
        HealthCollector::Cpu => "cpu",
        HealthCollector::Memory => "memory",
        HealthCollector::Disk => "disk",
        HealthCollector::KernelSignal => "kernel_signal",
        HealthCollector::ConfiguredService => "configured_service",
        HealthCollector::Provider => "provider",
        HealthCollector::Config => "config",
        HealthCollector::Hooks => "hooks",
        HealthCollector::Pty => "pty",
        HealthCollector::Permissions => "permissions",
    }
}

fn unavailable_reason(reason: HealthUnavailableReason) -> &'static str {
    match reason {
        HealthUnavailableReason::Unsupported => "unsupported",
        HealthUnavailableReason::PermissionDenied => "permission_denied",
        HealthUnavailableReason::CommandMissing => "command_missing",
        HealthUnavailableReason::Timeout => "timeout",
        HealthUnavailableReason::ParseError => "parse_error",
    }
}

fn try_kind(kind: HealthTryKind) -> &'static str {
    match kind {
        HealthTryKind::AskAgent => "ask_agent",
        HealthTryKind::DisplayCommand => "display_command",
    }
}
