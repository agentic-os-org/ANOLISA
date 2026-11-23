//! Replay synthetic Qoder gateway traffic through capture, analysis, and GenAI delivery.

#![cfg(target_os = "linux")]

mod common;

use agentsight::aggregator::AggregatedResult;
use agentsight::analyzer::{AnalysisResult, Analyzer};
use agentsight::config::AgentsightConfig;
use agentsight::event::Event;
use agentsight::genai::{GenAIBuilder, GenAISemanticEvent, MessagePart};
use agentsight::pipeline_core::PipelineCore;
use agentsight::response_map::ResponseSessionMapper;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::collections::HashMap;

const PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation?Encode=1";
const SESSION: &str = "00000000-0000-0000-0000-000000000001";

fn encode(value: &Value) -> String {
    let alphabet = b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
    let standard = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = STANDARD
        .encode(value.to_string())
        .bytes()
        .map(|byte| {
            if byte == b'=' {
                b'$'
            } else {
                alphabet[standard
                    .iter()
                    .position(|candidate| *candidate == byte)
                    .unwrap()]
            }
        })
        .collect();
    let third = bytes.len() / 3;
    String::from_utf8(
        [
            &bytes[bytes.len() - third..],
            &bytes[third..bytes.len() - third],
            &bytes[..third],
        ]
        .concat(),
    )
    .unwrap()
}

fn request() -> Vec<u8> {
    let body = encode(&json!({
        "session_id": SESSION, "system": "Be concise",
        "messages": [{"role":"user","content":"Find the fixture"}],
        "model_config": {"key":"fixture-model"},
        "parameters": {"max_tokens":32}, "stream":true
    }));
    format!(
        "POST {PATH} HTTP/1.1\r\nHost: gateway.example.test\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn envelope(value: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"body":value.to_string(),"statusCode":"OK","statusCodeValue":200})
    )
}

fn feed(core: &mut PipelineCore, rw: i32, bytes: Vec<u8>) -> Vec<AggregatedResult> {
    core.process_event(Event::Ssl(common::make_ssl_event(
        4242, 42, rw, bytes, "qodercli",
    )))
}

#[test]
fn wrapped_usage_and_object_arguments_survive_all_delivery_modes() {
    for chunked in [false, true] {
        for retain_raw in [false, true] {
            for owned in [false, true] {
                let mut config = AgentsightConfig::default();
                config.runtime_limits.retain_raw_sse = retain_raw;
                let mut core = PipelineCore::new(&config, Analyzer::new());
                assert!(feed(&mut core, 1, request()).is_empty());
                let headers = if chunked {
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec()
                } else {
                    common::make_openai_sse_response_headers()
                };
                assert!(feed(&mut core, 0, headers).is_empty());
                let content = envelope(json!({"id":"fixture-call","model":"fixture-model",
                "choices":[{"index":0,"delta":{"role":"assistant","content":"Found it","reasoning_content":"Check fixture"}}]}));
                let tools = envelope(json!({"id":"fixture-call","model":"fixture-model",
                "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"fixture-tool","type":"function",
                    "function":{"name":"lookup","arguments":{"query":"fixture"}}}]},"finish_reason":"tool_calls"}]}));
                let usage = envelope(
                    json!({"id":"fixture-call","model":"fixture-model","choices":[],
                "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120,
                    "prompt_tokens_details":{"cached_tokens":40}}}),
                );
                let wire = if chunked {
                    let body = format!("{content}{tools}{usage}");
                    format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).into_bytes()
                } else {
                    format!("{content}{tools}{usage}data: [DONE]\n\n").into_bytes()
                };
                // 每个封装都跨采集记录，真实网关只通过 HTTP framing 结束。
                let mut results = Vec::new();
                for bytes in wire.chunks(113) {
                    results.extend(feed(&mut core, 0, bytes.to_vec()));
                }
                results.extend(core.drain_pending());
                assert_eq!(results.len(), 1);
                let result = results.pop().unwrap();
                let analysis = if owned {
                    core.analyze_for_owned_delivery(result, retain_raw)
                } else {
                    core.analyze_aggregated(&result)
                };
                assert!(
                    analysis
                        .iter()
                        .any(|value| matches!(value, AnalysisResult::Token(token)
                if token.input_tokens == 100 && token.output_tokens == 20))
                );
                let (output, _) = GenAIBuilder::new().build_with_pending(
                    &analysis,
                    &ResponseSessionMapper::new(),
                    &HashMap::new(),
                );
                let [GenAISemanticEvent::LLMCall(call)] = output.events.as_slice() else {
                    panic!("expected one Qoder call, got {:?}", output.events);
                };
                assert_eq!(call.model, "fixture-model");
                assert_eq!(
                    call.metadata.get("session_id").map(String::as_str),
                    Some(SESSION)
                );
                let tokens = call.token_usage.as_ref().unwrap();
                assert_eq!(tokens.total_tokens, 120);
                assert_eq!(tokens.cache_read_input_tokens, Some(40));
                let request = serde_json::to_string(&call.request.messages).unwrap();
                assert!(request.contains("Be concise"));
                assert!(request.contains("Find the fixture"));
                let response = serde_json::to_value(&call.response.messages).unwrap();
                let response_text = response.to_string();
                assert!(
                    response_text.contains("Found it"),
                    "raw={retain_raw} owned={owned}: {response_text}; analysis={analysis:?}"
                );
                assert!(response_text.contains("Check fixture"));
                assert_eq!(response_text.matches("Found it").count(), 1);
                assert!(response_text.contains("lookup"));
                assert!(response_text.contains("fixture"));
                assert!(call.response.messages.iter().flat_map(|message| &message.parts)
                .any(|part| matches!(part, MessagePart::ToolCall { name, arguments: Some(arguments), .. }
                    if name == "lookup" && arguments == &json!({"query":"fixture"}))));
                assert!(core.drain_pending().is_empty());
            }
        }
    }
}

#[test]
fn qoder_configuration_streams_are_not_llm_calls() {
    for retain_raw in [false, true] {
        let mut config = AgentsightConfig::default();
        config.runtime_limits.retain_raw_sse = retain_raw;
        let mut core = PipelineCore::new(&config, Analyzer::new());
        assert!(feed(&mut core, 1, b"GET /api/v1/qcs/config/stream?ns=qodercli-feature-gates HTTP/1.1\r\nHost: gateway.example.test\r\n\r\n".to_vec()).is_empty());
        assert!(feed(&mut core, 0, common::make_openai_sse_response_headers()).is_empty());
        let mut results = feed(
            &mut core,
            0,
            b"event: connected\ndata: {}\n\ndata: [DONE]\n\n".to_vec(),
        );
        results.extend(core.drain_pending());
        assert!(!results.is_empty());
        for result in results {
            let analysis = core.analyze_aggregated(&result);
            let (output, _) = GenAIBuilder::new().build_with_pending(
                &analysis,
                &ResponseSessionMapper::new(),
                &HashMap::new(),
            );
            assert!(
                output.events.is_empty(),
                "配置订阅不是模型推理: {:?}",
                output.events
            );
        }
    }
}

#[test]
fn encoded_request_only_keeps_the_prompt() {
    let mut core = PipelineCore::new(&AgentsightConfig::default(), Analyzer::new());
    assert!(feed(&mut core, 1, request()).is_empty());
    let results = core.drain_pending();
    let analysis = core.analyze_aggregated(&results[0]);
    assert!(
        analysis
            .iter()
            .any(|value| matches!(value, AnalysisResult::Message(message)
        if message.model() == Some("fixture-model")))
    );
}
