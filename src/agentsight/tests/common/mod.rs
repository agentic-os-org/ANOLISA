//! Shared test utilities for agentsight integration and unit tests.
//!
//! Consolidates factory functions previously duplicated across 8+ modules.

#![allow(dead_code)]

use agentsight::probes::sslsniff::SslEvent;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// Create an SslEvent with the given connection identity and payload.
pub fn make_ssl_event(pid: u32, ssl_ptr: u64, rw: i32, buf: Vec<u8>, comm: &str) -> SslEvent {
    SslEvent {
        source: 0,
        timestamp_ns: 1_000_000_000,
        delta_ns: 0,
        pid,
        tid: pid,
        uid: 0,
        len: buf.len() as u32,
        rw,
        comm: comm.to_string(),
        buf,
        is_handshake: false,
        ssl_ptr,
    }
}

/// Build a minimal OpenAI chat completion POST request as raw HTTP bytes.
pub fn make_openai_request_bytes(model: &str, user_message: &str, stream: bool) -> Vec<u8> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": user_message}],
        "stream": stream,
    });
    let body_str = serde_json::to_string(&body).unwrap();
    format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: api.openai.com\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body_str.len(),
        body_str
    )
    .into_bytes()
}

/// Build OpenAI SSE response headers as raw HTTP bytes.
pub fn make_openai_sse_response_headers() -> Vec<u8> {
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec()
}

/// Build an OpenAI SSE data chunk as raw bytes (no HTTP headers).
pub fn make_openai_sse_chunk(
    response_id: &str,
    model: &str,
    content: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<u8> {
    let chunk = serde_json::json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    });
    format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()).into_bytes()
}

/// Build the SSE [DONE] marker as raw bytes.
pub fn make_sse_done() -> Vec<u8> {
    b"data: [DONE]\n\n".to_vec()
}

/// Build an OpenAI non-streaming JSON response as raw HTTP bytes.
pub fn make_openai_json_response_bytes(
    response_id: &str,
    model: &str,
    content: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<u8> {
    let body = serde_json::json!({
        "id": response_id,
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    });
    let body_str = serde_json::to_string(&body).unwrap();
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body_str.len(),
        body_str
    )
    .into_bytes()
}

/// Build Anthropic SSE response as separate chunks (headers, events, stop).
pub fn make_anthropic_sse_chunks(
    response_id: &str,
    model: &str,
    content: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<Vec<u8>> {
    let message_start = serde_json::json!({
        "type": "message_start",
        "message": {
            "id": response_id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "usage": {"input_tokens": input_tokens, "output_tokens": 0}
        }
    });
    let content_block = serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": content}
    });
    let message_delta = serde_json::json!({
        "type": "message_delta",
        "delta": {"stop_reason": "end_turn"},
        "usage": {"output_tokens": output_tokens}
    });
    let message_stop = serde_json::json!({"type": "message_stop"});

    vec![
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec(),
        format!(
            "event: message_start\ndata: {}\n\n",
            serde_json::to_string(&message_start).unwrap()
        )
        .into_bytes(),
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&content_block).unwrap()
        )
        .into_bytes(),
        format!(
            "event: message_delta\ndata: {}\n\n",
            serde_json::to_string(&message_delta).unwrap()
        )
        .into_bytes(),
        format!(
            "event: message_stop\ndata: {}\n\n",
            serde_json::to_string(&message_stop).unwrap()
        )
        .into_bytes(),
    ]
}

/// Build an Anthropic Messages API POST request as raw HTTP bytes.
pub fn make_anthropic_request_bytes(model: &str, user_message: &str) -> Vec<u8> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "stream": true,
        "messages": [{"role": "user", "content": user_message}]
    });
    let body_str = serde_json::to_string(&body).unwrap();
    format!(
        "POST /v1/messages HTTP/1.1\r\n\
         Host: api.anthropic.com\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         X-Api-Key: sk-test\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         \r\n\
         {}",
        body_str.len(),
        body_str
    )
    .into_bytes()
}

/// DashScope/Bailian **native** protocol path for plain-text generation.
pub const DASHSCOPE_TEXT_GENERATION_PATH: &str = "/api/v1/services/aigc/text-generation/generation";

/// DashScope/Bailian **native** protocol path for multimodal generation.
pub const DASHSCOPE_MULTIMODAL_GENERATION_PATH: &str =
    "/api/v1/services/aigc/multimodal-generation/generation";

/// Build a DashScope native-protocol POST request as raw HTTP bytes.
///
/// Unlike the OpenAI-compatible mode, the messages array is nested inside an
/// `input` **object** and sampling knobs live under `parameters`. Streaming is
/// requested via the `X-DashScope-SSE` header, not a body field.
pub fn make_dashscope_native_request_bytes(
    path: &str,
    model: &str,
    system_message: &str,
    user_message: &str,
    stream: bool,
) -> Vec<u8> {
    let body = serde_json::json!({
        "model": model,
        "input": {
            "messages": [
                {"role": "system", "content": system_message},
                {"role": "user", "content": user_message},
            ]
        },
        "parameters": {"result_format": "message"},
    });
    let body_str = serde_json::to_string(&body).unwrap();
    let sse_header = if stream {
        "X-DashScope-SSE: enable\r\n"
    } else {
        ""
    };
    format!(
        "POST {path} HTTP/1.1\r\n\
         Host: ws-abc123.cn-beijing.maas.aliyuncs.com\r\n\
         Content-Type: application/json\r\n\
         {sse_header}\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body_str.len(),
        body_str
    )
    .into_bytes()
}

