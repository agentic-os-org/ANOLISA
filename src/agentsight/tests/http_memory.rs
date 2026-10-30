//! Memory-policy regressions across capture, aggregation, analysis and semantic output.

mod common;

use agentsight::aggregator::{AggregatedResult, Aggregator, RemoveReason};
use agentsight::analyzer::Analyzer;
use agentsight::config::RuntimeLimits;
use agentsight::event::Event;
use agentsight::genai::GenAIBuilder;
use agentsight::genai::semantic::{GenAISemanticEvent, LLMCall};
use agentsight::parser::Parser;
use agentsight::response_map::ResponseSessionMapper;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;

fn feed(aggregator: &mut Aggregator, id: u64, rw: i32, bytes: &[u8]) -> Vec<AggregatedResult> {
    let event = common::make_ssl_event(std::process::id(), id, rw, bytes.to_vec(), "fixture");
    aggregator.process_result(Parser::new().parse_event(Event::Ssl(event)))
}

fn calls(results: &[AggregatedResult]) -> Vec<LLMCall> {
    let builder = GenAIBuilder::new();
    let analyzer = Analyzer::new();
    results
        .iter()
        .flat_map(|result| {
            builder
                .build_with_pending(
                    &analyzer.analyze_aggregated(result),
                    &ResponseSessionMapper::new(),
                    &HashMap::new(),
                )
                .0
                .events
        })
        .filter_map(|event| match event {
            GenAISemanticEvent::LLMCall(call) => Some(call),
            _ => None,
        })
        .collect()
}

