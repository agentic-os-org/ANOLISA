//! Pipeline integration tests: feed synthetic SslEvents through the full
//! parser → aggregator → analyzer → genai builder → SQLite chain.
//!
//! No BPF probes needed — exercises the data path with in-memory components.

use std::collections::HashMap;

mod common;

use agentsight::aggregator::Aggregator;
use agentsight::analyzer::{AnalysisResult, Analyzer};
use agentsight::event::Event;
use agentsight::genai::GenAIBuilder;
use agentsight::genai::semantic::{GenAISemanticEvent, MessagePart};
use agentsight::parser::Parser;
use agentsight::response_map::ResponseSessionMapper;

#[test]
fn native_event_clock_reaches_semantic_output() {
    use agentsight::probes::sslsniff::{SslEvent, bpf::probe_SSL_data_t as Raw};
    use std::mem::offset_of;
    use std::time::{SystemTime, UNIX_EPOCH};

    let wall_ns = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    };
    // Allow the bounded clock-sampling bracket; a container-epoch shift is far larger.
    let start = wall_ns();
    let parser = Parser::new();
    let mut aggregator = Aggregator::new();
    let analyzer = Analyzer::new();
    let builder = GenAIBuilder::new();
    let mapper = ResponseSessionMapper::new();
    let cache = HashMap::new();
    let mut result = Vec::new();
    for (rw, payload) in [
        (
            1_i32,
            common::make_openai_request_bytes("clock-fixture", "clock-check", false),
        ),
        (
            0_i32,
            common::make_openai_json_response_bytes("clock-response", "clock-fixture", "ok", 5, 3),
        ),
    ] {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid writable timespec is supplied to the clock syscall.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) },
            0
        );
        let ktime = ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;
        let header_len = offset_of!(Raw, buf);
        let mut bytes = vec![0_u8; header_len + payload.len()];
        for (offset, value) in [
            (offset_of!(Raw, timestamp_ns), ktime.to_ne_bytes().to_vec()),
            (offset_of!(Raw, pid), 5070_u32.to_ne_bytes().to_vec()),
            (offset_of!(Raw, tid), 5070_u32.to_ne_bytes().to_vec()),
            (
                offset_of!(Raw, len),
                (payload.len() as u32).to_ne_bytes().to_vec(),
            ),
            (
                offset_of!(Raw, buf_size),
                (payload.len() as u32).to_ne_bytes().to_vec(),
            ),
            (offset_of!(Raw, rw), rw.to_ne_bytes().to_vec()),
            (offset_of!(Raw, ssl_ptr), 0xCAFE_u64.to_ne_bytes().to_vec()),
            (offset_of!(Raw, comm), b"python3\0".to_vec()),
        ] {
            bytes[offset..offset + value.len()].copy_from_slice(&value);
        }
        bytes[header_len..].copy_from_slice(&payload);
        let ssl = SslEvent::from_bytes(&bytes).expect("native clock must decode");
        assert!(ssl.timestamp_ns >= start.saturating_sub(5_000_000));
        for aggregated in aggregator.process_result(parser.parse_event(Event::Ssl(ssl))) {
            let analyzed = analyzer.analyze_aggregated(&aggregated);
            result.extend(
                builder
                    .build_with_pending(&analyzed, &mapper, &cache)
                    .0
                    .events,
            );
        }
    }
    let end = wall_ns();
    let calls: Vec<_> = result
        .iter()
        .filter_map(|e| match e {
            GenAISemanticEvent::LLMCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].start_timestamp_ns >= start.saturating_sub(5_000_000));
    assert!(calls[0].end_timestamp_ns <= end + 5_000_000);
    assert!(calls[0].end_timestamp_ns >= calls[0].start_timestamp_ns);
}

/// Run a sequence of SslEvents through the full pipeline, return any GenAI events produced.
fn run_pipeline(ssl_events: Vec<(u32, u64, i32, Vec<u8>, &str)>) -> Vec<GenAISemanticEvent> {
    let parser = Parser::new();
    let mut aggregator = Aggregator::new();
    let analyzer = Analyzer::new();
    let builder = GenAIBuilder::new();
    let mapper = ResponseSessionMapper::new();
    let pid_cache: HashMap<u32, String> = HashMap::new();

    let mut all_genai_events = Vec::new();

    for (pid, ssl_ptr, rw, buf, comm) in ssl_events {
        let ssl_event = common::make_ssl_event(pid, ssl_ptr, rw, buf, comm);
        let event = Event::Ssl(ssl_event);

        let parse_result = parser.parse_event(event);
        let aggregated_results = aggregator.process_result(parse_result);

        for agg_result in &aggregated_results {
            let analysis_results = analyzer.analyze_aggregated(agg_result);
            let (output, _pending_info) =
                builder.build_with_pending(&analysis_results, &mapper, &pid_cache);

            all_genai_events.extend(output.events);
        }
    }

    all_genai_events
}

