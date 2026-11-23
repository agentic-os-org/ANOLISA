//! Exported observations must identify the selected original tool call.
#![cfg(target_os = "linux")]

use agentsight::atif::convert_trace_to_atif;
use agentsight::genai::semantic::{InputMessage, MessagePart, OutputMessage};
use agentsight::storage::sqlite::genai::TraceEventDetail;
use agentsight_atif::AtifTrajectory;
use serde_json::{Value, json};

fn event(id: i64) -> TraceEventDetail {
    TraceEventDetail {
        id,
        call_id: Some(format!("synthetic-llm-call-{id}")),
        start_timestamp_ns: 1_800_000_000_000_000_000 + id * 1_000_000,
        end_timestamp_ns: Some(1_800_000_000_000_000_000 + id * 1_000_000 + 500_000),
        model: Some("synthetic-model".into()),
        input_tokens: 1,
        output_tokens: 1,
        total_tokens: 2,
        input_messages: None,
        output_messages: None,
        system_instructions: None,
        agent_name: Some("SyntheticAgent".into()),
        process_name: Some("synthetic-agent".into()),
        pid: None,
        user_query: None,
        event_json: None,
        trace_id: Some(format!("synthetic-response-{id}")),
        conversation_id: Some("synthetic-conversation".into()),
        cache_read_tokens: None,
        status: Some("complete".into()),
        interruption_type: None,
    }
}

fn exported(calls: &[&str], responses: &[(Option<&str>, &str)]) -> AtifTrajectory {
    let mut first = event(1);
    first.output_messages = Some(
        serde_json::to_string(&vec![OutputMessage {
            role: "assistant".into(),
            parts: calls
                .iter()
                .map(|id| MessagePart::ToolCall {
                    id: Some((*id).into()),
                    name: "synthetic_echo".into(),
                    arguments: Some(json!({"marker": "inert fixture data"})),
                })
                .collect(),
            name: None,
            finish_reason: Some("tool_calls".into()),
        }])
        .expect("synthetic output columns serialize"),
    );
    let mut second = event(2);
    second.input_messages = Some(
        serde_json::to_string(&vec![InputMessage {
            role: "user".into(),
            parts: responses
                .iter()
                .map(|(id, marker)| MessagePart::ToolCallResponse {
                    id: id.map(str::to_string),
                    response: Value::String((*marker).into()),
                })
                .collect(),
            name: None,
        }])
        .expect("synthetic input columns serialize"),
    );
    let document = convert_trace_to_atif("synthetic-trace", vec![first, second])
        .expect("synthetic trace export succeeds");
    let serialized = serde_json::to_string(&document).expect("shared ATIF serializes");
    agentsight_atif::validate_trajectory_str(&serialized)
        .expect("shared ATIF survives normal serialization and validation")
}

fn paired_result(owner: &str, echo: &str) {
    let document = exported(&[owner], &[(Some(echo), "Synthetic result marker")]);
    let step = &document.steps[0];
    let calls = step.tool_calls.as_ref().expect("original call retained");
    let results = &step.observation.as_ref().expect("result retained").results;
    assert_eq!(calls.len(), 1);
    assert_eq!(results.len(), 1);
    assert_eq!(calls[0].tool_call_id, owner);
    assert_eq!(results[0].source_call_id.as_deref(), Some(owner));
    assert_eq!(results[0].content, Some(json!("Synthetic result marker")));
}

#[test]
fn exact_tool_result_reference_is_preserved() {
    paired_result("call_cb113b8e", "call_cb113b8e");
}

#[test]
fn measured_separator_stripped_echo_writes_original_reference() {
    paired_result("call_cb113b8e", "callcb113b8e");
}

#[test]
fn unique_case_folded_echo_writes_original_reference() {
    paired_result("call_cb113b8e", "CALL_CB113B8E");
}

#[test]
fn ambiguous_alias_does_not_invent_an_observation() {
    let document = exported(
        &["call_ab", "call_a_b"],
        &[(Some("callab"), "Unattributed synthetic marker")],
    );
    assert!(document.steps[0].observation.is_none());
}

#[test]
fn foreign_id_does_not_consume_idless_sibling_slot() {
    let document = exported(
        &["call_cb113b8e"],
        &[
            (Some("foreign-id"), "Foreign synthetic marker"),
            (None, "Current synthetic marker"),
        ],
    );
    let results = &document.steps[0].observation.as_ref().unwrap().results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_call_id.as_deref(), Some("call_cb113b8e"));
    assert_eq!(results[0].content, Some(json!("Current synthetic marker")));
}
