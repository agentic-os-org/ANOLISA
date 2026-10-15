//! Responses tool identity survives message parsing and drain enrichment.
#![cfg(target_os = "linux")]
mod common;

use agentsight::analyzer::message::{MessageParser, ParsedApiMessage};
use agentsight::genai::GenAIBuilder;
use agentsight::genai::semantic::{MessagePart, OutputMessage};
use agentsight::parser::sse::ParsedSseEvent;
use serde_json::{Value, json};
use std::rc::Rc;

fn added(index: u64, suffix: &str) -> Value {
    json!({"type":"response.output_item.added", "output_index":index,
        "item":{"type":"function_call", "id":format!("fc_{suffix}"),
            "call_id":format!("call_{suffix}"), "name":format!("tool_{suffix}"), "arguments":""}})
}

fn delta(index: u64, suffix: &str, text: &str) -> Value {
    json!({"type":"response.function_call_arguments.delta", "output_index":index,
        "item_id":format!("fc_{suffix}"), "delta":text})
}

fn done(index: u64, suffix: &str, args: &str) -> Value {
    json!({"type":"response.function_call_arguments.done", "output_index":index,
        "item_id":format!("fc_{suffix}"), "arguments":args})
}

fn item_done(index: u64, suffix: &str, args: &str) -> Value {
    json!({"type":"response.output_item.done", "output_index":index,
        "item":{"type":"function_call", "id":format!("fc_{suffix}"),
            "call_id":format!("call_{suffix}"), "name":format!("tool_{suffix}"),
            "arguments":args}})
}

fn message_item_done(index: u64) -> Value {
    json!({"type":"response.output_item.done", "output_index":index,
        "item":{"type":"message", "id":format!("msg_{index}"),
            "content":[{"type":"output_text", "text":"hello"}]}})
}

