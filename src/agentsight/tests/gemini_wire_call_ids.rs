//! Wire call IDs must survive parsing and semantic delivery in both directions.
#![cfg(target_os = "linux")]

use agentsight::analyzer::message::MessageParser;
use agentsight::analyzer::{AnalysisResult, HttpRecord};
use agentsight::genai::semantic::LLMCall;
use agentsight::genai::{GenAIBuilder, GenAISemanticEvent, MessagePart};
use agentsight::response_map::ResponseSessionMapper;
use serde_json::{Value, json};
use std::collections::HashMap;

fn exchange(with_ids: bool, stream: bool) -> LLMCall {
    exchange_mode(with_ids, stream, false)
}

fn exchange_mode(with_ids: bool, stream: bool, raw: bool) -> LLMCall {
    let mut first = json!({"name":"lookup", "args":{"key":"first"}});
    let mut second = json!({"name":"lookup", "args":{"key":"second"}});
    let mut first_result = json!({"name":"lookup", "response":{"value":"first value"}});
    let mut second_result = json!({"name":"lookup", "response":{"value":"second value"}});
    if with_ids {
        first["id"] = json!("call-first");
        second["id"] = json!("call-second");
        first_result["id"] = json!("call-first");
        second_result["id"] = json!("call-second");
    }
    let request = json!({"contents":[
        {"role":"user", "parts":[{"text":"Look up both keys"}]},
        {"role":"model", "parts":[{"functionCall":first}, {"functionCall":second}]},
        {"role":"user", "parts":[{"functionResponse":second_result}, {"functionResponse":first_result}]}
    ]});
    let response = json!({"candidates":[{"index":0, "content":{"role":"model", "parts":[
        {"functionCall":first}, {"functionCall":second}
    ]}, "finishReason":"STOP"}]});
    let response = if stream { json!([response]) } else { response };
    let path = if stream {
        "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    } else {
        "/v1beta/models/gemini-2.5-flash:generateContent"
    };
    let parsed = MessageParser::new()
        .parse_by_path(path, Some(&request), Some(&response))
        .expect("Gemini exchange parses");
    // Compact delivery has already released raw bodies. The typed message is
    // the only source of tool identity for persistence and downstream exports.
    let http = HttpRecord {
        timestamp_ns: 1_800_000_000_000_000_000,
        pid: 424242,
        comm: "gemini-id-fixture".into(),
        method: "POST".into(),
        path: path.into(),
        status_code: 200,
        request_headers: "{}".into(),
        request_body: raw.then(|| request.to_string()),
        response_headers: "{}".into(),
        // HttpRecord carries the aggregator's JSON array for SSE chunks.
        response_body: raw.then(|| response.to_string()),
        duration_ns: 1_000_000,
        first_output_timestamp_ns: None,
        is_sse: stream,
        sse_event_count: usize::from(stream),
    };
    let (delivery, _) = GenAIBuilder::new().build_with_pending(
        &if raw {
            vec![AnalysisResult::Http(http)]
        } else {
            vec![AnalysisResult::Http(http), AnalysisResult::Message(parsed)]
        },
        &ResponseSessionMapper::disabled(),
        &HashMap::from([(424242, "fixture-agent".to_string())]),
    );
    delivery
        .events
        .into_iter()
        .find_map(|event| match event {
            GenAISemanticEvent::LLMCall(call) => Some(call),
            _ => None,
        })
        .expect("semantic call")
}

fn assert_ids(call: &LLMCall) {
    let calls: Vec<_> = call
        .request
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => Some((
                id.as_deref(),
                name.as_str(),
                arguments.as_ref().unwrap()["key"].clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            (Some("call-first"), "lookup", json!("first")),
            (Some("call-second"), "lookup", json!("second"))
        ]
    );
    let results: Vec<_> = call
        .request
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match part {
            MessagePart::ToolCallResponse { id, response } => {
                // The raw normalization keeps JSON results as encoded text;
                // typed compact delivery already carries the object.
                let payload = response
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .unwrap_or_else(|| response.clone());
                Some((id.as_deref(), payload["value"].clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![
            (Some("call-second"), json!("second value")),
            (Some("call-first"), json!("first value"))
        ]
    );
    let output: Value = serde_json::to_value(&call.response.messages).unwrap();
    assert_eq!(output[0]["parts"][0]["id"], "call-first");
    assert_eq!(output[0]["parts"][1]["id"], "call-second");
}

#[test]
fn same_name_calls_and_reordered_results_keep_wire_ids() {
    assert_ids(&exchange(true, false));
}

#[test]
fn streamed_call_ids_survive_compact_delivery() {
    assert_ids(&exchange(true, true));
}

#[test]
fn legacy_idless_parts_remain_idless() {
    let call = exchange(false, false);
    let parts = call
        .request
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .chain(
            call.response
                .messages
                .iter()
                .flat_map(|message| &message.parts),
        );
    let mut count = 0;
    for part in parts {
        match part {
            MessagePart::ToolCall { id, .. } | MessagePart::ToolCallResponse { id, .. } => {
                count += 1;
                assert!(id.is_none());
            }
            _ => {}
        }
    }
    assert_eq!(count, 6);
}

#[test]
fn raw_json_and_sse_paths_preserve_wire_ids() {
    for stream in [false, true] {
        assert_ids(&exchange_mode(true, stream, true));
    }
}
