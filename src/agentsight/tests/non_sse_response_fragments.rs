//! Response framing regressions through Parser → Aggregator → Analyzer → GenAI → SQLite.
mod common;

use agentsight::aggregator::{AggregatedResult, Aggregator};
use agentsight::analyzer::Analyzer;
use agentsight::config::RuntimeLimits;
use agentsight::event::Event;
use agentsight::genai::semantic::GenAISemanticEvent;
use agentsight::genai::{GenAIBuilder, GenAIExporter};
use agentsight::parser::Parser;
use agentsight::response_map::ResponseSessionMapper;
use agentsight::storage::sqlite::GenAISqliteStore;
use std::collections::HashMap;

fn body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id": "fragment-fixture", "object": "chat.completion", "model": "test-model", "created": 0,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello",
            "tool_calls": [{"id": "call_fragment_fixture", "type": "function",
                "function": {"name": "read_file", "arguments": "{\"path\":\"fixture\"}"}}]},
            "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
    }))
    .unwrap()
}

fn response(body: &[u8], framing: &str) -> Vec<u8> {
    let mut bytes =
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{framing}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn full_response() -> Vec<u8> {
    let body = body();
    response(&body, &format!("Content-Length: {}\r\n", body.len()))
}

fn feed(aggregator: &mut Aggregator, rw: i32, bytes: &[u8], time: u64) -> Vec<AggregatedResult> {
    let mut event = common::make_ssl_event(123, 0x123, rw, bytes.to_vec(), "fixture");
    event.timestamp_ns = time;
    aggregator.process_result(Parser::new().parse_event(Event::Ssl(event)))
}

fn request(aggregator: &mut Aggregator) {
    assert!(
        feed(
            aggregator,
            1,
            &common::make_openai_request_bytes("test-model", "hello", false),
            1
        )
        .is_empty()
    );
}

fn check(chunks: &[&[u8]]) {
    let mut aggregator = Aggregator::new();
    // Reuse the same connection to detect stale framing and duplicated bytes.
    for _ in 0..2 {
        request(&mut aggregator);
        for (index, chunk) in chunks.iter().enumerate() {
            let time = 100 + index as u64;
            let completed = feed(&mut aggregator, 0, chunk, time);
            if index + 1 < chunks.len() {
                assert!(
                    completed.is_empty(),
                    "response completed at fragment {index}"
                );
                continue;
            }
            assert_eq!(completed.len(), 1);
            let AggregatedResult::HttpComplete(pair) = &completed[0] else {
                panic!("expected HTTP pair")
            };
            assert_eq!(
                pair.response.parsed.json_body(),
                Some(serde_json::from_slice(&body()).unwrap())
            );
            assert_eq!(pair.response.start_timestamp_ns(), 100);
            assert_eq!(pair.response.end_timestamp_ns(), time);
            let analyzed = Analyzer::new().analyze_aggregated(&completed[0]);
            let (output, _) = GenAIBuilder::new().build_with_pending(
                &analyzed,
                &ResponseSessionMapper::new(),
                &HashMap::new(),
            );
            let mut events = output.events;
            let call = events
                .iter_mut()
                .find_map(|event| match event {
                    GenAISemanticEvent::LLMCall(call) => Some(call),
                    _ => None,
                })
                .unwrap();
            let usage = call.token_usage.as_ref().unwrap();
            assert_eq!((usage.input_tokens, usage.output_tokens), (5, 3));
            assert_eq!(call.end_timestamp_ns, time);
            assert!(!call.response.messages.is_empty());
            call.metadata
                .insert("session_id".into(), "fragment-session".into());
            let dir = common::temp_dir("response-fragments");
            let store = GenAISqliteStore::new_with_path(
                &dir.join("genai.db"),
                agentsight::config::PeriodicStoragePolicy::default(),
            )
            .unwrap();
            store.export(&events);
            store.flush();
            // This is the output index used by the tokenless savings lookup.
            assert!(
                store
                    .get_tool_call_turn_indices(&["fragment-session"])
                    .unwrap()
                    .contains_key("call_fragment_fixture")
            );
            drop(store);
            std::fs::remove_dir_all(dir).unwrap();
            assert!(!aggregator.has_pending());
        }
    }
}

#[test]
fn complete_response_in_one_read() {
    check(&[&full_response()]);
}

#[test]
fn every_response_split_preserves_usage_and_tool_index() {
    let bytes = full_response();
    for split in 1..bytes.len() {
        check(&[&bytes[..split], &bytes[split..]]);
    }
}

#[test]
fn response_one_byte_per_read() {
    let bytes = full_response();
    check(&bytes.chunks(1).collect::<Vec<_>>());
}

#[test]
fn chunked_response_waits_for_trailers() {
    let body = body();
    let framed = format!(
        "{:x};fixture=yes\r\n{}\r\n0\r\nX-Fixture: ok\r\n\r\n",
        body.len(),
        std::str::from_utf8(&body).unwrap()
    );
    let bytes = response(
        framed.as_bytes(),
        "Transfer-Encoding: Chunked\r\nContent-Length: 1\r\n",
    );
    check(&bytes.chunks(7).collect::<Vec<_>>());
    // The parser synthesizes an SSE DONE event for this last read.
    let bytes = response(
        format!(
            "{:x}\r\n{}\r\n0\r\n\r\n",
            body.len(),
            std::str::from_utf8(&body).unwrap()
        )
        .as_bytes(),
        "Transfer-Encoding: chunked\r\n",
    );
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    check(&[&bytes[..split], &bytes[split..]]);
}

#[test]
fn compressed_response_is_decoded_after_framing() {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&body()).unwrap();
    let compressed = encoder.finish().unwrap();
    let bytes = response(
        &compressed,
        &format!(
            "Content-Encoding: gzip\r\nContent-Length: {}\r\n",
            compressed.len()
        ),
    );
    check(&bytes.chunks(17).collect::<Vec<_>>());
}

