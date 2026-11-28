//! Initial request headers must survive TLS capture boundaries before correlation.
#![cfg(target_os = "linux")]

mod common;

use agentsight::aggregator::{AggregatedResult, Aggregator};
use agentsight::analyzer::Analyzer;
use agentsight::event::Event;
use agentsight::genai::{GenAIBuilder, GenAISemanticEvent};
use agentsight::parser::Parser;
use agentsight::response_map::ResponseSessionMapper;
use std::collections::HashMap;

fn request() -> Vec<u8> {
    common::make_openai_request_bytes("fixture-model", "hello", false)
}

fn feed(aggregator: &mut Aggregator, rw: i32, bytes: &[u8]) -> Vec<AggregatedResult> {
    aggregator.process_result(Parser::new().parse_event(Event::Ssl(common::make_ssl_event(
        123,
        0x123,
        rw,
        bytes.to_vec(),
        "fixture",
    ))))
}

fn check(chunks: &[&[u8]]) {
    let mut aggregator = Aggregator::new();
    for chunk in chunks {
        assert!(feed(&mut aggregator, 1, chunk).is_empty());
    }
    let response =
        common::make_openai_json_response_bytes("fixture-call", "fixture-model", "reply", 5, 3);
    let completed = feed(&mut aggregator, 0, &response);
    let [AggregatedResult::HttpComplete(pair)] = completed.as_slice() else {
        panic!("request was lost: {completed:?}");
    };
    assert_eq!(pair.request.path, "/v1/chat/completions");
    assert_eq!(
        pair.request.json_body().unwrap()["messages"][0]["content"],
        "hello"
    );
    let analysis = Analyzer::new().analyze_aggregated(&completed[0]);
    let (output, _) = GenAIBuilder::new().build_with_pending(
        &analysis,
        &ResponseSessionMapper::new(),
        &HashMap::new(),
    );
    let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
        panic!("expected one semantic call");
    };
    assert!(!call.request.messages.is_empty());
    let usage = call.token_usage.as_ref().unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (5, 3));
    assert!(!aggregator.has_pending());
}

#[test]
fn complete_request_control() {
    check(&[&request()]);
}

#[test]
fn every_initial_request_header_split_preserves_the_prompt() {
    let bytes = request();
    let end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    for split in 1..end {
        check(&[&bytes[..split], &bytes[split..]]);
    }
}

#[test]
fn one_byte_request_captures_preserve_the_prompt() {
    let bytes = request();
    check(&bytes.chunks(1).collect::<Vec<_>>());
}

#[test]
fn opposite_direction_bytes_do_not_extend_request_headers() {
    let bytes = request();
    let mut aggregator = Aggregator::new();
    assert!(feed(&mut aggregator, 1, &bytes[..2]).is_empty());
    assert!(feed(&mut aggregator, 0, b"junk").is_empty());
    assert!(feed(&mut aggregator, 1, &bytes[2..]).is_empty());
    let results = feed(
        &mut aggregator,
        0,
        &common::make_openai_json_response_bytes("fixture-call", "fixture-model", "reply", 5, 3),
    );
    assert!(matches!(
        results.as_slice(),
        [AggregatedResult::HttpComplete(_)]
    ));
}

#[test]
fn header_line_resembling_sse_is_still_request_data() {
    let bytes = request();
    let end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 2;
    let mut aggregator = Aggregator::new();
    assert!(feed(&mut aggregator, 1, &bytes[..end]).is_empty());
    assert!(feed(&mut aggregator, 1, b"data: fixture\r\n\r\n").is_empty());
    assert!(feed(&mut aggregator, 1, &bytes[end + 2..]).is_empty());
    let results = feed(
        &mut aggregator,
        0,
        &common::make_openai_json_response_bytes("fixture-call", "fixture-model", "reply", 5, 3),
    );
    let [AggregatedResult::HttpComplete(pair)] = results.as_slice() else {
        panic!("request header was discarded: {results:?}");
    };
    assert_eq!(pair.request.headers["data"], "fixture");
    assert_eq!(
        pair.request.json_body().unwrap()["messages"][0]["content"],
        "hello"
    );
}

#[test]
fn read_direction_request_headers_use_their_own_direction() {
    let bytes = request();
    let mut aggregator = Aggregator::new();
    for chunk in bytes.chunks(17) {
        assert!(feed(&mut aggregator, 0, chunk).is_empty());
    }
    let results = feed(
        &mut aggregator,
        1,
        &common::make_openai_json_response_bytes("fixture-call", "fixture-model", "reply", 5, 3),
    );
    assert!(matches!(
        results.as_slice(),
        [AggregatedResult::HttpComplete(_)]
    ));
}

#[test]
fn early_response_preserves_the_response_without_inventing_a_request() {
    let mut aggregator = Aggregator::new();
    assert!(feed(&mut aggregator, 1, b"PO").is_empty());
    let response = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 2\r\n\r\n{}";
    assert!(feed(&mut aggregator, 0, &response[..3]).is_empty());
    let results = feed(&mut aggregator, 0, &response[3..]);
    assert!(matches!(
        results.as_slice(),
        [AggregatedResult::ResponseOnly { .. }]
    ));
    assert!(!aggregator.has_pending());
}