fn request(path: &str, body: Value) -> Vec<u8> {
    let body = body.to_string();
    format!("POST {path} HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

fn record(value: Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

fn run_sse(raw: bool, path: &str, body: Value, events: &[Vec<u8>]) -> (LLMCall, usize) {
    let mut aggregator = Aggregator::with_limits(
        64,
        &RuntimeLimits {
            retain_raw_sse: raw,
            ..Default::default()
        },
    );
    assert!(feed(&mut aggregator, 1, 1, &request(path, body)).is_empty());
    assert!(
        feed(
            &mut aggregator,
            1,
            0,
            &common::make_openai_sse_response_headers()
        )
        .is_empty()
    );
    let mut results = Vec::new();
    for event in events {
        results.extend(feed(&mut aggregator, 1, 0, event));
    }
    assert!(!aggregator.has_pending());
    let retained_events = match &results[0] {
        AggregatedResult::SseComplete(pair) => pair.response.sse_events.len(),
        other => panic!("unexpected result: {other:?}"),
    };
    let mut calls = calls(&results);
    assert_eq!(calls.len(), 1);
    (calls.pop().unwrap(), retained_events)
}

fn semantic(call: &LLMCall) -> Value {
    let mut value = serde_json::to_value(call).unwrap();
    value["request"].as_object_mut().unwrap().remove("raw_body");
    value["response"]
        .as_object_mut()
        .unwrap()
        .remove("raw_body");
    value
}

#[test]
fn rejected_calls_cannot_displace_active_sse_and_their_tails_are_ignored() {
    for raw in [false, true] {
        let mut aggregator = Aggregator::with_limits(
            2,
            &RuntimeLimits {
                retain_raw_sse: raw,
                ..Default::default()
            },
        );
        for id in 1..=2 {
            feed(
                &mut aggregator,
                id,
                1,
                &request(
                    "/v1/chat/completions",
                    json!({"model":"fixture", "stream":true, "messages":[{"role":"user","content":"hello"}]}),
                ),
            );
            feed(
                &mut aggregator,
                id,
                0,
                &common::make_openai_sse_response_headers(),
            );
            feed(
                &mut aggregator,
                id,
                0,
                &record(
                    json!({"id":format!("call-{id}"),"choices":[{"index":0,"delta":{"content":"partial"}}]}),
                ),
            );
        }
        let abandoned = feed(
            &mut aggregator,
            3,
            1,
            &request("/v1/chat/completions", json!({"messages":[]})),
        );
        assert!(matches!(
            &abandoned[..],
            [AggregatedResult::Abandoned {
                reason: RemoveReason::CapacityRejected,
                ..
            }]
        ));
        let partial = calls(&abandoned);
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].metadata["metadata_only"], "true");
        assert_eq!(partial[0].metadata["abandon_reason"], "capacity_rejected");
        assert!(partial[0].request.messages.is_empty());
        assert!(partial[0].response.messages[0].parts.is_empty());
        let tail = record(json!({"choices":[{"index":0,"delta":{"content":"tail"}}]}));
        for _ in 0..8 {
            assert!(feed(&mut aggregator, 3, 0, &tail).is_empty());
        }
        assert!(feed(&mut aggregator, 3, 0, b"data: [DONE]\n\n").is_empty());
        assert_eq!(aggregator.http().active_connections(), 2);
        let completed = feed(&mut aggregator, 2, 0, b"data: [DONE]\n\n");
        assert!(
            matches!(&completed[..], [AggregatedResult::SseComplete(pair)] if pair.connection_id.ssl_ptr == 2)
        );
        assert_eq!(calls(&completed).len(), 1);
        assert_eq!(
            calls(&feed(&mut aggregator, 1, 0, b"data: [DONE]\n\n")).len(),
            1
        );
        // Completed keep-alive connections still own their slots during the
        // inter-request grace period; reuse one of those admitted connections.
        assert!(
            feed(
                &mut aggregator,
                2,
                1,
                &request("/v1/chat/completions", json!({"messages":[]}))
            )
            .is_empty()
        );
        assert!(
            feed(
                &mut aggregator,
                2,
                0,
                &common::make_openai_sse_response_headers()
            )
            .is_empty()
        );
        assert!(feed(&mut aggregator, 2, 0, &tail).is_empty());
        assert!(matches!(
            &feed(&mut aggregator, 2, 0, b"data: [DONE]\n\n")[..],
            [AggregatedResult::SseComplete(_)]
        ));
    }
}

#[test]
fn keep_alive_requests_remain_complete_when_churn_arrives_between_exchanges() {
    for raw in [false, true] {
        let mut aggregator = Aggregator::with_limits(
            2,
            &RuntimeLimits {
                retain_raw_sse: raw,
                ..Default::default()
            },
        );
        let original = request(
            "/v1/chat/completions",
            json!({"model":"fixture", "stream":true, "messages":[{"role":"user","content":"hello"}]}),
        );
        for id in 1..=2 {
            assert!(feed(&mut aggregator, id, 1, &original).is_empty());
            assert!(
                feed(
                    &mut aggregator,
                    id,
                    0,
                    &common::make_openai_sse_response_headers()
                )
                .is_empty()
            );
        }
        for iteration in 0..8 {
            for id in 1..=2 {
                let bytes = [
                    record(json!({
                        "id":format!("call-{id}-{iteration}"),
                        "choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
                    })),
                    b"data: [DONE]\n\n".to_vec(),
                ]
                .concat();
                let completed = calls(&feed(&mut aggregator, id, 0, &bytes));
                assert_eq!(completed.len(), 1);
                assert_ne!(
                    completed[0].metadata.get("incomplete").map(String::as_str),
                    Some("true")
                );
                assert_eq!(completed[0].token_usage.as_ref().unwrap().total_tokens, 2);

                let rejected = feed(&mut aggregator, 100 + iteration * 2 + id, 1, &original);
                assert!(matches!(
                    &rejected[..],
                    [AggregatedResult::Abandoned {
                        reason: RemoveReason::CapacityRejected,
                        ..
                    }]
                ));
                let diagnostics = calls(&rejected);
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0].metadata["metadata_only"], "true");
                assert!(diagnostics[0].request.messages.is_empty());
                assert!(diagnostics[0].response.messages[0].parts.is_empty());
                assert!(diagnostics[0].token_usage.is_none());

                // Start the next request only after the competing connection arrived.
                assert!(feed(&mut aggregator, id, 1, &original).is_empty());
                assert!(
                    feed(
                        &mut aggregator,
                        id,
                        0,
                        &common::make_openai_sse_response_headers()
                    )
                    .is_empty()
                );
                assert_eq!(aggregator.http().active_connections(), 2);
            }
        }
        for id in 1..=2 {
            assert_eq!(
                calls(&feed(&mut aggregator, id, 0, b"data: [DONE]\n\n")).len(),
                1
            );
        }
        assert!(!aggregator.has_pending());
    }
}