#[test]
fn body_bytes_that_look_like_protocol_are_not_reparsed() {
    let mut aggregator = Aggregator::new();
    for body in [
        b"data: first\n\ndata: second\n\n".as_slice(),
        b"HTTP/1.1 200 OK\r\n\r\n",
        b"GET / HTTP/1.1\r\n\r\n",
        b"5\r\nhello\r\n0\r\n\r\n",
    ] {
        request(&mut aggregator);
        assert!(
            feed(
                &mut aggregator,
                0,
                &response(&[], &format!("Content-Length: {}\r\n", body.len())),
                100
            )
            .is_empty()
        );
        let results = feed(&mut aggregator, 0, body, 200);
        let [AggregatedResult::HttpComplete(pair)] = results.as_slice() else {
            panic!("expected one HTTP pair")
        };
        assert_eq!(pair.response.body(), body);
    }
}

#[test]
fn bodyless_responses_ignore_advertised_length() {
    for (method, status) in [("HEAD", 200), ("GET", 204), ("GET", 304), ("CONNECT", 200)] {
        let mut aggregator = Aggregator::new();
        // CONNECT is not detected by the unified parser's method heuristic;
        // exercise its no-body framing with the explicit HTTP parser instead.
        let event = common::make_ssl_event(
            123,
            0x123,
            1,
            format!("{method} / HTTP/1.1\r\n\r\n").into_bytes(),
            "fixture",
        );
        let agentsight::parser::http::ParsedHttpMessage::Request(request) =
            agentsight::parser::http::HttpParser::new()
                .parse(std::rc::Rc::new(event))
                .unwrap()
        else {
            panic!("expected request")
        };
        aggregator.http_mut().process_request(request);
        let results = feed(
            &mut aggregator,
            0,
            format!("HTTP/1.1 {status} OK\r\nContent-Length: 99999\r\n\r\n").as_bytes(),
            2,
        );
        assert_eq!(results.len(), 1, "{method} {status}");
        assert!(!aggregator.has_pending());
    }
}

#[test]
fn informational_response_keeps_request_for_final_response() {
    for coalesced in [false, true] {
        let mut aggregator = Aggregator::new();
        request(&mut aggregator);
        let mut first = b"HTTP/1.1 100 Continue\r\n\r\n".to_vec();
        if coalesced {
            first.extend(full_response());
        }
        let results = feed(&mut aggregator, 0, &first, 100);
        if coalesced {
            assert_eq!(results.len(), 1);
        } else {
            assert!(results.is_empty());
            assert_eq!(feed(&mut aggregator, 0, &full_response(), 200).len(), 1);
        }
    }
}

#[test]
fn incomplete_responses_are_bounded_and_never_complete_on_timeout() {
    let limits = RuntimeLimits {
        max_connection_body_bytes: 1024,
        connection_idle_timeout_secs: 0,
        ..Default::default()
    };
    for (bytes, completes) in [
        (response(b"{}", "Connection: close\r\n"), false),
        (response(b"{", "Content-Length: 2\r\n"), true),
        (b"HTTP/1.1 200".to_vec(), false),
    ] {
        // Use the HTTP API for eviction so zero timeout doesn't evict between reads.
        let mut aggregator = Aggregator::with_limits(4, &limits);
        request(&mut aggregator);
        assert!(feed(&mut aggregator, 0, &bytes, 100).is_empty());
        aggregator.http_mut().evict_idle_and_oversized();
        assert!(
            aggregator.has_pending(),
            "retain request evidence for idle persistence"
        );
        assert_eq!(
            feed(&mut aggregator, 0, b"}", 200).len(),
            usize::from(completes)
        );
        assert_eq!(aggregator.has_pending(), !completes);
    }
    let limits = RuntimeLimits {
        connection_idle_timeout_secs: 60,
        ..limits
    };
    for framing in [
        "Content-Length: 1025\r\n",
        "Transfer-Encoding: chunked\r\n",
        "Connection: close\r\n",
    ] {
        let mut aggregator = Aggregator::with_limits(4, &limits);
        request(&mut aggregator);
        assert!(feed(&mut aggregator, 0, &response(&[], framing), 100).is_empty());
        assert!(feed(&mut aggregator, 0, &[b'x'; 1025], 200).is_empty());
        assert!(!aggregator.has_pending());
        request(&mut aggregator);
        assert_eq!(feed(&mut aggregator, 0, &full_response(), 300).len(), 1);
    }
}

#[test]
fn malformed_framing_does_not_emit_complete() {
    for (framing, body) in [
        ("Content-Length: nope\r\n", "{}"),
        ("Transfer-Encoding: chunked\r\n", "z\r\n{}\r\n0\r\n\r\n"),
        ("Transfer-Encoding: chunked\r\n", "2\r\n{}xx0\r\n\r\n"),
    ] {
        let mut aggregator = Aggregator::new();
        request(&mut aggregator);
        assert!(feed(&mut aggregator, 0, &response(body.as_bytes(), framing), 100).is_empty());
        assert!(!aggregator.has_pending());
    }
}

#[test]
fn opposite_direction_bytes_do_not_extend_response() {
    let mut aggregator = Aggregator::new();
    request(&mut aggregator);
    let bytes = full_response();
    let split = bytes.len() - 20;
    assert!(feed(&mut aggregator, 0, &bytes[..split], 100).is_empty());
    assert!(feed(&mut aggregator, 1, b"unexpected write", 150).is_empty());
    assert_eq!(feed(&mut aggregator, 0, &bytes[split..], 200).len(), 1);
}