/// Same as [`run_pipeline`] but stops at the analyzer, so tests can assert on
/// the `TokenRecord` that lands in the token database.
fn run_analysis(ssl_events: Vec<(u32, u64, i32, Vec<u8>, &str)>) -> Vec<AnalysisResult> {
    let parser = Parser::new();
    let mut aggregator = Aggregator::new();
    let analyzer = Analyzer::new();

    let mut all = Vec::new();
    for (pid, ssl_ptr, rw, buf, comm) in ssl_events {
        let ssl_event = common::make_ssl_event(pid, ssl_ptr, rw, buf, comm);
        let parse_result = parser.parse_event(Event::Ssl(ssl_event));
        for agg_result in &aggregator.process_result(parse_result) {
            all.extend(analyzer.analyze_aggregated(agg_result));
        }
    }
    all
}

/// Pull the single LLMCall out of a pipeline run.
fn expect_llm_call(events: &[GenAISemanticEvent]) -> &agentsight::genai::LLMCall {
    match events.first().expect("pipeline produced no GenAI event") {
        GenAISemanticEvent::LLMCall(call) => call,
        other => panic!("expected LLMCall, got {other:?}"),
    }
}

/// Pull the token record out of an analyzer run.
fn expect_token_record(results: &[AnalysisResult]) -> &agentsight::analyzer::TokenRecord {
    results
        .iter()
        .find_map(|r| match r {
            AnalysisResult::Token(t) => Some(t),
            _ => None,
        })
        .expect("analyzer produced no token record")
}