#[test]
fn compact_sse_framing_preserves_every_split_and_line_ending() {
    for ending in ["\n", "\r", "\r\n"] {
        let bytes = format!("\u{feff}: comment{ending}event: message{ending}data: {{\"id\":\"split-fixture\",\"choices\":[{ending}data: {{\"index\":0,\"delta\":{{\"content\":\"汉字🙂\"}}}}]}}{ending}{ending}data: [DONE]{ending}{ending}").into_bytes();
        for split in 0..=bytes.len() {
            let mut aggregator = Aggregator::new();
            feed(
                &mut aggregator,
                1,
                1,
                &common::make_openai_request_bytes("fixture", "hello", true),
            );
            feed(
                &mut aggregator,
                1,
                0,
                &common::make_openai_sse_response_headers(),
            );
            let mut results = Vec::new();
            for fragment in [&bytes[..split], &bytes[split..]] {
                results.extend(feed(&mut aggregator, 1, 0, fragment));
            }
            // A final CR can be the first half of CRLF; transport completion resolves it.
            if aggregator.has_pending() {
                results.extend(feed(&mut aggregator, 1, 0, b"\n"));
            }
            let calls = calls(&results);
            assert_eq!(calls.len(), 1, "ending={ending:?}, split={split}");
            assert!(
                serde_json::to_string(&calls[0]).unwrap().contains("汉字🙂"),
                "ending={ending:?}, split={split}, call={:?}",
                calls[0]
            );
            assert!(!aggregator.has_pending());
        }
    }
}

#[test]
fn native_request_mode_controls_snapshot_or_delta_reduction() {
    for (incremental, expected) in [(false, "aa"), (true, "aaa")] {
        let (call, _) = run_sse(
            false,
            "/api/v1/services/aigc/text-generation/generation",
            json!({"model":"fixture", "input":{"messages":[{"role":"user","content":"hello"}]}, "parameters":{"incremental_output":incremental}}),
            &[
                record(json!({"id":"native-mode", "output":{"text":"a","finish_reason":"null"}})),
                record(json!({"id":"native-mode", "output":{"text":"aa","finish_reason":"stop"}})),
            ],
        );
        assert!(
            matches!(call.response.messages[0].parts.as_slice(), [agentsight::genai::MessagePart::Text { content }] if content == expected)
        );
    }
}

