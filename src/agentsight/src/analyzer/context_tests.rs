//! Compare semantic-only delivery with the original raw-record pipeline.

use super::*;
use crate::aggregator::{AggregatedResult, Aggregator};
use crate::config::RuntimeLimits;
use crate::event::Event;
use crate::genai::{GenAIBuilder, GenAISemanticEvent};
use crate::parser::Parser;
use crate::probes::sslsniff::SslEvent;
use crate::response_map::ResponseSessionMapper;
use serde_json::{Value, json};
use std::collections::HashMap;

fn event(rw: i32, bytes: &[u8]) -> Event {
    Event::Ssl(SslEvent {
        source: 0,
        timestamp_ns: 1_000,
        delta_ns: 0,
        pid: std::process::id(),
        tid: 1,
        uid: 0,
        len: bytes.len() as u32,
        rw,
        comm: "fixture".into(),
        buf: bytes.to_vec(),
        is_handshake: false,
        ssl_ptr: 1,
    })
}

fn captured(path: &str, request: Value, response: Value, sse: bool) -> AggregatedResult {
    captured_with_retention(path, request, response, sse, true)
}

fn captured_with_retention(
    path: &str,
    request: Value,
    response: Value,
    sse: bool,
    raw: bool,
) -> AggregatedResult {
    let request = request.to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{request}",
        request.len()
    );
    let mut aggregator = Aggregator::with_limits(
        4,
        &RuntimeLimits {
            retain_raw_sse: raw,
            ..Default::default()
        },
    );
    let parser = Parser::new();
    assert!(
        aggregator
            .process_result(parser.parse_event(event(1, request.as_bytes())))
            .is_empty()
    );
    let mut results = Vec::new();
    if sse {
        let headers = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
        assert!(
            aggregator
                .process_result(parser.parse_event(event(0, headers)))
                .is_empty()
        );
        for chunk in response.as_array().unwrap() {
            let bytes = format!("data: {chunk}\n\n");
            results
                .extend(aggregator.process_result(parser.parse_event(event(0, bytes.as_bytes()))));
        }
        if aggregator.has_pending() {
            results.extend(
                aggregator.process_result(parser.parse_event(event(0, b"data: [DONE]\n\n"))),
            );
        }
    } else {
        let body = response.to_string();
        let bytes = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        results.extend(aggregator.process_result(parser.parse_event(event(0, bytes.as_bytes()))));
    }
    assert_eq!(results.len(), 1);
    results.pop().unwrap()
}

fn build_full(results: &[AnalysisResult]) -> Value {
    let (output, pending) = GenAIBuilder::new().build_for_delivery(
        results,
        &ResponseSessionMapper::new(),
        &HashMap::new(),
        false,
    );
    assert!(pending.is_none());
    let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
        panic!("expected a semantic call")
    };
    call_value(call)
}

fn call_value(call: &crate::genai::LLMCall) -> Value {
    let mut value = serde_json::to_value(call).unwrap();
    // Native envelopes without an API id legitimately use a fresh generated id.
    if call.call_id.ends_with("_0") {
        value.as_object_mut().unwrap().remove("call_id");
        for field in ["response_id", "session_id", "conversation_id"] {
            value["metadata"].as_object_mut().unwrap().remove(field);
        }
    }
    value
}

fn build(results: &[AnalysisResult]) -> Value {
    let mut value = build_full(results);
    value["request"].as_object_mut().unwrap().remove("raw_body");
    value["response"]
        .as_object_mut()
        .unwrap()
        .remove("raw_body");
    value
}