/// Concatenate all text parts of an assistant output message.
fn assistant_text(call: &agentsight::genai::LLMCall) -> String {
    call.response
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| match p {
            MessagePart::Text { content } => Some(content.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn test_openai_sse_pipeline() {
    let pid = 5000u32;
    let ssl_ptr = 0xA000u64;
    let comm = "node";

    let request_bytes = common::make_openai_request_bytes("gpt-4o", "Say hello in 3 words", true);
    let resp_headers = common::make_openai_sse_response_headers();
    let sse_chunk =
        common::make_openai_sse_chunk("chatcmpl-test-001", "gpt-4o", "Hello there friend!", 10, 5);
    let sse_done = common::make_sse_done();

    let events = vec![
        (pid, ssl_ptr, 1, request_bytes, comm),
        (pid, ssl_ptr, 0, resp_headers, comm),
        (pid, ssl_ptr, 0, sse_chunk, comm),
        (pid, ssl_ptr, 0, sse_done, comm),
    ];

    let genai_events = run_pipeline(events);

    assert!(
        !genai_events.is_empty(),
        "pipeline should produce at least one GenAI event"
    );

    let call = match &genai_events[0] {
        GenAISemanticEvent::LLMCall(call) => call,
        other => panic!("expected LLMCall, got {other:?}"),
    };

    assert_eq!(call.provider, "openai");
    assert_eq!(call.model, "gpt-4o");
    assert_eq!(call.pid, pid as i32);
    assert_eq!(call.process_name, comm);
}

#[test]
fn test_openai_non_streaming_pipeline() {
    let pid = 5001u32;
    let ssl_ptr = 0xB000u64;
    let comm = "python3";

    let request_bytes = common::make_openai_request_bytes("gpt-4o-mini", "Count to 3", false);
    let response_bytes = common::make_openai_json_response_bytes(
        "chatcmpl-test-002",
        "gpt-4o-mini",
        "1, 2, 3.",
        8,
        4,
    );

    let events = vec![
        (pid, ssl_ptr, 1, request_bytes, comm),
        (pid, ssl_ptr, 0, response_bytes, comm),
    ];

    let result = run_pipeline(events);

    assert!(
        !result.is_empty(),
        "non-streaming pipeline should produce GenAI events"
    );

    let call = match &result[0] {
        GenAISemanticEvent::LLMCall(call) => call,
        other => panic!("expected LLMCall, got {other:?}"),
    };

    assert_eq!(call.provider, "openai");
    assert_eq!(call.model, "gpt-4o-mini");

    // Token usage must be extracted from the non-streaming JSON `usage` field
    // (the fix in #789: extract_token_from_json_body on HttpComplete). Before
    // that fix, non-streaming responses reported zero/absent token usage.
    let usage = call
        .token_usage
        .as_ref()
        .expect("non-streaming call must carry token usage");
    assert_eq!(usage.input_tokens, 8, "input tokens from JSON usage");
    assert_eq!(usage.output_tokens, 4, "output tokens from JSON usage");
}

#[test]
fn test_anthropic_sse_pipeline() {
    let pid = 5002u32;
    let ssl_ptr = 0xC000u64;
    let comm = "claude";

    let request_bytes = common::make_anthropic_request_bytes("claude-sonnet-4-20250514", "Say hi");
    let chunks = common::make_anthropic_sse_chunks(
        "msg_test_003",
        "claude-sonnet-4-20250514",
        "Hi there!",
        12,
        3,
    );

    let mut events = vec![(pid, ssl_ptr, 1, request_bytes, comm)];
    for chunk in chunks {
        events.push((pid, ssl_ptr, 0, chunk, comm));
    }

    let result = run_pipeline(events);

    assert!(
        !result.is_empty(),
        "Anthropic SSE pipeline should produce GenAI events"
    );

    let call = match &result[0] {
        GenAISemanticEvent::LLMCall(call) => call,
        other => panic!("expected LLMCall, got {other:?}"),
    };

    assert_eq!(call.provider, "anthropic");
    assert!(
        call.model.contains("claude"),
        "model should contain 'claude', got: {}",
        call.model
    );
}

// ---------------------------------------------------------------------------
// DashScope / Bailian native protocol (issue #2910)
//
// These endpoints are what the "Bailian" node types in Dify / LangGraph emit.
// They are *not* the OpenAI-compatible mode: the request nests messages in an
// `input` object, the response wraps the answer in `output` with no top-level
// `model`, `usage` reuses Anthropic's field names, and the SSE stream has no
// `data: [DONE]` terminator.
// ---------------------------------------------------------------------------

/// Before the fix the `!is_llm && !is_sse` gate dropped non-streaming native
/// calls outright, so they were invisible on the Dashboard.
#[test]
fn test_dashscope_native_non_streaming_pipeline() {
    let pid = 5100u32;
    let ssl_ptr = 0xD000u64;
    let comm = "python3";

    let request_bytes = common::make_dashscope_native_request_bytes(
        common::DASHSCOPE_TEXT_GENERATION_PATH,
        "qwen-plus",
        "You are helpful.",
        "Count to 3",
        false,
    );
    let response_bytes =
        common::make_dashscope_native_json_response_bytes("req-native-1", "1, 2, 3.", 22, 8, None);

    let events = vec![
        (pid, ssl_ptr, 1, request_bytes, comm),
        (pid, ssl_ptr, 0, response_bytes, comm),
    ];

    let genai_events = run_pipeline(events);
    let call = expect_llm_call(&genai_events);

    assert_eq!(call.provider, "dashscope");
    assert_eq!(call.model, "qwen-plus", "model must come from request body");

    // Conversation reconstruction: `input.messages` on the request side,
    // `output.choices[].message` on the response side.
    let roles: Vec<&str> = call
        .request
        .messages
        .iter()
        .map(|m| m.role.as_str())
        .collect();
    assert_eq!(roles, vec!["system", "user"]);
    assert_eq!(assistant_text(call), "1, 2, 3.");

    let usage = call
        .token_usage
        .as_ref()
        .expect("native non-streaming call must carry token usage");
    assert_eq!(usage.input_tokens, 22);
    assert_eq!(usage.output_tokens, 8);
}

/// Streaming native calls did reach the Dashboard (via the `is_sse` escape
/// hatch) but reported `provider: unknown` and carried no conversation.
#[test]
fn test_dashscope_native_sse_pipeline() {
    let pid = 5101u32;
    let ssl_ptr = 0xD100u64;
    let comm = "python3";

    let request_bytes = common::make_dashscope_native_request_bytes(
        common::DASHSCOPE_TEXT_GENERATION_PATH,
        "qwen-plus",
        "You are helpful.",
        "你好吗",
        true,
    );
    let chunks = common::make_dashscope_native_sse_chunks("req-native-2", "我很好", 22, 3);

    let mut events = vec![(pid, ssl_ptr, 1, request_bytes, comm)];
    for chunk in chunks {
        events.push((pid, ssl_ptr, 0, chunk, comm));
    }

    let genai_events = run_pipeline(events);
    let call = expect_llm_call(&genai_events);

    assert_eq!(call.provider, "dashscope");
    assert_eq!(call.model, "qwen-plus");
    assert!(call.response.streamed);

    let roles: Vec<&str> = call
        .request
        .messages
        .iter()
        .map(|m| m.role.as_str())
        .collect();
    assert_eq!(roles, vec!["system", "user"]);

    // Cumulative chunks must be de-duplicated, not concatenated.
    assert_eq!(assistant_text(call), "我很好");
    assert_eq!(
        call.response.messages[0].finish_reason.as_deref(),
        Some("stop"),
        "native finish_reason must be surfaced so the stream reads as complete"
    );
}

// ---------------------------------------------------------------------------
// Replay of REAL captured DashScope native responses.
//
// The payloads in tests/fixtures/dashscope_native/ are verbatim bodies from
// live `dashscope.aliyuncs.com` calls. Hand-written fixtures encoded two wrong
// assumptions about this protocol (`image_tokens` being additive, and
// `total_tokens` being Anthropic-exclusive), so these replays exist to keep the
// parser honest against bytes nobody on this side authored.
// ---------------------------------------------------------------------------

/// Wrap a captured JSON body in the HTTP response framing the wire carries.
fn real_json_response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

#[test]
fn test_real_dashscope_native_nonstream_replay() {
    let body = include_str!("fixtures/dashscope_native/text_generation_nonstream.json");
    let request = common::make_dashscope_native_request_bytes(
        common::DASHSCOPE_TEXT_GENERATION_PATH,
        "qwen-plus",
        "You are a helpful assistant.",
        "只回复三个字：收到了",
        false,
    );

    let genai_events = run_pipeline(vec![
        (5200, 0xE000, 1, request, "python3"),
        (5200, 0xE000, 0, real_json_response(body), "python3"),
    ]);
    let call = expect_llm_call(&genai_events);

    assert_eq!(call.provider, "dashscope");
    assert_eq!(call.model, "qwen-plus");
    assert_eq!(assistant_text(call), "收到了");
    let usage = call.token_usage.as_ref().expect("token usage");
    // Real values: input_tokens 25, output_tokens 1, total_tokens 26.
    assert_eq!(usage.input_tokens, 25);
    assert_eq!(usage.output_tokens, 1);
    assert_eq!(usage.total_tokens, 26);
}

#[test]
fn test_real_dashscope_native_stream_replay() {
    let sse = include_str!("fixtures/dashscope_native/text_generation_stream.sse");
    let request = common::make_dashscope_native_request_bytes(
        common::DASHSCOPE_TEXT_GENERATION_PATH,
        "qwen-plus",
        "You are a helpful assistant.",
        "从1数到5，只输出数字",
        true,
    );

    let mut events = vec![(5201u32, 0xE100u64, 1, request, "python3")];
    events.push((
        5201,
        0xE100,
        0,
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec(),
        "python3",
    ));
    // Feed each `id:`-delimited frame as its own TLS record, as observed.
    for frame in sse.split("\n\nid:").enumerate().map(|(i, f)| {
        if i == 0 {
            f.to_string()
        } else {
            format!("id:{f}")
        }
    }) {
        let mut bytes = frame.into_bytes();
        bytes.extend_from_slice(b"\n\n");
        events.push((5201, 0xE100, 0, bytes, "python3"));
    }

    let genai_events = run_pipeline(events);
    let call = expect_llm_call(&genai_events);

    assert_eq!(call.provider, "dashscope");
    assert_eq!(call.model, "qwen-plus");
    assert!(call.response.streamed);
    // Chunks are cumulative ("1", "1 2", "1 2 3 ", "1 2 3 4 5", "1 2 3 4 5"):
    // concatenating them would yield "11 21 2 3 ...".
    assert_eq!(assistant_text(call), "1 2 3 4 5");
    assert_eq!(
        call.response.messages[0].finish_reason.as_deref(),
        Some("stop"),
        "terminal chunk carries stop; earlier ones carry the literal \"null\""
    );
    let usage = call.token_usage.as_ref().expect("token usage");
    assert_eq!(usage.input_tokens, 28);
    assert_eq!(usage.output_tokens, 9);
}

#[test]
fn test_real_dashscope_native_multimodal_replay() {
    let body = include_str!("fixtures/dashscope_native/multimodal_generation_nonstream.json");
    let request = common::make_dashscope_native_request_bytes(
        common::DASHSCOPE_MULTIMODAL_GENERATION_PATH,
        "qwen-vl-plus",
        "You are a helpful assistant.",
        "图里有什么？",
        false,
    );

    let events = vec![
        (5202u32, 0xE200u64, 1, request, "python3"),
        (5202, 0xE200, 0, real_json_response(body), "python3"),
    ];

    let genai_events = run_pipeline(events.clone());
    let call = expect_llm_call(&genai_events);

    assert_eq!(call.provider, "dashscope");
    assert_eq!(call.model, "qwen-vl-plus");
    assert!(
        assistant_text(call).contains("沙滩"),
        "multimodal content blocks must be flattened to text, got: {:?}",
        assistant_text(call).chars().take(40).collect::<String>()
    );

    let usage = call.token_usage.as_ref().expect("token usage");
    // Real usage: input_tokens 1261 (= image 1249 + text 12), output 382,
    // total 1643. Adding image_tokens on top would report 2510 input.
    assert_eq!(
        usage.input_tokens, 1261,
        "image_tokens is an itemisation of input_tokens, not an addition"
    );
    assert_eq!(usage.output_tokens, 382);
    assert_eq!(usage.total_tokens, 1643, "matches provider total_tokens");

    let results = run_analysis(events);
    let record = expect_token_record(&results);
    assert_eq!(record.provider, "dashscope");
    assert_eq!(record.input_tokens, 1261);
    assert_eq!(record.model.as_deref(), Some("qwen-vl-plus"));
}

/// Multimodal native usage itemises the input side rather than adding to it.
///
/// Real response: `input_tokens_details {image_tokens 1249, text_tokens 12}` with
/// `input_tokens 1261` and `total_tokens 1643` (= 1261 + 382 output). Counting
/// `image_tokens` on top of `input_tokens` would roughly double the reported
/// input cost of every image call.
#[test]
fn test_dashscope_native_multimodal_image_tokens_pipeline() {
    let pid = 5102u32;
    let ssl_ptr = 0xD200u64;
    let comm = "python3";

    let request_bytes = common::make_dashscope_native_request_bytes(
        common::DASHSCOPE_MULTIMODAL_GENERATION_PATH,
        "qwen-vl-plus",
        "You are helpful.",
        "图里有什么？",
        false,
    );
    let response_bytes = common::make_dashscope_native_json_response_bytes(
        "req-native-3",
        "这是一只猫。",
        1261, // == 1249 image + 12 text, exactly as DashScope reports it
        382,
        Some(1249),
    );

    let events = vec![
        (pid, ssl_ptr, 1, request_bytes, comm),
        (pid, ssl_ptr, 0, response_bytes, comm),
    ];

    let genai_events = run_pipeline(events.clone());
    let call = expect_llm_call(&genai_events);
    assert_eq!(call.provider, "dashscope");
    assert_eq!(call.model, "qwen-vl-plus");

    // Multimodal replies use content blocks, so text must still be recovered.
    assert_eq!(assistant_text(call), "这是一只猫。");

    let usage = call
        .token_usage
        .as_ref()
        .expect("multimodal call must carry token usage");
    assert_eq!(
        usage.input_tokens, 1261,
        "image_tokens is already inside input_tokens — must not be added again"
    );
    assert_eq!(usage.output_tokens, 382);
    assert_eq!(
        usage.total_tokens, 1643,
        "must reconcile with the provider-reported total_tokens"
    );

    // The token record persisted to the token database must agree.
    let results = run_analysis(events);
    let record = expect_token_record(&results);
    assert_eq!(
        record.provider, "dashscope",
        "native usage must not be mislabelled anthropic"
    );
    assert_eq!(record.input_tokens, 1261);
    assert_eq!(
        record.model.as_deref(),
        Some("qwen-vl-plus"),
        "native responses carry no model, so it must be backfilled from the request"
    );
}
