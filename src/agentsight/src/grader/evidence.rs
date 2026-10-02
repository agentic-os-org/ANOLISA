//! Evidence reference helpers for grader dimensions and findings.

use super::input::EvaluationInput;
use super::types::{EvaluationRef, EvidenceDeeplink, EvidenceTarget, EvidenceType};
use crate::storage::sqlite::InterruptionRecord;
use crate::storage::sqlite::genai::TraceEventDetail;

pub(super) fn has_usable_output(event: &TraceEventDetail) -> bool {
    if let Some(raw) = event.output_messages.as_deref() {
        return raw_contains_content(raw);
    }

    event.output_tokens > 0
}

pub(super) fn looks_like_tool_failure(event: &TraceEventDetail) -> bool {
    event
        .output_messages
        .as_deref()
        .is_some_and(contains_tool_failure_signal)
        || event
            .input_messages
            .as_deref()
            .is_some_and(contains_structured_tool_failure_signal)
}

fn contains_tool_failure_signal(raw: &str) -> bool {
    if contains_structured_tool_failure_signal(raw) {
        return true;
    }

    let text = raw.to_ascii_lowercase();
    text.contains("tool_call_response")
        && (text.contains("\"error\"")
            || text.contains("traceback")
            || text.contains("exception")
            || text.contains("failed"))
}

fn contains_structured_tool_failure_signal(raw: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(raw)
        .map(|value| json_has_tool_failure(&value))
        .unwrap_or(false)
}

fn json_has_tool_failure(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(json_has_tool_failure),
        serde_json::Value::Object(map) => {
            let is_tool_response = map
                .get("type")
                .and_then(|value| value.as_str())
                .is_some_and(|kind| matches!(kind, "tool_call_response" | "tool_result"))
                || map.contains_key("tool_call_response");

            if is_tool_response && tool_response_has_error(map) {
                return true;
            }

            map.values().any(json_has_tool_failure)
        }
        _ => false,
    }
}

fn tool_response_has_error(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    if let Some(is_error) = map.get("is_error").and_then(|value| value.as_bool()) {
        return is_error;
    }

    ["response", "content", "error"]
        .iter()
        .any(|key| map.get(*key).is_some_and(value_has_error_signal))
}

fn value_has_error_signal(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text_has_error_signal(text),
        serde_json::Value::Array(items) => items.iter().any(value_has_error_signal),
        serde_json::Value::Object(map) => {
            if let Some(is_error) = map.get("is_error").and_then(|value| value.as_bool()) {
                return is_error;
            }

            map.values().any(value_has_error_signal)
        }
        _ => false,
    }
}

fn text_has_error_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("traceback")
        || lower.contains("exception")
        || lower.contains("failed")
        || lower.contains("exit code 1")
        || lower.contains("no such file or directory")
        || lower.contains("permission denied")
        || lower.contains("command not found")
        || lower.contains("\"error\"")
        || lower.contains("error:")
}

pub(super) fn first_event_refs(input: &EvaluationInput, label: &str) -> Vec<EvaluationRef> {
    input
        .events
        .first()
        .map(|event| vec![genai_ref(&input.target_id, event, label)])
        .unwrap_or_default()
}

pub(super) fn genai_ref(
    conversation_id: &str,
    event: &TraceEventDetail,
    label: &str,
) -> EvaluationRef {
    let id = event
        .call_id
        .clone()
        .unwrap_or_else(|| format!("genai-event-{}", event.id));
    EvaluationRef {
        evidence_type: EvidenceType::GenaiEvent,
        id,
        label: label.to_string(),
        severity: event.interruption_type.clone(),
        target: EvidenceTarget {
            conversation_id: conversation_id.to_string(),
            trace_id: event.trace_id.clone(),
            call_id: event.call_id.clone(),
            step_id: None,
        },
        deeplink: Some(EvidenceDeeplink {
            route: "/atif".to_string(),
            query: serde_json::json!({
                "type": "conversation",
                "id": conversation_id,
                "highlight_call_id": &event.call_id,
            }),
        }),
        metadata: serde_json::json!({
            "event_id": event.id,
            "model": &event.model,
            "status": &event.status,
        }),
    }
}