#[test]
fn resource_abandonments_skip_body_analysis_and_preserve_metadata_for_every_delivery_path() {
    use crate::aggregator::RemoveReason;
    for reason in [
        RemoveReason::LruEvicted,
        RemoveReason::CapacityRejected,
        RemoveReason::OverBudget,
    ] {
        for raw in [false, true] {
            let mut partial = captured(
                "/v1/chat/completions",
                json!({"model":"fixture", "messages":[{"role":"user", "content":"x".repeat(65536)}]}),
                json!({"id":"fixture", "choices":[{"message":{"role":"assistant", "content":"response"}}],
                    "usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}),
                false,
            );
            let AggregatedResult::HttpComplete(pair) = &mut partial else {
                panic!("expected pair")
            };
            pair.request
                .headers
                .insert("x-request-id".into(), "request-fixture".into());
            pair.request
                .headers
                .insert("host".into(), "fixture.test".into());
            let result = AggregatedResult::Abandoned {
                reason,
                partial: Box::new(partial),
            };
            let analyzer = Analyzer::new();
            for analysis in [
                analyzer.analyze_aggregated(&result),
                analyzer.analyze_for_delivery(&result, raw),
                analyzer.analyze_for_owned_delivery(result.clone(), raw),
            ] {
                let [
                    AnalysisResult::Http(http),
                    AnalysisResult::Abandoned(actual),
                ] = &analysis[..]
                else {
                    panic!("only metadata should be analyzed")
                };
                assert_eq!(*actual, reason);
                assert!(http.request_body.is_none());
                assert!(http.response_body.is_none());
                let (output, pending) = GenAIBuilder::new().build_for_delivery(
                    &analysis,
                    &ResponseSessionMapper::new(),
                    &HashMap::from([(std::process::id(), "fixture-agent".to_owned())]),
                    true,
                );
                assert!(output.pending_response_id.is_none());
                let [GenAISemanticEvent::LLMCall(call)] = &output.events[..] else {
                    panic!("one metadata call")
                };
                assert_eq!(call.pid, std::process::id() as i32);
                assert_eq!(call.agent_name.as_deref(), Some("fixture-agent"));
                assert_eq!(call.metadata["metadata_only"], "true");
                assert_eq!(call.metadata["incomplete"], "true");
                assert_eq!(call.metadata["abandon_reason"], reason.as_str());
                assert_eq!(call.metadata["request_id"], "request-fixture");
                assert_eq!(call.metadata["server.address"], "fixture.test");
                assert_eq!(call.metadata["status_code"], "200");
                assert!(!call.metadata.contains_key("pending_match_key"));
                assert!(call.request.messages.is_empty());
                assert!(call.request.raw_body.is_none());
                assert!(call.response.raw_body.is_none());
                assert!(call.token_usage.is_none());
                assert!(call.response.messages[0].parts.is_empty());
                assert_eq!(
                    call.response.messages[0].finish_reason.as_deref(),
                    Some("incomplete")
                );
                let pending = pending.unwrap();
                assert!(pending.input_messages.is_none());
                assert!(pending.pending_match_key.is_none());
            }
        }
    }
}

#[test]
fn semantic_context_preserves_provider_fields_without_raw_body_strings() {
    let chat_request = json!({"model":"fixture","temperature":0.2,"max_tokens":100,"seed":123,"messages":[{"role":"user","content":"你好"}],"tools":[{"type":"function","function":{"name":"lookup"}}]});
    let chat_response = json!({"id":"chatcmpl-fixture","object":"chat.completion","created":0,"model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}});
    let fixtures = [
        (
            "/v1/chat/completions",
            chat_request.clone(),
            chat_response,
            false,
        ),
        (
            "/v1/chat/completions",
            chat_request,
            json!([
                {"id":"chatcmpl-fixture","model":"fixture","choices":[{"index":0,"delta":{"content":"hi","reasoning_content":"think"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}
            ]),
            true,
        ),
        (
            "/v1/messages",
            json!({"model":"fixture","max_tokens":100,"system":"system","messages":[{"role":"user","content":"你好"}]}),
            json!([
                {"type":"message_start","message":{"id":"msg-fixture","model":"fixture","role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":0,"cache_read_input_tokens":4}}},
                {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},
                {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}},
                {"type":"content_block_stop","index":0},
                {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}},
                {"type":"message_stop"}
            ]),
            true,
        ),
        (
            "/v1/responses",
            json!({"model":"fixture","instructions":"system","input":[{"role":"user","content":"你好"}],"max_output_tokens":100,"stream":true}),
            json!([
                {"type":"response.created","response":{"id":"resp-fixture","model":"fixture","output":[]}},
                {"type":"response.output_text.delta","item_id":"item-fixture","output_index":0,"content_index":0,"delta":"hi"},
                {"type":"response.completed","response":{"id":"resp-fixture","model":"fixture","output":[],"status":"completed","usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15}}}
            ]),
            true,
        ),
        (
            "/api/v1/services/aigc/text-generation/generation",
            json!({"model":"fixture","input":{"messages":[{"role":"user","content":"你好"}]},"parameters":{"max_tokens":100,"temperature":0.2}}),
            json!([
                {"id":"native-fixture","output":{"text":"hi","finish_reason":"stop"},"usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15}}
            ]),
            true,
        ),
    ];
    for (path, request, response, sse) in fixtures {
        let reduced = captured_with_retention(path, request.clone(), response.clone(), sse, false);
        let result = captured(path, request, response, sse);
        let analyzer = Analyzer::new();
        let original = analyzer.analyze_aggregated(&result);
        assert_eq!(
            build(&analyzer.analyze_for_owned_delivery(reduced, false)),
            build(&original),
            "owned path={path}"
        );
        let compact = analyzer.analyze_for_delivery(&result, false);
        let context = compact
            .iter()
            .find_map(|result| match result {
                AnalysisResult::HttpContext(context) => Some(context),
                _ => None,
            })
            .expect("shared JSON context");
        assert!(context.http.request_body.is_none());
        assert!(context.http.response_body.is_none());
        assert_eq!(build(&compact), build(&original), "path={path}");

        let raw = analyzer.analyze_for_delivery(&result, true);
        let raw_context = raw
            .iter()
            .find_map(|result| match result {
                AnalysisResult::HttpContext(context) => Some(context),
                _ => None,
            })
            .expect("raw delivery shares the parsed JSON context");
        let original_http = original
            .iter()
            .find_map(|result| match result {
                AnalysisResult::Http(record) => Some(record),
                _ => None,
            })
            .unwrap();
        assert!(raw_context.retain_raw);
        assert_eq!(
            serde_json::to_value(&raw_context.http).unwrap(),
            serde_json::to_value(original_http).unwrap(),
            "raw HTTP path={path}"
        );
        assert_eq!(
            build_full(&raw),
            build_full(&original),
            "raw LLM path={path}"
        );

        let expected = build_full(&raw);
        let mut owned = raw;
        let (mut output, pending) = GenAIBuilder::new().build_for_owned_delivery(
            &owned,
            &ResponseSessionMapper::new(),
            &HashMap::new(),
            false,
        );
        assert!(pending.is_none());
        output.take_raw_bodies(&mut owned);
        let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
            panic!("expected call")
        };
        assert_eq!(call_value(call), expected, "owned path={path}");
        assert!(owned.iter().all(|result| !matches!(result,
            AnalysisResult::HttpContext(context) if context.http.request_body.is_some() || context.http.response_body.is_some()
        )));
    }
}

#[test]
fn completion_memory_stores_http_before_transferring_body_ownership() {
    let directory =
        std::env::temp_dir().join(format!("agentsight-completion-{}", std::process::id()));
    let storage = crate::storage::Storage::with_sqlite_config(&crate::storage::SqliteConfig::new(
        directory.clone(),
    ))
    .unwrap();
    let text = "x".repeat(1024 * 1024);
    let result = captured(
        "/v1/chat/completions",
        json!({"model":"fixture","messages":[{"role":"user","content":text}]}),
        json!({"id":"completion-fixture","choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":{"prompt_tokens":12,"completion_tokens":1,"total_tokens":13}}),
        false,
    );
    let mut analysis = Analyzer::new().analyze_for_delivery(&result, true);
    drop(result);
    let context = analysis
        .iter_mut()
        .find_map(|result| match result {
            AnalysisResult::HttpContext(context) => Some(context),
            _ => None,
        })
        .unwrap();
    // The transfer itself must keep the original allocation when no shrink is needed.
    context.http.request_body.as_mut().unwrap().shrink_to_fit();
    let pointer = context.http.request_body.as_ref().unwrap().as_ptr();
    let expected = context.http.request_body.as_ref().unwrap().clone();
    for result in &analysis {
        storage.store(result).unwrap();
    }
    let (mut output, pending) = GenAIBuilder::new().build_for_owned_delivery(
        &analysis,
        &ResponseSessionMapper::new(),
        &HashMap::new(),
        true,
    );
    output.take_raw_bodies(&mut analysis);
    drop(analysis);
    let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
        panic!("expected call")
    };
    assert_eq!(call.request.raw_body.as_ref().unwrap().as_ptr(), pointer);
    assert_eq!(call.request.raw_body.as_deref(), Some(expected.as_str()));
    assert_eq!(call.metadata.get("user_query"), Some(&text));
    assert_eq!(pending.unwrap().user_query.as_deref(), Some(text.as_str()));
    let stored = storage.http().query_by_pid(std::process::id()).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].request_body.as_deref(), Some(expected.as_str()));
    drop(storage);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "short heap-sampling fixture; run explicitly under heaptrack"]
fn completion_memory_heap_sample() {
    use crate::storage::sqlite::GenAISqliteStore;

    let directory =
        std::env::temp_dir().join(format!("agentsight-heap-sample-{}", std::process::id()));
    let storage = crate::storage::Storage::with_sqlite_config(&crate::storage::SqliteConfig::new(
        directory.clone(),
    ))
    .unwrap();
    let store = GenAISqliteStore::new_with_path(
        &directory.join("genai_events.db"),
        crate::config::PeriodicStoragePolicy::default(),
    )
    .unwrap();
    let mut results = Vec::new();
    for index in 0..32 {
        results.push(captured(
            "/v1/chat/completions",
            json!({"model":"fixture","messages":[{"role":"user","content":"x".repeat(1024 * 1024)}],"stream":true}),
            json!([{"id":format!("heap-fixture-{index}"),"model":"fixture","choices":[{"index":0,"delta":{"content":" x".repeat(1000)},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":1000,"total_tokens":1012}}]), true,
        ));
    }
    let builder = GenAIBuilder::new();
    let analyzer = Analyzer::new();
    let mapper = ResponseSessionMapper::new();
    let cache = HashMap::new();
    for result in results {
        let mut analysis = analyzer.analyze_for_delivery(&result, true);
        drop(result);
        for result in &analysis {
            storage.store(result).unwrap();
        }
        let (mut output, pending) =
            builder.build_for_owned_delivery(&analysis, &mapper, &cache, true);
        output.take_raw_bodies(&mut analysis);
        drop(analysis);
        let pending = pending.unwrap();
        store.insert_pending(&pending).unwrap();
        drop(pending);
        for event in &output.events {
            store.complete_pending(event).unwrap();
        }
    }
    let validation = rusqlite::Connection::open_with_flags(
        directory.join("genai_events.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let saved: i64 = validation
        .query_row(
            "SELECT COUNT(*) FROM genai_events WHERE status = 'complete'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, 32);
    drop(validation);
    drop(store);
    drop(storage);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn raw_export_and_unknown_paths_keep_serialized_http_bodies() {
    for (path, retain_raw) in [("/v1/chat/completions", true), ("/unknown", false)] {
        let result = captured(
            path,
            json!({"model":"fixture","messages":[{"role":"user","content":"hi"}]}),
            json!({"ok":true}),
            false,
        );
        let results = Analyzer::new().analyze_for_delivery(&result, retain_raw);
        if retain_raw {
            assert!(results.iter().any(|result| {
                matches!(result, AnalysisResult::HttpContext(context) if context.retain_raw && context.http.request_body.is_some() && context.http.response_body.is_some())
            }));
        } else {
            assert!(results.iter().any(
                |result| matches!(result, AnalysisResult::Http(record) if record.request_body.is_some())
            ));
            assert!(
                !results
                    .iter()
                    .any(|result| matches!(result, AnalysisResult::HttpContext(_)))
            );
        }
    }
}

#[test]
fn raw_context_persists_the_original_http_record_without_storing_semantic_only_bodies() {
    static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "agentsight-json-context-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let storage = crate::storage::Storage::with_sqlite_config(&crate::storage::SqliteConfig::new(
        directory.clone(),
    ))
    .unwrap();
    let result = captured(
        "/v1/chat/completions",
        json!({"model":"fixture","messages":[{"role":"user","content":"hello"}]}),
        json!({"id":"chatcmpl-persist","choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":1,"total_tokens":5}}),
        false,
    );
    let analyzer = Analyzer::new();
    for analyzed in analyzer.analyze_for_delivery(&result, false) {
        if matches!(analyzed, AnalysisResult::HttpContext(_)) {
            assert_eq!(storage.store(&analyzed).unwrap(), 0);
        }
    }
    assert_eq!(storage.http().count().unwrap(), 0);
    let raw = analyzer.analyze_for_delivery(&result, true);
    let context = raw
        .iter()
        .find(|result| matches!(result, AnalysisResult::HttpContext(_)))
        .unwrap();
    storage.store(context).unwrap();
    let original = analyzer.analyze_aggregated(&result);
    let original_http = original
        .iter()
        .find_map(|result| match result {
            AnalysisResult::Http(record) => Some(record),
            _ => None,
        })
        .unwrap();
    let persisted = storage.http().query_by_pid(std::process::id()).unwrap();
    assert_eq!(persisted.len(), 1);
    assert_eq!(
        serde_json::to_value(&persisted[0]).unwrap(),
        serde_json::to_value(original_http).unwrap()
    );
    drop(storage);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn semantic_context_keeps_multiple_choices_and_content_block_order() {
    let fixtures = [
        (
            "/v1/chat/completions",
            json!({"model":"fixture","messages":[{"role":"user","content":"hello"}],"stream":true,"n":2}),
            json!([
                {"id":"multi-fixture","object":"chat.completion.chunk","created":0,"model":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":"first"}}]},
                {"id":"multi-fixture","object":"chat.completion.chunk","created":0,"model":"fixture","choices":[{"index":1,"delta":{"role":"assistant","content":"second"},"finish_reason":"stop"}]},
                {"id":"multi-fixture","object":"chat.completion.chunk","created":0,"model":"fixture","choices":[{"index":0,"delta":{"content":" choice"},"finish_reason":"stop"}]}
            ]),
        ),
        (
            "/v1/messages",
            json!({"model":"fixture","max_tokens":100,"messages":[{"role":"user","content":"hello"}]}),
            json!([
                {"type":"message_start","message":{"id":"blocks-fixture","type":"message","model":"fixture","role":"assistant","content":[],"usage":{"input_tokens":3,"output_tokens":0}}},
                {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"first"}},
                {"type":"content_block_stop","index":0},
                {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call-fixture","name":"lookup","input":{"city":"上海"}}},
                {"type":"content_block_stop","index":1},
                {"type":"content_block_start","index":2,"content_block":{"type":"text","text":"last"}},
                {"type":"content_block_stop","index":2},
                {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}},
                {"type":"message_stop"}
            ]),
        ),
    ];
    for (path, request, response) in fixtures {
        let result = captured(path, request, response, true);
        let analyzer = Analyzer::new();
        assert_eq!(
            build(&analyzer.analyze_for_delivery(&result, false)),
            build(&analyzer.analyze_aggregated(&result)),
            "path={path}"
        );
    }
}

#[test]
fn thirty_two_interleaved_calls_preserve_large_requests_and_all_output_tokens() {
    let parser = Parser::new();
    let mut aggregator = Aggregator::with_limits(32, &RuntimeLimits::default());
    let mut feed = |id, rw, bytes: &[u8]| {
        let Event::Ssl(mut ssl) = event(rw, bytes) else {
            unreachable!()
        };
        ssl.ssl_ptr = id;
        aggregator.process_result(parser.parse_event(Event::Ssl(ssl)))
    };
    let messages = json!([{"role":"user","content":"keep this request"}]);
    let body = json!({
        "model":"fixture",
        "messages":messages,
        "stream":true,
        "padding":"x".repeat(1024 * 1024)
    })
    .to_string();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    for id in 1..=32 {
        assert!(feed(id, 1, &request.as_bytes()[..8192]).is_empty());
        assert!(feed(id, 1, &request.as_bytes()[8192..]).is_empty());
        assert!(
            feed(
                id,
                0,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n"
            )
            .is_empty()
        );
    }
    let deltas: Vec<_> = (1..=32)
        .map(|id| {
            format!(
                "data: {}\n\n",
                json!({
                    "id":format!("chatcmpl-{id}"),
                    "model":"fixture",
                    "choices":[{"index":0,"delta":{"content":" x"}}]
                })
            )
            .into_bytes()
        })
        .collect();
    for _ in 0..1000 {
        for (index, delta) in deltas.iter().enumerate() {
            assert!(feed(index as u64 + 1, 0, delta).is_empty());
        }
    }
    let live = aggregator.connection_metrics();
    assert_eq!(live.pending_connection_count, 32);
    assert_eq!(live.eviction_count, 0);
    assert!(live.connection_cache_bytes < 36 * 1024 * 1024);

    let analyzer = Analyzer::new();
    let builder = GenAIBuilder::new();
    let mapper = ResponseSessionMapper::new();
    let cache = HashMap::new();
    let expected_text = " x".repeat(1000);
    let mut emitted = 0;
    for id in 1..=32 {
        let final_event = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "id":format!("chatcmpl-{id}"),
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":64,"completion_tokens":1000,"total_tokens":1064}
            })
        );
        let Event::Ssl(mut ssl) = event(0, final_event.as_bytes()) else {
            unreachable!()
        };
        ssl.ssl_ptr = id;
        let results = aggregator.process_result(parser.parse_event(Event::Ssl(ssl)));
        let [AggregatedResult::SseComplete(pair)] = results.as_slice() else {
            panic!("connection {id} must produce one complete call")
        };
        assert_eq!(pair.request.body(), body.as_bytes());
        let analyzed = analyzer.analyze_for_delivery(&results[0], false);
        let (output, pending) = builder.build_for_delivery(&analyzed, &mapper, &cache, false);
        assert!(pending.is_none());
        let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
            panic!("connection {id} must produce one semantic call")
        };
        assert_eq!(call.call_id, format!("chatcmpl-{id}"));
        assert!(call.request.raw_body.is_none());
        assert!(call.response.raw_body.is_none());
        assert_eq!(call.token_usage.as_ref().unwrap().output_tokens, 1000);
        assert_eq!(call.token_usage.as_ref().unwrap().input_tokens, 64);
        assert_eq!(
            call.response.messages[0].finish_reason.as_deref(),
            Some("stop")
        );
        assert!(!call.metadata.contains_key("abandon_reason"));
        assert!(matches!(
            call.response.messages[0].parts.as_slice(),
            [crate::genai::MessagePart::Text { content }] if content == &expected_text
        ));
        assert!(matches!(
            call.request.messages[0].parts.as_slice(),
            [crate::genai::MessagePart::Text { content }] if content == "keep this request"
        ));
        emitted += 1;
    }
    assert_eq!(emitted, 32);
    assert!(!aggregator.has_pending());
    let finished = aggregator.connection_metrics();
    assert_eq!(finished.connection_cache_bytes, 0);
    assert_eq!(finished.eviction_count, 0);
}

#[test]
fn delivery_keeps_malformed_request_and_response_evidence_on_known_llm_paths() {
    let valid_request = br#"{"model":"fixture","messages":[{"role":"user","content":"hello"}]}"#;
    let valid_response =
        br#"{"id":"fixture","choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
    for (request, response) in [
        (valid_request.as_slice(), b"upstream unavailable".as_slice()),
        (valid_request.as_slice(), br#"{"choices":["#.as_slice()),
        (br#"{"messages":["#.as_slice(), valid_response.as_slice()),
        (
            b"unparsed request".as_slice(),
            b"unparsed response".as_slice(),
        ),
    ] {
        let mut aggregator = Aggregator::new();
        let parser = Parser::new();
        let mut bytes = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", request.len(),
        ).into_bytes();
        bytes.extend_from_slice(request);
        assert!(
            aggregator
                .process_result(parser.parse_event(event(1, &bytes)))
                .is_empty()
        );
        let mut bytes = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            response.len(),
        )
        .into_bytes();
        bytes.extend_from_slice(response);
        let results = aggregator.process_result(parser.parse_event(event(0, &bytes)));
        let [result] = results.as_slice() else {
            panic!("expected captured exchange")
        };
        let analyzed = Analyzer::new().analyze_for_delivery(result, true);
        let record = analyzed
            .iter()
            .find_map(|result| match result {
                AnalysisResult::Http(record) => Some(record),
                AnalysisResult::HttpContext(context) => Some(&context.http),
                _ => None,
            })
            .unwrap();
        let expected = |bytes: &[u8]| {
            serde_json::from_slice::<Value>(bytes)
                .map(|value| value.to_string())
                .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned())
        };
        assert_eq!(
            record.request_body.as_deref(),
            Some(expected(request).as_str())
        );
        assert_eq!(
            record.response_body.as_deref(),
            Some(expected(response).as_str())
        );
        let call = build_full(&analyzed);
        assert_eq!(call["request"]["raw_body"], expected(request));
        assert_eq!(call["response"]["raw_body"], expected(response));
    }
}

#[test]
fn delivery_uses_authoritative_anthropic_input_and_cache_corrections() {
    let result = captured(
        "/v1/messages",
        json!({"model":"fixture","max_tokens":100,"messages":[{"role":"user","content":"hello"}]}),
        json!([
            {"type":"message_start","message":{"id":"usage-correction","role":"assistant","model":"fixture","content":[],"usage":{"input_tokens":12,"cache_creation_input_tokens":40,"cache_read_input_tokens":80,"output_tokens":0}}},
            {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"hello"}},
            {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":3,"cache_creation_input_tokens":2,"cache_read_input_tokens":4,"output_tokens":5}},
            {"type":"message_stop"}
        ]),
        true,
    );
    let analyzer = Analyzer::new();
    let expected = build(&analyzer.analyze_aggregated(&result));
    for raw in [false, true] {
        let actual = build(&analyzer.analyze_for_delivery(&result, raw));
        assert_eq!(actual, expected);
        assert_eq!(actual["token_usage"]["input_tokens"], 9);
        assert_eq!(actual["token_usage"]["output_tokens"], 5);
        assert_eq!(actual["token_usage"]["total_tokens"], 14);
    }
}
