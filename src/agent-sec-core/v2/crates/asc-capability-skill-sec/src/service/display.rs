//! Bounded caller-facing explanations preserve the Ledger show contract.

use crate::{DecisionAction, Manifest};
use serde_json::{Value, json};

pub(super) fn consistency(
    summary: &Value,
    latest: Option<&Manifest>,
    active: Option<&Manifest>,
    matches: Option<bool>,
) -> Value {
    let reason = summary["reasonCode"].as_str().unwrap_or_default();
    if reason == "user_block" {
        return json!("user decision block hides this skill");
    }
    if matches!(
        reason,
        "root_drift"
            | "tampered"
            | "latest_risk_fallback_to_previous"
            | "latest_risk_pending_decision"
    ) {
        return summary["message"].clone();
    }
    let latest_id = latest.map(|m| m.version_id.as_str());
    let active_id = active.map(|m| m.version_id.as_str());
    if latest_id != active_id {
        if let Some(decision) = active.and_then(|m| m.user_decision.as_ref())
            && decision.action != DecisionAction::Block
        {
            let name = serde_json::to_value(decision.action).unwrap_or(Value::Null);
            return json!(format!(
                "user decision {} pins active version {} instead of latest {}",
                name.as_str().unwrap_or("allow"),
                active_id.unwrap_or("none"),
                latest_id.unwrap_or("none")
            ));
        }
        let Some(latest) = latest else {
            return json!("no signed manifest snapshot is available");
        };
        let status = serde_json::to_value(latest.scan_status).unwrap_or(Value::Null);
        return active_id.map_or_else(
            || {
                json!(format!(
                    "activationPolicy pass_warn_only hides latest version {} with scanStatus {}",
                    latest.version_id,
                    status.as_str().unwrap_or("none")
                ))
            },
            |id| {
                json!(format!(
                    "activationPolicy pass_warn_only exposes version {id} instead of latest {}",
                    latest.version_id
                ))
            },
        );
    }
    if matches == Some(false) {
        return json!(format!(
            "root drift: current skill root does not match active snapshot {}",
            active_id.unwrap_or("none")
        ));
    }
    Value::Null
}

pub(super) fn message(summary: &Value, findings: &Value) -> Value {
    let Some(original) = summary["message"].as_str().filter(|s| !s.is_empty()) else {
        return Value::Null;
    };
    let evidence = findings_summary(findings);
    if !matches!(
        summary["reasonCode"].as_str(),
        Some("latest_risk_fallback_to_previous" | "latest_risk_pending_decision")
    ) {
        return json!(evidence.map_or_else(
            || original.into(),
            |text| format!("{original} Latest findings: {text}.")
        ));
    }
    let latest = summary["latestVersionId"].as_str().unwrap_or("none");
    let status = summary["latestStatus"].as_str().unwrap_or("unknown");
    let active = summary["activeVersionId"]
        .as_str()
        .filter(|s| !s.is_empty());
    let clause = active.map_or_else(
        || "no active safe version is exposed yet".into(),
        |id| format!("current active version is {id}"),
    );
    let action = active.map_or_else(|| "Review hidden latest with export --version latest, then decide: block or allow after review.".into(), |id| format!("Review hidden latest with export --version latest, then decide: block, rollback --version {id}, or allow after review."));
    let mut parts = vec![format!(
        "Latest version {latest} is {status} and is not exposed; {clause}."
    )];
    if let Some(evidence) = evidence {
        parts.push(format!("Latest findings: {evidence}."));
    }
    parts.push(action);
    json!(parts.join(" "))
}

fn findings_summary(value: &Value) -> Option<String> {
    let mut items: Vec<_> = value
        .as_array()?
        .iter()
        .filter(|v| v.is_object())
        .enumerate()
        .collect();
    if items.is_empty() {
        return None;
    }
    items.sort_by_key(|(index, finding)| (rank(finding), *index));
    let mut lines: Vec<_> = items
        .iter()
        .take(3)
        .map(|(_, finding)| {
            let level = field(finding, "level")
                .or_else(|| field(finding, "severity"))
                .unwrap_or_else(|| "unknown".into());
            let location = field(finding, "file").or_else(|| field(finding, "path"));
            let rule = field(finding, "rule")
                .or_else(|| field(finding, "rule_id"))
                .or_else(|| field(finding, "title"));
            let message = field(finding, "message").or_else(|| field(finding, "description"));
            let mut parts = vec![format!("[{level}]")];
            parts.extend(location);
            parts.extend(rule);
            let mut text = parts.join(" ");
            if let Some(message) = message {
                text.push_str(": ");
                text.push_str(&message);
            }
            if text.chars().count() > 160 {
                text = format!(
                    "{}...",
                    text.chars().take(157).collect::<String>().trim_end()
                );
            }
            text
        })
        .collect();
    if items.len() > 3 {
        lines.push(format!("+{} more findings", items.len() - 3));
    }
    Some(lines.join("; "))
}

fn rank(finding: &Value) -> u8 {
    let level = field(finding, "level")
        .or_else(|| field(finding, "severity"))
        .unwrap_or_default();
    match level.to_lowercase().as_str() {
        "deny" | "critical" | "high" => 0,
        "warn" | "warning" | "medium" | "low" => 1,
        "pass" | "info" | "informational" => 2,
        _ => 3,
    }
}

fn field(finding: &Value, key: &str) -> Option<String> {
    let value = finding
        .get(key)
        .filter(|v| !v.is_null())
        .or_else(|| finding.get("metadata").and_then(|v| v.get(key)))?;
    if value.is_null() {
        return None;
    }
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}