#[test]
fn escaped_delta_keys_and_multiline_payloads_keep_raw_and_compact_semantics() {
    let events = vec![
        br#"event: update
data: {"id":"escaped-fixture","choices":[{"index":0,"d\u0065lta":{"content":"first"}}]}

"#.to_vec(),
        br#"id: second
retry: 123
data: {"choices":[
: ignored comment
data: {"index":0,"delta":{"content":" second"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}

"#.to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let body =
        json!({"model":"fixture", "messages":[{"role":"user","content":"hello"}],"stream":true});
    let (compact, _) = run_sse(false, "/v1/chat/completions", body.clone(), &events);
    let (raw, _) = run_sse(true, "/v1/chat/completions", body, &events);
    assert_eq!(semantic(&compact), semantic(&raw));
    assert!(matches!(compact.response.messages[0].parts.as_slice(),
        [agentsight::genai::MessagePart::Text {content}] if content == "first second"));
    assert_eq!(compact.token_usage.unwrap().total_tokens, 5);
}

#[test]
fn gemini_passthrough_preserves_payload_order_usage_and_escaped_termination() {
    for raw in [false, true] {
        let mut aggregator = Aggregator::with_limits(
            4,
            &RuntimeLimits {
                retain_raw_sse: raw,
                ..Default::default()
            },
        );
        feed(
            &mut aggregator,
            1,
            1,
            &request(
                "/v1beta/models/gemini-fixture:streamGenerateContent",
                json!({"contents":[{"role":"user","parts":[{"text":"hello"}]}]}),
            ),
        );
        feed(
            &mut aggregator,
            1,
            0,
            &common::make_openai_sse_response_headers(),
        );
        let first = json!({"candidates":[{"content":{"parts":[{"text":"first 汉字"}],"role":"model"},"index":0}]});
        assert!(feed(&mut aggregator, 1, 0, &record(first.clone())).is_empty());
        assert!(aggregator.has_pending());
        let results = feed(&mut aggregator, 1, 0, br#"data: {"candidates":[{"content":{"parts":[{"text":" second"}],"role":"model"},"index":0,"finish\u0052eason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}}

"#);
        let [AggregatedResult::SseComplete(pair)] = results.as_slice() else {
            panic!("Gemini terminator was missed: {results:?}")
        };
        assert_eq!(pair.response.sse_events.len(), 2);
        assert_eq!(pair.response.sse_events[0].json_body(), Some(first));
        assert_eq!(
            pair.response.sse_events[1].json_body().unwrap()["candidates"][0]["finishReason"],
            "STOP"
        );
        assert!(!aggregator.has_pending());
        let calls = calls(&results);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].provider, "gemini");
        assert_eq!(calls[0].token_usage.as_ref().unwrap().total_tokens, 5);
    }
}

#[test]
fn unknown_native_mode_remains_exact_when_cumulative_and_incremental_chunks_alternate() {
    let (call, _) = run_sse(
        false,
        "/api/v1/services/aigc/text-generation/generation",
        json!({"model":"fixture","input":{"messages":[{"role":"user","content":"hello"}]}}),
        &[
            record(json!({"output":{"text":"a","finish_reason":"null"}})),
            record(json!({"output":{"text":"ab","finish_reason":"null"}})),
            record(json!({"output":{"text":"X","finish_reason":"null"}})),
            record(json!({"output":{"text":"abXz","finish_reason":"stop"}})),
        ],
    );
    assert!(matches!(call.response.messages[0].parts.as_slice(),
        [agentsight::genai::MessagePart::Text {content}] if content == "abXz"));
}

#[test]
fn over_budget_sse_reports_only_metadata_and_counts_ignored_tail_bytes() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let tail = record(json!({"choices":[{"index":0,"delta":{"content":"tail"}}]}));
    metrics::with_local_recorder(&recorder, || {
        let mut aggregator = Aggregator::with_limits(
            4,
            &RuntimeLimits {
                max_sse_retained_bytes: 4096,
                ..Default::default()
            },
        );
        for id in 1..=2 {
            feed(
                &mut aggregator,
                id,
                1,
                &common::make_openai_request_bytes("fixture", "hello", true),
            );
            feed(
                &mut aggregator,
                id,
                0,
                &common::make_openai_sse_response_headers(),
            );
        }
        assert!(feed(&mut aggregator, 1, 0, &record(json!({"id":"budget-fixture","choices":[{"index":0,"delta":{"content":"partial"}}]}))).is_empty());
        let abandoned = feed(
            &mut aggregator,
            1,
            0,
            &record(json!({"choices":[{"index":0,"delta":{"content":"x".repeat(8192)}}]})),
        );
        assert!(matches!(
            &abandoned[..],
            [AggregatedResult::Abandoned {
                reason: RemoveReason::OverBudget,
                ..
            }]
        ));
        let partial = calls(&abandoned);
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].metadata["truncated"], "true");
        assert_eq!(partial[0].metadata["metadata_only"], "true");
        assert!(partial[0].request.messages.is_empty());
        assert!(partial[0].response.messages[0].parts.is_empty());
        assert!(partial[0].token_usage.is_none());
        assert!(partial[0].response.raw_body.is_none());
        for _ in 0..8 {
            assert!(feed(&mut aggregator, 1, 0, &tail).is_empty());
        }
        assert_eq!(aggregator.http().active_connections(), 1);
        assert!(matches!(
            &feed(&mut aggregator, 2, 0, b"data: [DONE]\n\n")[..],
            [AggregatedResult::SseComplete(_)]
        ));
    });
    assert!(handle.render().contains(&format!(
        "agentsight_http_sse_discarded_bytes_total{{reason=\"over_budget\"}} {}",
        tail.len() * 8
    )));
}

