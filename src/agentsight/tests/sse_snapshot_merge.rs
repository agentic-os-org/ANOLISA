//! Drained SSE snapshots keep refusal text and whole tool calls.
#![cfg(target_os = "linux")]

mod common;

use agentsight::genai::GenAIBuilder;
use agentsight::genai::semantic::{MessagePart, OutputMessage};
use agentsight::parser::sse::ParsedSseEvent;
use serde_json::{Value, json};
use std::rc::Rc;

fn drain(chunks: &[Value]) -> Vec<OutputMessage> {
    let events: Vec<_> = chunks
        .iter()
        .map(|chunk| {
            let bytes = serde_json::to_vec(chunk).unwrap();
            let len = bytes.len();
            ParsedSseEvent::new(
                None,
                None,
                None,
                0,
                len,
                Rc::new(common::make_ssl_event(123, 0x123, 0, bytes, "fixture")),
            )
        })
        .collect();
    let enrichment = GenAIBuilder::extract_sse_enrichment(&events).expect("SSE enrichment");
    serde_json::from_str(
        enrichment
            .output_messages
            .as_deref()
            .expect("merged output messages"),
    )
    .expect("valid output messages")
}

#[test]
fn cumulative_refusal_snapshots_replace_earlier_text() {
    let messages = drain(&[
        json!({"choices":[{"message":{"refusal":"I can't"}}]}),
        json!({"choices":[{"message":{"refusal":"I can't help with that."},"finish_reason":"stop"}]}),
    ]);

    let text: Vec<_> = messages[0]
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, ["I can't help with that."]);
}

#[test]
fn refusal_deltas_still_append_fragments() {
    let messages = drain(&[
        json!({"choices":[{"delta":{"refusal":"I can't help"}}]}),
        json!({"choices":[{"delta":{"refusal":" with that."},"finish_reason":"stop"}]}),
    ]);

    assert!(matches!(
        &messages[0].parts[0],
        MessagePart::Text { content } if content == "I can't help with that."
    ));
}

#[test]
fn openai_tool_call_snapshots_replace_whole_arguments() {
    let messages = drain(&[
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":r#"{"path":"#}}]}}]}),
        json!({"choices":[{"message":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":r#"{"path":"fixture"}"#}}]},"finish_reason":"tool_calls"}]}),
    ]);

    assert_eq!(messages[0].parts.len(), 1);
    match &messages[0].parts[0] {
        MessagePart::ToolCall {
            id,
            name,
            arguments,
        } => {
            assert_eq!(id.as_deref(), Some("call_1"));
            assert_eq!(name, "read_file");
            assert_eq!(
                arguments
                    .as_ref()
                    .and_then(|value| value.get("path"))
                    .and_then(Value::as_str),
                Some("fixture")
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
    assert_eq!(messages[0].finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn sysom_tool_use_snapshots_are_kept() {
    let messages = drain(&[
        json!({"choices":[{"message":{"tool_use":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{}"}}]}}]}),
        json!({"choices":[{"message":{"tool_use":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":r#"{"path":"fixture"}"#}}]},"finish_reason":"tool_calls"}]}),
    ]);

    assert_eq!(messages[0].parts.len(), 1);
    match &messages[0].parts[0] {
        MessagePart::ToolCall {
            id,
            name,
            arguments,
        } => {
            assert_eq!(id.as_deref(), Some("call_1"));
            assert_eq!(name, "read_file");
            assert_eq!(
                arguments
                    .as_ref()
                    .and_then(|value| value.get("path"))
                    .and_then(Value::as_str),
                Some("fixture")
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}
