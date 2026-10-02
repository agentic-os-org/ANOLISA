//! Token Parser - Extract token usage from SSE events
//!
//! This module provides functionality to extract LLM token usage information
//! from SSE (Server-Sent Events) streaming responses.
//!
//! Supported providers:
//! - OpenAI (GPT-4, GPT-3.5, etc.)
//! - Anthropic (Claude)
//! - Gemini
//!
//! # Example
//! ```rust,ignore
//! use agentsight::analyzer::token::{TokenParser, TokenUsage};
//! use agentsight::parser::ParsedSseEvent;
//!
//! let parser = TokenParser::new();
//! for event in sse_events {
//!     if let Some(usage) = parser.parse_event(&event) {
//!         println!("Tokens: {} in, {} out", usage.input_tokens, usage.output_tokens);
//!     }
//! }
//! ```

use super::{LLMProvider, TokenUsage, detect_provider_from_usage, extract_usage_object};
use crate::parser::sse::ParsedSseEvent;

/// Token parser for extracting usage from SSE events
pub struct TokenParser;

impl TokenParser {
    /// Create a new token parser
    pub fn new() -> Self {
        TokenParser
    }

    /// Parse token usage from a ParsedSseEvent
    ///
    /// Returns `Some(TokenUsage)` if the event contains usage information,
    /// `None` otherwise.
    pub fn parse_event(&self, event: &ParsedSseEvent) -> Option<TokenUsage> {
        // Get event data as string
        let data = event.data();
        let data_str = std::str::from_utf8(data).ok()?;

        // Parse as JSON
        self.parse_data(data_str)
    }

    /// Parse token usage from raw SSE data string
    ///
    /// This is useful when you have raw SSE data without a ParsedSseEvent.
    pub fn parse_data(&self, data: &str) -> Option<TokenUsage> {
        // Skip done markers
        if data.trim() == "[DONE]" || data.trim() == "[END]" {
            return None;
        }

        // Try strict JSON parse first
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
            return self.parse_json(&json).inspect(|_usage| {
                log::debug!("token usage parsed from data: {data}");
            });
        }

        // Fallback: OpenAI Responses API embeds usage in a final
        // `response.completed` event whose payload (instructions + tools +
        // output) routinely exceeds a single TLS record. We may be looking
        // at a concatenation of SSE chunks that together don't form a
        // single valid JSON object. Recover input/output token counts via
        // a regex-free string scan when the buffer references usage fields.
        if data.contains("\"input_tokens\"")
            || data.contains("\"output_tokens\"")
            || data.contains("\"prompt_tokens\"")
            || data.contains("\"completion_tokens\"")
        {
            let usage = Self::scan_partial_usage(data);
            if usage.is_some() {
                log::debug!("token usage recovered from continuation buffer");
            }
            return usage;
        }