fn parsed_calls(chunks: &[Value]) -> Vec<(String, String, Value)> {
    let parsed = MessageParser::new()
        .parse_by_path("/v1/responses", None, Some(&Value::Array(chunks.to_vec())))
        .unwrap();
    let ParsedApiMessage::OpenAICompletion {
        response: Some(response),
        ..
    } = parsed
    else {
        panic!("expected Responses message");
    };
    response.choices[0]
        .message
        .tool_calls
        .as_ref()
        .unwrap()
        .iter()
        .map(|call| {
            (
                call["id"].as_str().unwrap().to_string(),
                call["function"]["name"].as_str().unwrap().to_string(),
                serde_json::from_str(call["function"]["arguments"].as_str().unwrap())
                    .unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn drained_calls(chunks: &[Value]) -> Vec<(String, String, Value)> {
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
    let enrichment = GenAIBuilder::extract_sse_enrichment(&events).unwrap();
    let messages: Vec<OutputMessage> =
        serde_json::from_str(&enrichment.output_messages.unwrap()).unwrap();
    messages[0]
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => Some((
                id.clone().unwrap(),
                name.clone(),
                arguments.clone().unwrap_or(Value::Null),
            )),
            _ => None,
        })
        .collect()
}

fn check(chunks: Vec<Value>, expected: Vec<(&str, &str, Value)>) {
    let expected: Vec<_> = expected
        .into_iter()
        .map(|(id, name, args)| (id.to_string(), name.to_string(), args))
        .collect();
    assert_eq!(
        parsed_calls(&chunks),
        expected,
        "message parser call identity"
    );
    assert_eq!(
        drained_calls(&chunks),
        expected,
        "drain enrichment call identity"
    );
}

#[test]
fn interleaved_deltas_keep_each_calls_arguments() {
    check(
        vec![
            added(0, "a"),
            added(1, "b"),
            delta(0, "a", "{\"a\":"),
            delta(1, "b", "{\"b\":"),
            delta(0, "a", "1}"),
            delta(1, "b", "2}"),
        ],
        vec![
            ("call_a", "tool_a", json!({"a":1})),
            ("call_b", "tool_b", json!({"b":2})),
        ],
    );
}

#[test]
fn interleaved_done_events_finalize_their_own_calls() {
    check(
        vec![
            added(0, "a"),
            added(1, "b"),
            done(1, "b", "{\"b\":2}"),
            done(0, "a", "{\"a\":1}"),
            done(0, "a", "{\"a\":1}"),
        ],
        vec![
            ("call_a", "tool_a", json!({"a":1})),
            ("call_b", "tool_b", json!({"b":2})),
        ],
    );
}

#[test]
fn unknown_identified_event_does_not_pollute_current_call() {
    check(
        vec![
            added(0, "a"),
            delta(9, "unknown", "garbage"),
            delta(0, "a", "{\"a\":1}"),
        ],
        vec![("call_a", "tool_a", json!({"a":1}))],
    );
}

#[test]
fn legacy_unidentified_sequential_calls_still_parse() {
    let mut chunks = vec![
        added(0, "a"),
        delta(0, "a", "{\"a\":1}"),
        done(0, "a", "{\"a\":1}"),
        added(1, "b"),
        delta(1, "b", "{\"b\":2}"),
    ];
    for chunk in &mut chunks {
        chunk.as_object_mut().unwrap().remove("output_index");
        chunk.as_object_mut().unwrap().remove("item_id");
        if let Some(item) = chunk.get_mut("item") {
            item.as_object_mut().unwrap().remove("id");
        }
    }
    check(
        chunks,
        vec![
            ("call_a", "tool_a", json!({"a":1})),
            ("call_b", "tool_b", json!({"b":2})),
        ],
    );
}

#[test]
fn sparse_index_and_item_id_only_events_keep_identity() {
    let mut first = delta(7, "a", "{\"a\":1}");
    first.as_object_mut().unwrap().remove("output_index");
    check(
        vec![
            added(7, "a"),
            added(11, "b"),
            first,
            delta(11, "b", "{\"b\":2}"),
        ],
        vec![
            ("call_a", "tool_a", json!({"a":1})),
            ("call_b", "tool_b", json!({"b":2})),
        ],
    );
}

#[test]
fn conflicting_item_identity_is_ignored() {
    check(
        vec![
            added(0, "a"),
            added(1, "b"),
            delta(0, "b", "garbage"),
            delta(0, "a", "{\"a\":1}"),
            delta(1, "b", "{\"b\":2}"),
        ],
        vec![
            ("call_a", "tool_a", json!({"a":1})),
            ("call_b", "tool_b", json!({"b":2})),
        ],
    );
}

#[test]
fn large_wire_index_does_not_allocate_intermediate_slots() {
    check(
        vec![added(u64::MAX, "a"), delta(u64::MAX, "a", "{\"a\":1}")],
        vec![("call_a", "tool_a", json!({"a":1}))],
    );
}

#[test]
fn captured_http_stream_keeps_both_tool_calls_in_semantic_output() {
    use agentsight::aggregator::Aggregator;
    use agentsight::analyzer::Analyzer;
    use agentsight::event::Event;
    use agentsight::genai::semantic::GenAISemanticEvent;
    use agentsight::parser::Parser;
    use agentsight::response_map::ResponseSessionMapper;
    use std::collections::HashMap;

    let request_body = json!({"model":"fixture-model", "input":"hello", "stream":true}).to_string();
    let request = format!(
        "POST /v1/responses HTTP/1.1\r\nHost: api.openai.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        request_body.len(),
        request_body
    );
    let mut aggregator = Aggregator::new();
    let parser = Parser::new();
    assert!(
        aggregator
            .process_result(parser.parse_event(Event::Ssl(common::make_ssl_event(
                123,
                0x123,
                1,
                request.into_bytes(),
                "fixture"
            ))))
            .is_empty()
    );
    let chunks = vec![
        json!({"type":"response.created", "response":{"id":"resp_fixture", "model":"fixture-model"}}),
        added(0, "a"),
        added(1, "b"),
        delta(0, "a", "{\"a\":1}"),
        delta(1, "b", "{\"b\":2}"),
        done(1, "b", "{\"b\":2}"),
        done(0, "a", "{\"a\":1}"),
        json!({"type":"response.completed", "response":{"id":"resp_fixture", "model":"fixture-model", "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}),
    ];
    // Keep HTTP headers and SSE events in separate capture records, as the
    // production parser dispatches each record according to its leading bytes.
    let mut records = vec![common::make_openai_sse_response_headers()];
    records.extend(
        chunks
            .into_iter()
            .map(|chunk| format!("data: {chunk}\n\n").into_bytes()),
    );
    records.push(common::make_sse_done());
    let mut completed = Vec::new();
    for bytes in records {
        completed.extend(aggregator.process_result(parser.parse_event(Event::Ssl(
            common::make_ssl_event(123, 0x123, 0, bytes, "fixture"),
        ))));
    }
    assert!(!completed.is_empty());
    let mut observed = Vec::new();
    for result in completed {
        let analyzed = Analyzer::new().analyze_aggregated(&result);
        let (output, _) = GenAIBuilder::new().build_with_pending(
            &analyzed,
            &ResponseSessionMapper::new(),
            &HashMap::new(),
        );
        for event in output.events {
            if let GenAISemanticEvent::LLMCall(call) = event {
                for message in call.response.messages {
                    for part in message.parts {
                        if let MessagePart::ToolCall {
                            id,
                            name,
                            arguments,
                        } = part
                        {
                            observed.push((id.unwrap(), name, arguments.unwrap_or(Value::Null)));
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        observed,
        vec![
            ("call_a".into(), "tool_a".into(), json!({"a":1})),
            ("call_b".into(), "tool_b".into(), json!({"b":2}))
        ]
    );
}

#[test]
fn drain_independently_preserves_interleaved_arguments() {
    let chunks = vec![
        added(0, "a"),
        added(1, "b"),
        delta(0, "a", "{\"a\":1}"),
        delta(1, "b", "{\"b\":2}"),
    ];
    assert_eq!(
        drained_calls(&chunks),
        vec![
            ("call_a".into(), "tool_a".into(), json!({"a":1})),
            ("call_b".into(), "tool_b".into(), json!({"b":2}))
        ]
    );
}

#[test]
fn too_many_calls_are_bounded_without_mutating_an_earlier_call() {
    let chunks: Vec<_> = (0..257)
        .flat_map(|i| {
            let suffix = i.to_string();
            [added(i, &suffix), delta(i, &suffix, "{}")]
        })
        .collect();
    let calls = parsed_calls(&chunks);
    assert_eq!(calls.len(), 256);
    assert!(calls.iter().all(|(_, _, args)| args == &json!({})));
    assert_eq!(drained_calls(&chunks), calls);
}

#[test]
fn item_done_recovers_a_call_from_a_mid_stream_capture() {
    // The capture started after output_item.added and before the arguments
    // events; response.output_item.done carries the complete call. The token
    // extractor already recovers text items from this event, so the message
    // parsers must recover the tool call too.
    check(
        vec![
            item_done(0, "a", "{\"a\":1}"),
            item_done(1, "b", "{\"b\":2}"),
            json!({"type":"response.completed", "response":{
                "id":"resp_fixture", "model":"fixture-model",
                "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}),
        ],
        vec![
            ("call_a", "tool_a", json!({"a":1})),
            ("call_b", "tool_b", json!({"b":2})),
        ],
    );
}

#[test]
fn item_done_supersedes_partial_deltas_with_authoritative_arguments() {
    // added carried empty arguments, the deltas were cut mid-JSON, and the
    // stream ended without function_call_arguments.done: the done item is the
    // only complete arguments record.
    check(
        vec![
            added(0, "a"),
            delta(0, "a", "{\"a\":"),
            item_done(0, "a", "{\"a\":1}"),
        ],
        vec![("call_a", "tool_a", json!({"a":1}))],
    );
}

#[test]
fn item_done_for_non_call_items_and_contradicted_identity_is_ignored() {
    // A message item's done event must not register a call, and a done item
    // whose identity contradicts a known call (reusing output_index 0 under
    // a different item id) must not be recorded against it.
    check(
        vec![
            added(0, "a"),
            delta(0, "a", "{\"a\":1}"),
            message_item_done(3),
            json!({"type":"response.output_item.done", "output_index":0,
                "item":{"type":"function_call", "id":"fc_b",
                    "call_id":"call_b", "name":"tool_b", "arguments":"garbage"}}),
        ],
        vec![("call_a", "tool_a", json!({"a":1}))],
    );
}