#[test]
fn payload_memory_session_deferral_requires_enabled_mapping_and_an_unresolved_session() {
    for (enabled, session) in [(false, None), (true, Some("session-fixture")), (true, None)] {
        let mut body = json!({"model":"claude-fixture","messages":[{"role":"user","content":"hello"}],"max_tokens":10});
        if let Some(session) = session {
            body["metadata"] = json!({"session_id":session});
        }
        let mut aggregator = Aggregator::with_limits(32, &RuntimeLimits::default());
        assert!(feed(&mut aggregator, 1, 1, &request("/v1/messages", body)).is_empty());
        let response = json!({"id":"msg-fixture","type":"message","role":"assistant","model":"claude-fixture","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}).to_string();
        let bytes = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{response}",
            response.len()
        );
        let results = feed(&mut aggregator, 1, 0, bytes.as_bytes());
        assert_eq!(results.len(), 1);
        let mapper = if enabled {
            ResponseSessionMapper::new()
        } else {
            ResponseSessionMapper::disabled()
        };
        let (output, _) = GenAIBuilder::new().build_with_pending(
            &Analyzer::new().analyze_aggregated(&results[0]),
            &mapper,
            &HashMap::new(),
        );
        assert_eq!(
            output.pending_response_id.is_some(),
            enabled && session.is_none()
        );
        let GenAISemanticEvent::LLMCall(call) = &output.events[0] else {
            panic!("expected LLM call")
        };
        assert_eq!(call.token_usage.as_ref().unwrap().total_tokens, 5);
        if let Some(session) = session {
            assert_eq!(
                call.metadata.get("session_id").map(String::as_str),
                Some(session)
            );
        }
    }
}

#[test]
fn compact_chat_keeps_tool_reasoning_usage_and_timing_fields() {
    let body = json!({"model":"fixture", "stream":true, "messages":[{"role":"user", "content":"hello"}], "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]});
    let mut events = vec![record(
        json!({"id":"chatcmpl-fixture","model":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":""}}],"usage":{"prompt_tokens":12,"completion_tokens":0,"total_tokens":12}}),
    )];
    for _ in 0..100 {
        events.push(record(json!({"id":"chatcmpl-fixture","model":"fixture","choices":[{"index":0,"delta":{"reasoning_content":"想","content":"文"}}],"usage":{"prompt_tokens":0,"completion_tokens":1,"total_tokens":1}})));
    }
    events.extend([
        record(json!({"id":"chatcmpl-fixture","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-fixture","type":"function","function":{"name":"lookup","arguments":"{\"city\":\""}}]}}]})),
        record(json!({"id":"chatcmpl-fixture","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"上海\"}"}}]},"finish_reason":"tool_calls"}]})),
        record(json!({"id":"chatcmpl-fixture","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":100,"total_tokens":112}})),
        common::make_sse_done(),
    ]);
    let (compact, count) = run_sse(false, "/v1/chat/completions", body.clone(), &events);
    let (raw, raw_count) = run_sse(true, "/v1/chat/completions", body, &events);
    assert_eq!(semantic(&compact), semantic(&raw));
    assert!(count < 8);
    assert!(raw_count > 100);
}