pub(super) fn interruption_ref(
    conversation_id: &str,
    record: &InterruptionRecord,
) -> EvaluationRef {
    EvaluationRef {
        evidence_type: EvidenceType::Interruption,
        id: record.interruption_id.clone(),
        label: record.interruption_type.clone(),
        severity: Some(record.severity.clone()),
        target: EvidenceTarget {
            conversation_id: conversation_id.to_string(),
            trace_id: record.trace_id.clone(),
            call_id: record.call_id.clone(),
            step_id: None,
        },
        deeplink: Some(EvidenceDeeplink {
            route: "/atif".to_string(),
            query: serde_json::json!({
                "type": "conversation",
                "id": conversation_id,
                "highlight_call_id": &record.call_id,
                "interruption_id": &record.interruption_id,
            }),
        }),
        metadata: serde_json::json!({
            "occurred_at_ns": record.occurred_at_ns,
            "detail": &record.detail,
            "resolved": record.resolved,
        }),
    }
}

fn raw_contains_content(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "[]" || trimmed == "{}" {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .map(|value| json_has_text(&value))
        .unwrap_or_else(|_| !trimmed.is_empty())
}

fn json_has_text(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Array(values) => values.iter().any(json_has_text),
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "content" | "text" | "message" | "output" | "response"
            ) && json_has_text(value)
                || key == "parts" && json_has_text(value)
                || key == "Text" && json_has_text(value)
                || key == "Reasoning" && json_has_text(value)
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        input_messages: Option<&str>,
        output_messages: Option<&str>,
        event_json: Option<&str>,
    ) -> TraceEventDetail {
        TraceEventDetail {
            id: 1,
            call_id: Some("call-1".to_string()),
            start_timestamp_ns: 100,
            end_timestamp_ns: Some(200),
            model: Some("test-model".to_string()),
            input_tokens: 10,
            output_tokens: 10,
            total_tokens: 20,
            input_messages: input_messages.map(str::to_string),
            output_messages: output_messages.map(str::to_string),
            system_instructions: None,
            agent_name: Some("agent".to_string()),
            process_name: None,
            pid: Some(1),
            user_query: Some("do work".to_string()),
            event_json: event_json.map(str::to_string),
            trace_id: Some("trace-1".to_string()),
            conversation_id: Some("conv-1".to_string()),
            cache_read_tokens: None,
            status: Some("complete".to_string()),
            interruption_type: None,
        }
    }

    #[test]
    fn ignores_historical_tool_failure_text_in_raw_event_json() {
        let event = event(
            Some(r#"[{"role":"user","content":"write a Linux troubleshooting guide"}]"#),
            Some(r#"[{"role":"assistant","content":"step 1: check the process"}]"#),
            Some(
                r#"{"request":{"messages":[{"role":"user","content":"tool_call_response: {\"error\":\"failed\", \"traceback\":\"FileNotFoundError\"}"}]},"response":{"messages":[{"role":"assistant","content":"step 1: check the process"}]},"error":null}"#,
            ),
        );

        assert!(!looks_like_tool_failure(&event));
    }

    #[test]
    fn detects_tool_failure_in_current_assistant_output() {
        let event = event(
            Some(r#"[{"role":"user","content":"run the tool"}]"#),
            Some(
                r#"[{"role":"assistant","content":"tool_call_response: {\"error\":\"failed to read config\", \"traceback\":\"FileNotFoundError\"}"}]"#,
            ),
            None,
        );

        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn detects_tool_failure_in_structured_input_tool_result() {
        let event = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":{"content":"Exit code 1\ncat: /tmp/missing.txt: No such file or directory","is_error":true}}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"file missing"}]}]"#),
            None,
        );

        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn ignores_structured_tool_result_with_explicit_non_error_flag() {
        let event = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":{"content":"5 passed, 2 failed","is_error":false}}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"tests completed"}]}]"#),
            None,
        );

        assert!(!looks_like_tool_failure(&event));
    }

    #[test]
    fn detects_tool_failure_from_nested_is_error_flag() {
        let event = event(
            Some(
                r#"[{"role":"user","parts":[{"type":"tool_call_response","id":"toolu_1","response":{"is_error":true}}]}]"#,
            ),
            Some(r#"[{"role":"assistant","parts":[{"type":"text","content":"tool failed"}]}]"#),
            None,
        );

        assert!(looks_like_tool_failure(&event));
    }

    #[test]
    fn ignores_tool_failure_text_in_user_prompt() {
        let event = event(
            Some(
                r#"[{"role":"user","content":"please quote this: tool_call_response: {\"error\":\"failed\", \"traceback\":\"FileNotFoundError\"}"}]"#,
            ),
            Some(r#"[{"role":"assistant","content":"quoted text omitted"}]"#),
            None,
        );

        assert!(!looks_like_tool_failure(&event));
    }
}
