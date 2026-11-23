#![cfg(target_os = "linux")]

//! Native request parameters through the in-memory capture-to-semantic pipeline.

use agentsight::aggregator::{AggregatedResult, Aggregator};
use agentsight::analyzer::Analyzer;
use agentsight::event::Event;
use agentsight::genai::GenAIBuilder;
use agentsight::genai::semantic::{GenAISemanticEvent, LLMCall};
use agentsight::parser::Parser;
use agentsight::probes::sslsniff::SslEvent;
use agentsight::response_map::ResponseSessionMapper;
use serde_json::{Value, json};
use std::collections::HashMap;

const FIXTURE_PID: u32 = 424242;

fn capture(aggregator: &mut Aggregator, rw: i32, bytes: Vec<u8>) -> Vec<AggregatedResult> {
    let event = SslEvent {
        source: 0,
        timestamp_ns: if rw == 1 { 1_000_000 } else { 2_000_000 },
        delta_ns: 0,
        pid: FIXTURE_PID,
        tid: FIXTURE_PID,
        uid: 0,
        len: bytes.len() as u32,
        rw,
        comm: "native-fixture".into(),
        buf: bytes,
        is_handshake: false,
        ssl_ptr: 0x1234,
    };
    aggregator.process_result(Parser::new().parse_event(Event::Ssl(event)))
}

fn native_call(stop: Option<Value>) -> LLMCall {
    // The fixture never initializes a trace sink or a database/exporter.
    assert!(!agentsight::config::chrome_trace());
    // Process-name fallback uses this synthetic, unpopulated fixture root.
    // No probe/configure function is called and no real procfs is accessed.
    static PROC_ROOT: std::sync::Once = std::sync::Once::new();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/native-stop-fixture-proc");
    PROC_ROOT.call_once(|| assert!(agentsight::utils::procfs::set_proc_root(&root)));
    assert_eq!(agentsight::utils::procfs::proc_root(), root);
    let mut request = json!({
        "model": "qwen-fixture",
        "input": {"messages": [{"role": "user", "content": "fixture question"}]},
        "parameters": {"temperature": 0.25, "max_tokens": 9}
    });
    if let Some(stop) = stop {
        request["parameters"]["stop"] = stop;
    }
    let body = serde_json::to_string(&request).unwrap();
    let bytes = format!(
        "POST /api/v1/services/aigc/text-generation/generation HTTP/1.1\r\nHost: native.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(), body
    ).into_bytes();
    let mut aggregator = Aggregator::new();
    assert!(capture(&mut aggregator, 1, bytes).is_empty());
    let body = serde_json::to_string(&json!({
        "request_id": "native-fixture-response",
        "output": {"text": "fixture answer", "finish_reason": "stop"},
        "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}
    }))
    .unwrap();
    let bytes = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes();
    let completed = capture(&mut aggregator, 0, bytes);
    assert_eq!(completed.len(), 1);
    assert!(matches!(completed[0], AggregatedResult::HttpComplete(_)));
    let analysis = Analyzer::new().analyze_aggregated(&completed[0]);
    // A guaranteed cache hit avoids all process-name discovery (/proc reads).
    let agent_cache = HashMap::from([(FIXTURE_PID, "fixture-agent".to_string())]);
    let (delivery, _) = GenAIBuilder::new().build_with_pending(
        &analysis,
        &ResponseSessionMapper::new(),
        &agent_cache,
    );
    let mut calls = delivery.events.into_iter().filter_map(|event| match event {
        GenAISemanticEvent::LLMCall(call) => Some(call),
        _ => None,
    });
    let call = calls.next().expect("one native semantic call");
    assert!(calls.next().is_none());
    assert_eq!(call.agent_name.as_deref(), Some("fixture-agent"));
    assert_eq!(call.process_name, "native-fixture");
    assert_eq!(call.model, "qwen-fixture");
    assert_eq!(call.request.temperature, Some(0.25));
    assert_eq!(call.request.max_tokens, Some(9));
    assert!(!call.request.messages.is_empty());
    call
}

#[test]
fn native_string_stop_is_preserved() {
    let call = native_call(Some(json!("END")));
    assert_eq!(call.request.stop_sequences, Some(vec!["END".into()]));
}

#[test]
fn native_array_stop_is_preserved() {
    let call = native_call(Some(json!(["END", "结束"])));
    assert_eq!(
        call.request.stop_sequences,
        Some(vec!["END".into(), "结束".into()])
    );
}

#[test]
fn missing_native_stop_stays_absent() {
    assert_eq!(native_call(None).request.stop_sequences, None);
}