#[test]
fn compact_anthropic_keeps_multiple_blocks_and_split_usage() {
    let body = json!({"model":"fixture", "max_tokens":300, "stream":true, "messages":[{"role":"user","content":"hello"}]});
    let mut events = vec![record(
        json!({"type":"message_start","message":{"id":"msg-fixture","type":"message","role":"assistant","model":"fixture","content":[],"usage":{"input_tokens":8,"output_tokens":0,"cache_read_input_tokens":4}}}),
    )];
    events.push(record(
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
    ));
    for _ in 0..100 {
        events.push(record(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"文"}})));
    }
    events.extend([
        record(json!({"type":"content_block_stop","index":0})),
        record(json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call-fixture","name":"lookup","input":{}}})),
        record(json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}})),
        record(json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"上海\"}"}})),
        record(json!({"type":"content_block_stop","index":1})),
        record(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":110}})),
        record(json!({"type":"message_stop"})),
    ]);
    let (compact, count) = run_sse(false, "/v1/messages", body.clone(), &events);
    let (raw, _) = run_sse(true, "/v1/messages", body, &events);
    assert_eq!(semantic(&compact), semantic(&raw));
    assert!(count < 12);
}

#[test]
fn native_rich_content_stays_equivalent_to_the_original_sequence() {
    let body = json!({"model":"fixture","input":{"messages":[{"role":"user","content":"hello"}]},"parameters":{"result_format":"message"}});
    let events = vec![
        record(
            json!({"id":"native-fixture","output":{"choices":[{"message":{"role":"assistant","content":[{"text":"first"}]},"finish_reason":"null"}]},"usage":{"input_tokens":3,"output_tokens":1}}),
        ),
        record(
            json!({"id":"native-fixture","output":{"choices":[{"message":{"role":"assistant","content":[{"text":" last"}]},"finish_reason":"stop"}]},"usage":{"input_tokens":3,"output_tokens":2}}),
        ),
    ];
    let path = "/api/v1/services/aigc/text-generation/generation";
    let (compact, _) = run_sse(false, path, body.clone(), &events);
    let (raw, _) = run_sse(true, path, body, &events);
    assert_eq!(semantic(&compact), semantic(&raw));
}

fn compressed(encoding: &str, bytes: &[u8]) -> Vec<u8> {
    match encoding {
        "zstd" => zstd::encode_all(bytes, 3).unwrap(),
        "gzip" => {
            let mut writer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            writer.write_all(bytes).unwrap();
            writer.finish().unwrap()
        }
        "deflate" => {
            let mut writer =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            writer.write_all(bytes).unwrap();
            writer.finish().unwrap()
        }
        "raw-deflate" => {
            let mut writer =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            writer.write_all(bytes).unwrap();
            writer.finish().unwrap()
        }
        "br" => {
            let mut writer = brotli::CompressorWriter::new(Vec::new(), 4096, 4, 20);
            writer.write_all(bytes).unwrap();
            writer.into_inner()
        }
        _ => unreachable!(),
    }
}

#[test]
fn compressed_chunked_sse_decodes_across_single_byte_tls_boundaries() {
    let plain = [
        common::make_openai_sse_chunk("compressed-fixture", "fixture", "汉字🙂", 9, 3),
        common::make_sse_done(),
    ]
    .concat();
    for encoding in ["gzip", "zstd", "deflate", "raw-deflate", "br"] {
        let mut aggregator = Aggregator::new();
        assert!(
            feed(
                &mut aggregator,
                1,
                1,
                &common::make_openai_request_bytes("fixture", "hello", true)
            )
            .is_empty()
        );
        let header_encoding = if encoding == "raw-deflate" {
            "deflate"
        } else {
            encoding
        };
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Encoding: {header_encoding}\r\nTransfer-Encoding: chunked\r\n\r\n"
        );
        assert!(feed(&mut aggregator, 1, 0, headers.as_bytes()).is_empty());
        let compressed = compressed(encoding, &plain);
        let mut framed = Vec::new();
        for chunk in compressed.chunks(13) {
            framed.extend_from_slice(format!("{:x};fixture=yes\r\n", chunk.len()).as_bytes());
            framed.extend_from_slice(chunk);
            framed.extend_from_slice(b"\r\n");
        }
        framed.extend_from_slice(b"0\r\nx-fixture: trailer\r\n\r\n");
        let mut results = Vec::new();
        for byte in framed.chunks(1) {
            results.extend(feed(&mut aggregator, 1, 0, byte));
        }
        let calls = calls(&results);
        assert_eq!(calls.len(), 1, "encoding={encoding}");
        assert!(
            matches!(calls[0].response.messages[0].parts.as_slice(), [agentsight::genai::MessagePart::Text { content }] if content == "汉字🙂"),
            "encoding={encoding}"
        );
        assert_eq!(calls[0].token_usage.as_ref().unwrap().output_tokens, 3);
        assert!(!aggregator.has_pending());
    }
}