/// Build a DashScope native-protocol non-streaming JSON response.
///
/// Shapes verified against real `dashscope.aliyuncs.com` responses: the answer
/// sits under `output.choices[]`, there is **no** top-level `model`, and `usage`
/// uses Anthropic-style `input_tokens`/`output_tokens` plus `total_tokens` and
/// `prompt_tokens_details.cached_tokens`.
///
/// `image_tokens`, when present, is an *itemisation* of `input_tokens` (real
/// response: `input_tokens_details {image 1249, text 12}` with
/// `input_tokens 1261`), not an addition — so callers pass the already-total
/// `input_tokens`.
pub fn make_dashscope_native_json_response_bytes(
    request_id: &str,
    content: &str,
    input_tokens: u32,
    output_tokens: u32,
    image_tokens: Option<u32>,
) -> Vec<u8> {
    let mut usage = serde_json::json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens,
        "prompt_tokens_details": {"cached_tokens": 0},
    });
    if let Some(image_tokens) = image_tokens {
        usage["image_tokens"] = serde_json::json!(image_tokens);
        usage["input_tokens_details"] = serde_json::json!({
            "image_tokens": image_tokens,
            "text_tokens": input_tokens.saturating_sub(image_tokens),
        });
        usage["output_tokens_details"] = serde_json::json!({"text_tokens": output_tokens});
    }
    let message = if image_tokens.is_some() {
        // Multimodal replies carry content blocks, not a bare string.
        serde_json::json!({
            "role": "assistant",
            "content": [{"text": content}],
            "reasoning_content": "",
        })
    } else {
        serde_json::json!({"role": "assistant", "content": content})
    };
    let body = serde_json::json!({
        "output": {"choices": [{"finish_reason": "stop", "message": message}]},
        "usage": usage,
        "request_id": request_id,
    });
    let body_str = serde_json::to_string(&body).unwrap();
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        body_str.len(),
        body_str
    )
    .into_bytes()
}

/// Build a DashScope native-protocol SSE stream as separate chunks.
///
/// Framing verified against a real streaming response: `id:` / `event:result` /
/// `:HTTP_STATUS/200` fields, `data:` with no space after the colon, cumulative
/// (`incremental_output=false`) content, the **literal string** `"null"` as the
/// in-progress finish_reason, and **no** `data: [DONE]` terminator.
pub fn make_dashscope_native_sse_chunks(
    request_id: &str,
    content: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> Vec<Vec<u8>> {
    let mut chunks = vec![b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec()];
    let chars: Vec<char> = content.chars().collect();
    for (i, _) in chars.iter().enumerate() {
        let cumulative: String = chars[..=i].iter().collect();
        let last = i + 1 == chars.len();
        let out_tok = if last { output_tokens } else { i as u32 + 1 };
        let payload = serde_json::json!({
            "output": {"choices": [{
                "message": {"content": cumulative, "role": "assistant"},
                "finish_reason": if last { "stop" } else { "null" },
            }]},
            "usage": {
                "total_tokens": input_tokens + out_tok,
                "output_tokens": out_tok,
                "input_tokens": input_tokens,
                "prompt_tokens_details": {"cached_tokens": 0},
            },
            "request_id": request_id,
        });
        chunks.push(
            format!(
                "id:{}\nevent:result\n:HTTP_STATUS/200\ndata:{}\n\n",
                i + 1,
                serde_json::to_string(&payload).unwrap()
            )
            .into_bytes(),
        );
    }
    chunks
}

/// Generate a unique temporary directory for test isolation.
pub fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("agentsight-int-{pid}-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Construct a minimal LLMCall for testing.
pub fn make_test_llm_call(call_id: &str) -> agentsight::genai::LLMCall {
    use agentsight::genai::semantic::{LLMRequest, LLMResponse};
    agentsight::genai::LLMCall {
        call_id: call_id.to_string(),
        start_timestamp_ns: 1_000_000_000,
        end_timestamp_ns: 2_000_000_000,
        duration_ns: 1_000_000_000,
        provider: "openai".to_string(),
        model: "gpt-4".to_string(),
        request: LLMRequest {
            messages: vec![],
            temperature: None,
            max_tokens: None,
            frequency_penalty: None,
            presence_penalty: None,
            top_p: None,
            top_k: None,
            seed: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            raw_body: None,
        },
        response: LLMResponse {
            messages: vec![],
            streamed: false,
            raw_body: None,
        },
        token_usage: None,
        error: None,
        pid: 1234,
        process_name: "test".to_string(),
        agent_name: Some("test-agent".to_string()),
        metadata: HashMap::new(),
    }
}
