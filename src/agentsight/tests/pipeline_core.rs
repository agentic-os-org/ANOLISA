//! Verify capture configuration, owned delivery, and maintenance without eBPF attachments.

#![cfg(target_os = "linux")]

mod common;

use agentsight::aggregator::{AggregatedResult, RemoveReason};
use agentsight::analyzer::Analyzer;
use agentsight::config::{AgentsightConfig, DEFAULT_MAX_HEADERS};
use agentsight::event::Event;
use agentsight::genai::{GenAIBuilder, GenAISemanticEvent};
use agentsight::pipeline_core::PipelineCore;
use agentsight::response_map::ResponseSessionMapper;
use std::collections::HashMap;
use std::time::{Duration, Instant};

fn feed(core: &mut PipelineCore, rw: i32, bytes: Vec<u8>) -> Vec<AggregatedResult> {
    core.process_event(Event::Ssl(common::make_ssl_event(
        4242, 0x4242, rw, bytes, "fixture",
    )))
}

#[test]
fn configured_header_cap_reaches_initial_and_interim_response_parsing() {
    let config = AgentsightConfig {
        max_headers: DEFAULT_MAX_HEADERS + 8,
        ..Default::default()
    };
    let mut core = PipelineCore::new(&config, Analyzer::new());
    let extra = (0..DEFAULT_MAX_HEADERS)
        .map(|i| format!("X-Fixture-{i}: value\r\n"))
        .collect::<String>();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: fixture.test\r\nContent-Length: 2\r\n{extra}\r\n{{}}"
    );
    assert!(feed(&mut core, 1, request.into_bytes()).is_empty());
    let response = format!(
        "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n{extra}\r\n{{}}"
    );
    let results = feed(&mut core, 0, response.into_bytes());
    let [AggregatedResult::HttpComplete(pair)] = results.as_slice() else {
        panic!("expected the final response, got {results:?}");
    };
    assert_eq!(pair.request.headers.len(), DEFAULT_MAX_HEADERS + 2);
    assert_eq!(pair.response.parsed.status_code, 200);
    assert_eq!(pair.response.parsed.headers.len(), DEFAULT_MAX_HEADERS + 2);
    assert_eq!(pair.response.body(), b"{}");
    assert!(core.drain_pending().is_empty());
}

#[test]
fn sse_retention_modes_preserve_semantic_delivery() {
    check_sse_delivery(false, false);
    check_sse_delivery(true, false);
}

#[test]
fn compact_sse_reassembles_fragmented_usage() {
    check_sse_delivery(false, true);
}

fn check_sse_delivery(retain_raw: bool, fragmented: bool) {
    let mut config = AgentsightConfig::default();
    config.runtime_limits.retain_raw_sse = retain_raw;
    let mut core = PipelineCore::new(&config, Analyzer::new());
    assert!(
        feed(
            &mut core,
            1,
            common::make_openai_request_bytes("fixture-model", "hello", true)
        )
        .is_empty()
    );
    assert!(feed(&mut core, 0, common::make_openai_sse_response_headers()).is_empty());
    let chunk = common::make_openai_sse_chunk("fixture-call", "fixture-model", "reply", 12, 8);
    let mut results = if fragmented {
        let split = chunk.len() / 2;
        assert!(feed(&mut core, 0, chunk[..split].to_vec()).is_empty());
        feed(&mut core, 0, chunk[split..].to_vec())
    } else {
        feed(&mut core, 0, chunk)
    };
    results.extend(feed(&mut core, 0, common::make_sse_done()));
    assert_eq!(results.len(), 1);
    let analysis = core.analyze_for_owned_delivery(results.pop().unwrap(), retain_raw);
    let (output, _) = GenAIBuilder::new().build_with_pending(
        &analysis,
        &ResponseSessionMapper::new(),
        &HashMap::new(),
    );
    let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
        panic!("expected one semantic call");
    };
    assert_eq!(call.model, "fixture-model");
    assert!(call.response.streamed);
    assert_eq!(call.token_usage.as_ref().unwrap().total_tokens, 20);
    assert_eq!(call.response.raw_body.is_some(), retain_raw);
    let response = serde_json::to_string(&call.response.messages).unwrap();
    assert!(response.contains("reply"));
    let late = feed(&mut core, 0, common::make_sse_done());
    // Unmatched tail bytes may retain response-only diagnostics, never a second call.
    assert!(
        late.iter()
            .all(|result| matches!(result, AggregatedResult::ResponseOnly { .. }))
    );
    for result in late {
        let analysis = core.analyze_for_owned_delivery(result, retain_raw);
        let (output, _) = GenAIBuilder::new().build_with_pending(
            &analysis,
            &ResponseSessionMapper::new(),
            &HashMap::new(),
        );
        assert!(
            !output
                .events
                .iter()
                .any(|event| matches!(event, GenAISemanticEvent::LLMCall(_)))
        );
    }
    assert!(core.drain_pending().is_empty());
}

#[test]
fn idle_maintenance_and_shutdown_return_unfinished_evidence_once() {
    for shutdown in [false, true] {
        let mut config = AgentsightConfig::default();
        config.runtime_limits.connection_hard_timeout_secs = 2;
        let mut core = PipelineCore::new(&config, Analyzer::new());
        assert!(
            feed(
                &mut core,
                1,
                common::make_openai_request_bytes("fixture-model", "unfinished", false)
            )
            .is_empty()
        );
        let now = Instant::now() + Duration::from_secs(3);
        let results = if shutdown {
            core.drain_pending()
        } else {
            core.tick(now)
        };
        let expected = if shutdown {
            RemoveReason::Shutdown
        } else {
            RemoveReason::HardTimeout
        };
        assert!(
            matches!(results.as_slice(), [AggregatedResult::Abandoned { reason, .. }] if *reason == expected)
        );
        assert!(!core.analyze_aggregated(&results[0]).is_empty());
        assert!(core.tick(now).is_empty());
        assert!(core.drain_pending().is_empty());
    }
}