#[test]
fn rejected_and_replaced_calls_are_delivered_as_incomplete() {
    let mut aggregator = Aggregator::with_limits(1, &RuntimeLimits::default());
    let first = common::make_openai_request_bytes("fixture", "first", true);
    assert!(feed(&mut aggregator, 1, 1, &first).is_empty());
    let second = common::make_openai_request_bytes("fixture", "second", true);
    let results = feed(&mut aggregator, 2, 1, &second);
    assert!(matches!(
        results.as_slice(),
        [AggregatedResult::Abandoned {
            reason: RemoveReason::CapacityRejected,
            ..
        }]
    ));
    let calls = calls(&results);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].metadata["abandon_reason"], "capacity_rejected");
    assert_eq!(calls[0].metadata["metadata_only"], "true");
    assert!(calls[0].request.messages.is_empty());
    assert!(calls[0].request.raw_body.is_none());
    assert!(calls[0].token_usage.is_none());
    assert_eq!(
        calls[0].response.messages[0].finish_reason.as_deref(),
        Some("incomplete")
    );
    assert!(
        feed(
            &mut aggregator,
            1,
            0,
            &common::make_openai_sse_response_headers()
        )
        .is_empty()
    );
    assert!(
        feed(
            &mut aggregator,
            1,
            0,
            &common::make_openai_sse_chunk("partial-fixture", "fixture", "partial", 2, 3)
        )
        .is_empty()
    );
    let results = feed(&mut aggregator, 1, 1, &second);
    let calls = self::calls(&results);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].metadata["abandon_reason"], "replaced");
    assert_eq!(
        calls[0].response.messages[0].finish_reason.as_deref(),
        Some("incomplete")
    );
    assert!(
        matches!(calls[0].response.messages[0].parts.as_slice(), [agentsight::genai::MessagePart::Text { content }] if content == "partial")
    );
}

#[test]
fn fragmented_request_and_response_waiting_survive_interleaved_churn() {
    for raw in [false, true] {
        let mut aggregator = Aggregator::with_limits(
            2,
            &RuntimeLimits {
                retain_raw_sse: raw,
                ..Default::default()
            },
        );
        let original = common::make_openai_request_bytes("fixture", &"x".repeat(8192), true);
        let boundary = original
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 5;
        assert!(feed(&mut aggregator, 1, 1, &original[..boundary]).is_empty());
        assert!(
            feed(
                &mut aggregator,
                2,
                1,
                &common::make_openai_request_bytes("fixture", "sibling", true)
            )
            .is_empty()
        );
        assert!(
            feed(
                &mut aggregator,
                2,
                0,
                &common::make_openai_sse_response_headers()
            )
            .is_empty()
        );
        for (id, finish_body) in [(3, true), (4, false)] {
            let rejected = feed(
                &mut aggregator,
                id,
                1,
                &common::make_openai_request_bytes("fixture", "churn", false),
            );
            assert!(matches!(
                &rejected[..],
                [AggregatedResult::Abandoned {
                    reason: RemoveReason::CapacityRejected,
                    ..
                }]
            ));
            let diagnostics = calls(&rejected);
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].metadata["metadata_only"], "true");
            assert!(
                feed(
                    &mut aggregator,
                    id,
                    0,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"
                )
                .is_empty()
            );
            if finish_body {
                assert!(feed(&mut aggregator, 1, 1, &original[boundary..]).is_empty());
            }
        }
        assert!(
            feed(
                &mut aggregator,
                1,
                0,
                &common::make_openai_sse_response_headers()
            )
            .is_empty()
        );
        let mut completed = Vec::new();
        for id in [1, 2] {
            completed.extend(feed(
                &mut aggregator,
                id,
                0,
                &common::make_openai_sse_chunk(&format!("main-{id}"), "fixture", "complete", 2, 3),
            ));
            completed.extend(feed(&mut aggregator, id, 0, b"data: [DONE]\n\n"));
        }
        assert_eq!(completed.len(), 2);
        assert!(
            completed
                .iter()
                .all(|result| matches!(result, AggregatedResult::SseComplete(_)))
        );
        let calls = calls(&completed);
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| !call.metadata.contains_key("incomplete"))
        );
        assert_eq!(calls[0].token_usage.as_ref().unwrap().output_tokens, 3);
        assert!(!aggregator.has_pending());
    }
}