        None
    }

    /// Recover token usage fields from a possibly-truncated JSON string by
    /// scanning for the integer values that follow `"input_tokens"`,
    /// `"output_tokens"`, etc. The first occurrence wins, which matches
    /// dashscope's behaviour of placing the canonical `usage` block before
    /// any `x_details` echo.
    fn scan_partial_usage(data: &str) -> Option<TokenUsage> {
        fn find_u64(s: &str, key: &str) -> Option<u64> {
            let pat = format!("\"{key}\"");
            let mut idx = s.find(&pat)?;
            idx += pat.len();
            let rest = s.get(idx..)?;
            let rest = rest.trim_start();
            let rest = rest.strip_prefix(':')?.trim_start();
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            if end == 0 {
                return None;
            }
            rest[..end].parse::<u64>().ok()
        }

        let input = find_u64(data, "input_tokens").or_else(|| find_u64(data, "prompt_tokens"));
        let output =
            find_u64(data, "output_tokens").or_else(|| find_u64(data, "completion_tokens"));
        if input.is_none() && output.is_none() {
            return None;
        }

        Some(TokenUsage {
            input_tokens: input.unwrap_or(0),
            output_tokens: output.unwrap_or(0),
            cache_creation_input_tokens: find_u64(data, "cache_creation_input_tokens"),
            cache_read_input_tokens: find_u64(data, "cache_read_input_tokens")
                .or_else(|| find_u64(data, "cached_tokens")),
            model: None,
            provider: LLMProvider::OpenAI,
        })
    }

    /// Internal method to parse JSON and extract token usage
    pub fn parse_json(&self, json: &serde_json::Value) -> Option<TokenUsage> {
        // 1. Check for message_start event (Anthropic streaming)
        if json.get("type").and_then(|v| v.as_str()) == Some("message_start") {
            if let Some(message) = json.get("message") {
                if let Some(usage) = message.get("usage") {
                    return extract_usage_object(usage, LLMProvider::Anthropic, json);
                }
            }
        }

        // 2. Check for message_delta event (Anthropic streaming final)
        if json.get("type").and_then(|v| v.as_str()) == Some("message_delta") {
            if let Some(usage) = json.get("usage") {
                return extract_usage_object(usage, LLMProvider::Anthropic, json);
            }
        }

        // 3. DashScope/Bailian native protocol envelope.
        //
        // Must precede the generic `usage` branch: the native `usage` field
        // names are identical to Anthropic's, and real Anthropic-gateway
        // traffic even ships the same `total_tokens` sibling, so usage shape
        // alone cannot tell them apart. The envelope can: an `output` *object*
        // paired with `usage` is unique to the native protocol (the OpenAI
        // Responses API nests `output` as an array under `response`).
        if json.get("output").is_some_and(|o| o.is_object()) {
            if let Some(usage) = json.get("usage") {
                return extract_usage_object(usage, LLMProvider::DashScope, json);
            }
        }

        // 4. Check for usage object directly (OpenAI and compatible APIs)
        if let Some(usage) = json.get("usage") {
            let provider = detect_provider_from_usage(usage);
            return extract_usage_object(usage, provider, json);
        }

        // 5. Responses API: usage nested in response.completed event
        if json.get("type").and_then(|v| v.as_str()) == Some("response.completed") {
            if let Some(resp) = json.get("response") {
                if let Some(usage) = resp.get("usage") {
                    let provider = detect_provider_from_usage(usage);
                    return extract_usage_object(usage, provider, json);
                }
            }
        }

        None
    }
}

