//! Provider-side results belong to the response that contains the call.
#![cfg(target_os = "linux")]
use agentsight::atif::{convert_session_to_atif, convert_trace_to_atif};
use agentsight::genai::semantic::{InputMessage, MessagePart, OutputMessage};
use agentsight::storage::sqlite::genai::TraceEventDetail;
use serde_json::json;
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

fn call(id: &str) -> MessagePart {
    MessagePart::ToolCall {
        id: Some(id.into()),
        name: "web_search".into(),
        arguments: Some(json!({"query":"fixture"})),
    }
}
fn result(id: &str, text: &str, error: bool) -> MessagePart {
    MessagePart::ToolCallResponse {
        id: Some(id.into()),
        response: json!({"content":[{"type":"text","text":text}],"is_error":error}),
    }
}
fn response_event(parts: Vec<MessagePart>) -> TraceEventDetail {
    let mut row = event(1);
    row.output_messages = Some(
        serde_json::to_string(&[OutputMessage {
            role: "assistant".into(),
            parts,
            name: None,
            finish_reason: Some("end_turn".into()),
        }])
        .unwrap(),
    );
    row
}
#[test]
fn terminal_provider_result_survives_trace_and_session_export() {
    for session in [false, true] {
        let row = response_event(vec![
            call("search-1"),
            result("search-1", "fixture source", false),
            MessagePart::Text {
                content: "answer".into(),
            },
        ]);
        let doc = if session {
            convert_session_to_atif("fixture", vec![row])
        } else {
            convert_trace_to_atif("fixture", vec![row])
        }
        .unwrap();
        let agent = doc
            .steps
            .iter()
            .find(|step| step.tool_calls.is_some())
            .unwrap();
        let results = &agent
            .observation
            .as_ref()
            .expect("inline evidence retained")
            .results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_call_id.as_deref(), Some("search-1"));
        assert_eq!(results[0].content, Some(json!("fixture source")));
        assert_eq!(results[0].extra.as_ref().unwrap()["is_error"], false);
        assert_eq!(agent.message, "answer");
    }
}
#[test]
fn inline_and_client_results_share_ownership_without_duplicates() {
    let first = response_event(vec![
        call("server"),
        call("client"),
        result("server", "Error: failed", true),
    ]);
    let mut next = event(2);
    next.input_messages = Some(
        serde_json::to_string(&[InputMessage {
            role: "user".into(),
            parts: vec![
                result("server", "replayed copy", false),
                result("client", "local success", false),
            ],
            name: None,
        }])
        .unwrap(),
    );
    let doc = convert_trace_to_atif("fixture", vec![first, next]).unwrap();
    let results = &doc.steps[0].observation.as_ref().unwrap().results;
    assert_eq!(results.len(), 2);
    let own = results
        .iter()
        .find(|r| r.source_call_id.as_deref() == Some("server"))
        .unwrap();
    assert_eq!(own.content, Some(json!("Error: failed")));
    assert_eq!(own.extra.as_ref().unwrap()["is_error"], true);
    let local = results
        .iter()
        .find(|r| r.source_call_id.as_deref() == Some("client"))
        .unwrap();
    assert_eq!(local.content, Some(json!("local success")));
}
#[test]
fn foreign_inline_results_do_not_consume_client_slots() {
    let first = response_event(vec![call("client"), result("foreign", "unrelated", false)]);
    let mut next = event(2);
    next.input_messages = Some(
        serde_json::to_string(&[InputMessage {
            role: "tool".into(),
            parts: vec![MessagePart::ToolCallResponse {
                id: None,
                response: json!("local success"),
            }],
            name: None,
        }])
        .unwrap(),
    );
    let doc = convert_trace_to_atif("fixture", vec![first, next]).unwrap();
    let results = &doc.steps[0].observation.as_ref().unwrap().results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_call_id.as_deref(), Some("client"));
    assert_eq!(results[0].content, Some(json!("local success")));
}

#[path = "common/atif_inline_fixture.rs"]
mod atif_inline_fixture;

#[test]
fn typed_next_event_fills_client_results_without_replacing_inline_evidence() {
    let first = response_event(vec![
        call("server"),
        call("client"),
        result("server", "inline source", false),
    ]);
    let mut semantic = atif_inline_fixture::native_call();
    semantic.request.messages = vec![
        InputMessage {
            role: "assistant".into(),
            parts: vec![call("server"), call("client")],
            name: None,
        },
        InputMessage {
            role: "user".into(),
            parts: vec![
                result("server", "replayed source", false),
                result("client", "client source", false),
            ],
            name: None,
        },
    ];
    semantic.response.messages.clear();
    let mut next = event(2);
    next.event_json = Some(
        serde_json::to_string(&agentsight::genai::GenAISemanticEvent::LLMCall(semantic)).unwrap(),
    );
    let doc = convert_trace_to_atif("fixture", vec![first, next]).unwrap();
    let results = &doc.steps[0].observation.as_ref().unwrap().results;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].source_call_id.as_deref(), Some("server"));
    assert_eq!(results[0].content, Some(json!("inline source")));
    assert_eq!(results[1].source_call_id.as_deref(), Some("client"));
    assert_eq!(results[1].content, Some(json!("client source")));
}

#[test]
fn captured_anthropic_server_result_survives_semantic_event_export() {
    let call = atif_inline_fixture::native_call();
    let mut row = event(1);
    row.event_json =
        Some(serde_json::to_string(&agentsight::genai::GenAISemanticEvent::LLMCall(call)).unwrap());
    let doc = convert_trace_to_atif("fixture", vec![row]).unwrap();
    let step = doc
        .steps
        .iter()
        .find(|step| step.tool_calls.is_some())
        .unwrap();
    let observation = step.observation.as_ref().expect("captured result exported");
    assert_eq!(observation.results.len(), 1);
    assert_eq!(
        observation.results[0].source_call_id.as_deref(),
        Some("srvtoolu_search")
    );
    assert!(
        observation.results[0]
            .content
            .as_ref()
            .unwrap()
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
}