fn h2_frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(9 + payload.len());
    let length = payload.len() as u32;
    frame.extend_from_slice(&length.to_be_bytes()[1..]);
    frame.extend_from_slice(&[kind, flags]);
    frame.extend_from_slice(&stream.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[test]
fn http2_uses_streaming_sse_decoding_and_preserves_semantic_fields() {
    let plain = [
        common::make_openai_sse_chunk("h2-fixture", "fixture", "汉字", 4, 2),
        common::make_sse_done(),
    ]
    .concat();
    for encoding in ["identity", "gzip", "zstd", "br"] {
        let mut aggregator = Aggregator::new();
        let mut encoder = hpack::Encoder::new();
        let request_headers = encoder.encode([
            (b":method".as_slice(), b"POST".as_slice()),
            (b":path".as_slice(), b"/v1/chat/completions".as_slice()),
            (b":scheme".as_slice(), b"https".as_slice()),
            (b":authority".as_slice(), b"fixture.example".as_slice()),
            (b"content-type".as_slice(), b"application/json".as_slice()),
        ]);
        assert!(feed(&mut aggregator, 1, 1, &h2_frame(1, 4, 1, &request_headers)).is_empty());
        let body =
            json!({"model":"fixture","messages":[{"role":"user","content":"hello"}],"stream":true})
                .to_string();
        assert!(feed(&mut aggregator, 1, 1, &h2_frame(0, 1, 1, body.as_bytes())).is_empty());
        let mut encoder = hpack::Encoder::new();
        let response_headers = encoder.encode([
            (b":status".as_slice(), b"200".as_slice()),
            (b"content-type".as_slice(), b"text/event-stream".as_slice()),
            (b"content-encoding".as_slice(), encoding.as_bytes()),
        ]);
        assert!(feed(&mut aggregator, 1, 0, &h2_frame(1, 4, 1, &response_headers)).is_empty());
        let bytes = if encoding == "identity" {
            plain.clone()
        } else {
            compressed(encoding, &plain)
        };
        let mut results = Vec::new();
        let fragments: Vec<_> = bytes.chunks(7).collect();
        for (index, fragment) in fragments.iter().enumerate() {
            let end = u8::from(index + 1 == fragments.len());
            results.extend(feed(&mut aggregator, 1, 0, &h2_frame(0, end, 1, fragment)));
        }
        let calls = calls(&results);
        assert_eq!(calls.len(), 1, "encoding={encoding}");
        assert_eq!(calls[0].call_id, "h2-fixture");
        assert_eq!(calls[0].model, "fixture");
        assert!(
            matches!(calls[0].response.messages[0].parts.as_slice(), [agentsight::genai::MessagePart::Text { content }] if content == "汉字")
        );
        assert_eq!(calls[0].token_usage.as_ref().unwrap().input_tokens, 4);
        assert_eq!(calls[0].metadata["server.address"], "fixture.example");
        assert!(!aggregator.has_pending());
    }
}