impl Default for TokenParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a mock ParsedSseEvent for testing
    ///
    /// Note: SslEvent is large (~512KB), so we allocate it on the heap
    fn create_test_event(data: &str) -> ParsedSseEvent {
        use crate::probes::sslsniff::SslEvent;
        use std::rc::Rc;

        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1234567890,
            delta_ns: 0,
            pid: 1234,
            tid: 5678,
            uid: 0,
            len: data.len() as u32,
            rw: 0,
            comm: String::new(),
            buf: data.as_bytes().to_vec(),
            is_handshake: false,
            ssl_ptr: 0,
        });

        ParsedSseEvent::new(None, None, None, 0, data.len(), ssl_event)
    }

    /// DashScope/Bailian native protocol is identified by its **response
    /// envelope** (`output` object + `request_id`, no top-level `model`), not by
    /// the shape of `usage` — the native `usage` field names are
    /// indistinguishable from Anthropic-gateway traffic that also reports
    /// `total_tokens`.
    /// Envelope shape taken verbatim from a real non-streaming native response.
    #[test]
    fn test_parse_dashscope_native_envelope() {
        let parser = TokenParser::new();
        let data = r#"{
            "output": {"choices": [{
                "message": {"content": "收到了", "role": "assistant"},
                "finish_reason": "stop"
            }]},
            "usage": {"total_tokens": 26, "output_tokens": 1, "input_tokens": 25,
                      "prompt_tokens_details": {"cached_tokens": 0}},
            "request_id": "b82b259c-ed1d-95bd-8c0e-4f357868a622"
        }"#;
        let usage = parser
            .parse_data(data)
            .expect("native envelope must yield usage");
        assert_eq!(usage.provider, LLMProvider::DashScope);
        assert_eq!(usage.input_tokens, 25);
        assert_eq!(usage.output_tokens, 1);
    }

    /// `result_format: text` puts the answer on `output.text`.
    #[test]
    fn test_parse_dashscope_native_envelope_text_format() {
        let parser = TokenParser::new();
        let data = r#"{
            "output": {"finish_reason": "stop", "text": "hi"},
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
            "request_id": "req-2"
        }"#;
        let usage = parser.parse_data(data).expect("usage");
        assert_eq!(usage.provider, LLMProvider::DashScope);
    }

    /// multimodal-generation itemises the input side; `image_tokens` is already
    /// counted inside `input_tokens` (real values: 1249 image + 12 text = 1261).
    #[test]
    fn test_parse_dashscope_native_envelope_multimodal_input_tokens() {
        let parser = TokenParser::new();
        let data = r#"{
            "output": {"choices": [{"finish_reason": "stop",
                "message": {"role": "assistant", "content": [{"text": "cat"}],
                            "reasoning_content": ""}}]},
            "usage": {
                "input_tokens_details": {"image_tokens": 1249, "text_tokens": 12},
                "prompt_tokens_details": {"cached_tokens": 0},
                "total_tokens": 1643,
                "output_tokens": 382,
                "input_tokens": 1261,
                "image_tokens": 1249
            },
            "request_id": "req-3"
        }"#;
        let usage = parser.parse_data(data).expect("usage");
        assert_eq!(usage.provider, LLMProvider::DashScope);
        assert_eq!(
            usage.input_tokens, 1261,
            "no double counting of image tokens"
        );
        assert_eq!(usage.output_tokens, 382);
        assert_eq!(usage.total_tokens(), 1643, "reconciles with total_tokens");
    }

    /// Regression guard from **real captured traffic**: Anthropic-protocol
    /// gateway responses carry `total_tokens` in `usage` but have no `output`
    /// envelope, so they must stay Anthropic.
    #[test]
    fn test_parse_anthropic_gateway_usage_with_total_tokens_stays_anthropic() {
        let parser = TokenParser::new();
        let data = r#"{
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {
                "input_tokens": 1111,
                "output_tokens": 803,
                "total_tokens": 26490,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 24576
            }
        }"#;
        let usage = parser.parse_data(data).expect("usage");
        assert_eq!(usage.provider, LLMProvider::Anthropic);
        assert_eq!(usage.input_tokens, 1111);
        assert_eq!(usage.cache_read_input_tokens, Some(24576));
    }

    /// Same shape without the Anthropic `type` marker (non-streaming gateway
    /// response) must also stay Anthropic rather than flipping to DashScope.
    #[test]
    fn test_parse_bare_anthropic_usage_with_total_tokens_stays_anthropic() {
        let parser = TokenParser::new();
        let data = r#"{
            "id": "msg_01",
            "model": "claude-opus-5",
            "usage": {
                "input_tokens": 977,
                "output_tokens": 169,
                "total_tokens": 26746,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 25600
            }
        }"#;
        let usage = parser.parse_data(data).expect("usage");
        assert_eq!(usage.provider, LLMProvider::Anthropic);
        assert_eq!(usage.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn test_parse_openai_usage() {
        let parser = TokenParser::new();
        let data = r#"{
            "id": "chatcmpl-123",
            "model": "gpt-4",
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        }"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event);
        assert!(usage.is_some());

        let usage = usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.total_tokens(), 150);
        assert_eq!(usage.provider, LLMProvider::OpenAI);
        assert_eq!(usage.model, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_parse_anthropic_message_start() {
        let parser = TokenParser::new();
        let data = r#"{
            "type": "message_start",
            "message": {
                "id": "msg_123",
                "model": "claude-3-opus",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 0
                }
            }
        }"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event);
        assert!(usage.is_some());

        let usage = usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.provider, LLMProvider::Anthropic);
    }

    #[test]
    fn test_parse_anthropic_message_delta() {
        let parser = TokenParser::new();
        let data = r#"{
            "type": "message_delta",
            "delta": {},
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 10,
                "cache_read_input_tokens": 20
            }
        }"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event);
        assert!(usage.is_some());

        let usage = usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, Some(10));
        assert_eq!(usage.cache_read_input_tokens, Some(20));
        assert_eq!(usage.provider, LLMProvider::Anthropic);
    }

    #[test]
    fn test_parse_anthropic_message_delta_output_only() {
        // Official Anthropic message_delta carries only output_tokens; the
        // parser must still surface it (input defaults to 0) so the caller can
        // merge it with message_start rather than dropping the event.
        let parser = TokenParser::new();
        let data = r#"{
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 42}
        }"#;

        let event = create_test_event(data);
        let usage = parser
            .parse_event(&event)
            .expect("output-only should parse");
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.provider, LLMProvider::Anthropic);
    }

    #[test]
    fn test_parse_openai_sse_streaming() {
        let parser = TokenParser::new();
        let data = r#"{"choices":[],"object":"chat.completion.chunk","usage":{"prompt_tokens":61744,"completion_tokens":61,"total_tokens":61805},"created":1773640825,"model":"qwen3.5-plus","id":"chatcmpl-816f7538-0ac9-98c4-8259-9bade0c2cde7"}"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event);
        assert!(
            usage.is_some(),
            "Should extract usage from SSE streaming data"
        );

        let usage = usage.unwrap();
        assert_eq!(usage.input_tokens, 61744);
        assert_eq!(usage.output_tokens, 61);
        assert_eq!(usage.total_tokens(), 61805);
        assert_eq!(usage.provider, LLMProvider::OpenAI);
        assert_eq!(usage.model, Some("qwen3.5-plus".to_string()));
    }

    #[test]
    fn test_parse_openai_usage_with_cached_tokens() {
        // OpenAI nests cache hits under prompt_tokens_details.cached_tokens.
        let parser = TokenParser::new();
        let data = r#"{
            "id": "chatcmpl-456",
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 10000,
                "completion_tokens": 50,
                "total_tokens": 10050,
                "prompt_tokens_details": {
                    "cached_tokens": 8500
                }
            }
        }"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event).expect("should parse");
        assert_eq!(usage.input_tokens, 10000);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_input_tokens, Some(8500));
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.provider, LLMProvider::OpenAI);
    }

    #[test]
    fn test_parse_dashscope_usage_with_cached_tokens() {
        // DashScope may surface cached_tokens at the top level of the usage
        // object instead of nesting under prompt_tokens_details.
        let parser = TokenParser::new();
        let data = r#"{
            "id": "chatcmpl-789",
            "model": "qwen3.5-plus",
            "usage": {
                "prompt_tokens": 9800,
                "completion_tokens": 120,
                "total_tokens": 9920,
                "cached_tokens": 9000
            }
        }"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event).expect("should parse");
        assert_eq!(usage.input_tokens, 9800);
        assert_eq!(usage.output_tokens, 120);
        assert_eq!(usage.cache_read_input_tokens, Some(9000));
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.provider, LLMProvider::OpenAI);
    }

    #[test]
    fn test_parse_dashscope_prompt_tokens_details_cache_creation() {
        // DashScope nests cache_creation_input_tokens under prompt_tokens_details
        // rather than at the top level of the usage object.
        let parser = TokenParser::new();
        let data = r#"{
            "id": "chatcmpl-ds-001",
            "model": "qwen3.6-plus",
            "usage": {
                "prompt_tokens": 29719,
                "completion_tokens": 435,
                "total_tokens": 30154,
                "completion_tokens_details": {
                    "reasoning_tokens": 382,
                    "text_tokens": 435
                },
                "prompt_tokens_details": {
                    "text_tokens": 29719,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 29713
                    },
                    "cache_creation_input_tokens": 29713,
                    "cache_type": "ephemeral",
                    "cached_tokens": 0
                }
            }
        }"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event).expect("should parse");
        assert_eq!(usage.input_tokens, 29719);
        assert_eq!(usage.output_tokens, 435);
        assert_eq!(usage.cache_creation_input_tokens, Some(29713));
        assert_eq!(usage.cache_read_input_tokens, Some(0));
    }

    #[test]
    fn test_parse_dashscope_usage_without_prompt_tokens_details() {
        // DashScope response lacking prompt_tokens_details — cache fields
        // must remain None, preserving pre-fix behavior.
        let parser = TokenParser::new();
        let data = r#"{
            "id": "chatcmpl-ds-002",
            "model": "qwen3.6-plus",
            "usage": {
                "prompt_tokens": 500,
                "completion_tokens": 50,
                "total_tokens": 550
            }
        }"#;

        let event = create_test_event(data);
        let usage = parser.parse_event(&event).expect("should parse");
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.cache_read_input_tokens, None);
    }

    #[test]
    fn test_skip_done_marker() {
        let parser = TokenParser::new();

        let event = create_test_event("[DONE]");
        assert!(parser.parse_event(&event).is_none());

        let event = create_test_event("[END]");
        assert!(parser.parse_event(&event).is_none());
    }

    #[test]
    fn test_parse_no_usage() {
        let parser = TokenParser::new();
        let data = r#"{"choices":[{"delta":{"content":"Hello"}}]}"#;

        let event = create_test_event(data);
        assert!(parser.parse_event(&event).is_none());
    }

    #[test]
    fn test_parse_data_directly() {
        let parser = TokenParser::new();
        let data = r#"{"usage":{"prompt_tokens":10,"completion_tokens":5},"model":"gpt-3.5"}"#;

        let usage = parser.parse_data(data);
        assert!(usage.is_some());

        let usage = usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
    }

    #[test]
    fn test_scan_partial_usage_dashscope_response_completed() {
        // Real dashscope `/compatible-mode/v1/responses` `response.completed`
        // payload, captured with curl. The strict JSON path should succeed
        // for this complete buffer, so this just confirms canonical fields.
        let data = r#"{"sequence_number":10,"type":"response.completed","response":{"top_logprobs":0,"instructions":"You are a helpful assistant.","metadata":{},"usage":{"total_tokens":60,"input_tokens_details":{"cached_tokens":0},"output_tokens":3,"input_tokens":57,"output_tokens_details":{"reasoning_tokens":0},"x_details":[{"total_tokens":60,"x_billing_type":"response_api","output_tokens":3,"input_tokens":57,"prompt_tokens_details":{"cached_tokens":0}}]},"created_at":1782287513,"model":"qwen3-coder-plus"}}"#;
        let parser = TokenParser::new();
        let usage = parser.parse_data(data).expect("usage should parse");
        assert_eq!(usage.input_tokens, 57);
        assert_eq!(usage.output_tokens, 3);
    }

    #[test]
    fn test_scan_partial_usage_truncated_buffer() {
        // Simulate a continuation buffer where only the leading bytes around
        // the `usage` block survived; trailing braces / brackets are missing
        // and strict JSON parsing fails. The fallback path should still
        // recover the integer values.
        let data = r#"event:response.completed
data:{"sequence_number":10,"type":"response.completed","response":{"usage":{"total_tokens":60,"input_tokens_details":{"cached_tokens":2},"output_tokens":3,"input_tokens":57"#;
        let parser = TokenParser::new();
        let usage = parser
            .parse_data(data)
            .expect("partial usage should still parse");
        assert_eq!(usage.input_tokens, 57);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cache_read_input_tokens, Some(2));
    }

    #[test]
    fn test_scan_partial_usage_returns_none_when_no_tokens() {
        let data = "event:response.output_text.delta\ndata:{\"delta\":\"hi\"}";
        let parser = TokenParser::new();
        assert!(parser.parse_data(data).is_none());
    }

    #[test]
    fn test_scan_partial_usage_only_input() {
        // Truncated JSON forces the regex-free string-scan fallback.
        let data = r#"{"input_tokens": 42"#;
        let parser = TokenParser::new();
        let usage = parser.parse_data(data).expect("should parse input only");
        assert_eq!(usage.input_tokens, 42);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_scan_partial_usage_only_output() {
        let data = r#"{"output_tokens": 7"#;
        let parser = TokenParser::new();
        let usage = parser.parse_data(data).expect("should parse output only");
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 7);
    }

    #[test]
    fn test_scan_partial_usage_prompt_completion_aliases() {
        let data = r#"{"prompt_tokens": 10, "completion_tokens": 5"#;
        let parser = TokenParser::new();
        let usage = parser.parse_data(data).expect("should parse aliases");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
    }

    #[test]
    fn test_scan_partial_usage_invalid_value_none() {
        let data = r#"{"input_tokens": "not a number"}"#;
        let parser = TokenParser::new();
        assert!(parser.parse_data(data).is_none());
    }
}
