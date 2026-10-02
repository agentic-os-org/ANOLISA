//! LLM API Message Format Parser
//!
//! This module provides structured parsing for LLM API request and response bodies.
//!
//! # Supported APIs
//!
//! ## OpenAI Chat Completions
//! - Endpoint: `/v1/chat/completions`, `/v1/completions`
//! - Request: model, messages, temperature, max_tokens, stream, etc.
//! - Response: id, choices, usage, model, etc.
//!
//! ## Anthropic Messages
//! - Endpoint: `/v1/messages`
//! - Request: model, messages, max_tokens, system, stream, etc.
//! - Response: id, content, usage, model, stop_reason, etc.
//!
//! # Example
//!
//! ```rust,ignore
//! use agentsight::analyzer::message::{MessageParser, ParsedApiMessage};
//!
//! let parser = MessageParser::new();
//!
//! // Parse based on API path
//! let path = "/v1/chat/completions";
//! let request_body: serde_json::Value = serde_json::from_str(request_json)?;
//! let response_body: serde_json::Value = serde_json::from_str(response_json)?;
//!
//! if let Some(parsed) = parser.parse_by_path(path, Some(&request_body), Some(&response_body)) {
//!     match parsed {
//!         ParsedApiMessage::OpenAICompletion { request, response } => {
//!             println!("OpenAI completion parsed");
//!         }
//!         ParsedApiMessage::AnthropicMessage { request, response } => {
//!             println!("Anthropic message parsed");
//!         }
//!     }
//! }
//! ```

mod anthropic;
mod openai;
pub mod sysom;
pub mod types;

pub use anthropic::AnthropicParser;
pub use openai::OpenAIParser;
pub use sysom::SysomParser;
pub use types::*;

use crate::parser::sse::ParsedSseEvent;

/// Unified message format parser for multiple LLM providers
///
/// Provides a single entry point for parsing both OpenAI and Anthropic
/// API request/response bodies based on the API endpoint path.
pub struct MessageParser;

impl MessageParser {
    /// Create a new message parser
    pub fn new() -> Self {
        MessageParser
    }

    /// Parse request/response bodies based on API path
    ///
    /// Automatically detects the LLM provider based on the request path:
    /// - Paths containing `/v1/messages` → Anthropic
    /// - Paths containing `/v1/chat/completions` or `/v1/completions` → OpenAI
    /// - Paths containing `/api/v1/copilot/generate_copilot` → Aliyun SysOM
    /// # Arguments
    /// * `path` - The HTTP request path (e.g., "/v1/chat/completions")
    /// * `request_body` - Optional JSON body from the HTTP request
    /// * `response_body` - Optional JSON body from the HTTP response
    ///
    /// # Returns
    /// * `Some(ParsedApiMessage)` if the path matches a known provider and parsing succeeds
    /// * `None` if the path doesn't match or both bodies fail to parse
    ///
    /// # Example
    /// ```rust,ignore
    /// let parser = MessageParser::new();
    /// let parsed = parser.parse_by_path(
    ///     "/v1/chat/completions",
    ///     Some(&request_json),
    ///     Some(&response_json),
    /// );
    /// ```
    pub fn parse_by_path(
        &self,
        path: &str,
        request_body: Option<&serde_json::Value>,
        response_body: Option<&serde_json::Value>,
    ) -> Option<ParsedApiMessage> {
        // Try Anthropic first (more specific path)
        if AnthropicParser::matches_path(path) {
            let request = request_body.and_then(AnthropicParser::parse_request);
            let response = response_body.and_then(AnthropicParser::parse_response);

            // Return Some only if at least one was parsed
            if request.is_some() || response.is_some() {
                return Some(ParsedApiMessage::AnthropicMessage { request, response });
            }
        }

        // Try OpenAI
        if OpenAIParser::matches_path(path) {
            let request = request_body.and_then(OpenAIParser::parse_request);
            let response = response_body.and_then(OpenAIParser::parse_response);

            // Return Some only if at least one was parsed
            if request.is_some() || response.is_some() {
                return Some(ParsedApiMessage::OpenAICompletion { request, response });
            }
        }

        // Try Aliyun SysOM (AK/SK auth mode)
        if let Some(parsed) = self.parse_by_path_sysom_only(path, request_body, response_body) {
            return Some(parsed);
        }

        // The analyzer pre-filters non-LLM paths before calling in, so
        // reaching this point is unexpected but harmless — keep it at debug
        // as a last-resort diagnostic rather than a per-request alert.
        log::debug!("Path '{path}' does not match any known LLM API endpoint");
        None
    }

    /// Parse request/response bodies against the SysOM parser only.
    ///
    /// SysOM's Copilot API is deliberately kept as the *only* deep-parsing
    /// target for SSE-shaped responses: its body is a non-standard envelope
    /// (`llmParamString`-encoded request, cumulative SSE chunks, `tool_use`
    /// array) that the generic HttpRecord/genai-builder SSE fallback cannot
    /// reconstruct. OpenAI/Anthropic SSE responses are intentionally left
    /// unparsed here because `genai::builder::extract_parts_from_sse_body`
    /// already rebuilds their semantic content from the raw HttpRecord —
    /// parsing them again here would just duplicate that work.
    pub fn parse_by_path_sysom_only(
        &self,
        path: &str,
        request_body: Option<&serde_json::Value>,
        response_body: Option<&serde_json::Value>,
    ) -> Option<ParsedApiMessage> {
        if !SysomParser::matches_path(path) {
            return None;
        }
        let request = request_body.and_then(SysomParser::parse_request);
        let response = response_body.and_then(SysomParser::parse_response);

        if request.is_some() || response.is_some() {
            return Some(ParsedApiMessage::SysomMessage { request, response });
        }
        None
    }

    /// Parse request body and SSE events based on API path
    ///
    /// This method handles streaming responses where the response is delivered
    /// via Server-Sent Events (SSE) instead of a single JSON body.
    /// SSE events are converted to a JSON array and passed to parse_response.
    ///
    /// Delegates to [`Self::parse_by_path`] so that Anthropic/OpenAI/SysOM
    /// SSE responses are all deep-parsed by their typed parsers. The
    /// genai-builder SSE fallback alone cannot reconstruct Anthropic
    /// content blocks from `message_start`/`content_block_delta` events.
    ///
    /// # Arguments
    /// * `path` - The HTTP request path (e.g., "/v1/chat/completions")
    /// * `request_body` - Optional JSON body from the HTTP request
    /// * `sse_events` - Slice of SSE events from the streaming response
    ///
    /// # Returns
    /// * `Some(ParsedApiMessage)` if parsing succeeds
    /// * `None` if the path doesn't match or parsing fails
    pub fn parse_by_path_with_sse(
        &self,
        path: &str,
        request_body: Option<&serde_json::Value>,
        sse_events: &[ParsedSseEvent],
    ) -> Option<ParsedApiMessage> {
        // Convert SSE events to JSON array
        let chunks: Vec<serde_json::Value> = sse_events
            .iter()
            .filter_map(|e| {
                let data = String::from_utf8_lossy(e.data());
                serde_json::from_str(&data).ok()
            })
            .collect();

        let response_body = if chunks.is_empty() {
            None
        } else {
            Some(serde_json::Value::Array(chunks))
        };

        self.parse_by_path(path, request_body, response_body.as_ref())
    }

    /// Detect provider from path without parsing
    ///
    /// # Arguments
    /// * `path` - The HTTP request path
    ///
    /// # Returns
    /// * `Some("anthropic")` for Anthropic paths
    /// * `Some("openai")` for OpenAI paths
    /// * `None` for unknown paths
    pub fn detect_provider(path: &str) -> Option<&'static str> {
        if AnthropicParser::matches_path(path) {
            Some("anthropic")
        } else if OpenAIParser::matches_path(path) {
            Some("openai")
        } else if SysomParser::matches_path(path) {
            Some("sysom")
        } else {
            None
        }
    }

    /// Check if a path matches any known LLM API endpoint
    pub fn is_llm_api_path(path: &str) -> bool {
        AnthropicParser::matches_path(path)
            || OpenAIParser::matches_path(path)
            || SysomParser::matches_path(path)
    }
}

impl Default for MessageParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_openai_by_path() {
        let parser = MessageParser::new();

        let request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let response = serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hi there!"},
                "finish_reason": "stop"
            }]
        });

        let result = parser.parse_by_path("/v1/chat/completions", Some(&request), Some(&response));
        assert!(result.is_some());

        match result.unwrap() {
            ParsedApiMessage::OpenAICompletion {
                request: req,
                response: resp,
            } => {
                assert!(req.is_some());
                assert!(resp.is_some());
                assert_eq!(req.unwrap().model, "gpt-4");
                assert_eq!(resp.unwrap().id, "chatcmpl-123");
            }
            _ => panic!("Expected OpenAICompletion"),
        }
    }

    #[test]
    fn test_parse_anthropic_by_path() {
        let parser = MessageParser::new();

        let request = serde_json::json!({
            "model": "claude-3-opus-20240229",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let response = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hi there!"}],
            "model": "claude-3-opus-20240229",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let result = parser.parse_by_path("/v1/messages", Some(&request), Some(&response));
        assert!(result.is_some());

        match result.unwrap() {
            ParsedApiMessage::AnthropicMessage {
                request: req,
                response: resp,
            } => {
                assert!(req.is_some());
                assert!(resp.is_some());
                assert_eq!(req.unwrap().model, "claude-3-opus-20240229");
                assert_eq!(resp.unwrap().id, "msg_123");
            }
            _ => panic!("Expected AnthropicMessage"),
        }
    }

    #[test]
    fn test_parse_with_only_request() {
        let parser = MessageParser::new();

        let request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let result = parser.parse_by_path("/v1/chat/completions", Some(&request), None);
        assert!(result.is_some());

        match result.unwrap() {
            ParsedApiMessage::OpenAICompletion {
                request: req,
                response: resp,
            } => {
                assert!(req.is_some());
                assert!(resp.is_none());
            }
            _ => panic!("Expected OpenAICompletion"),
        }
    }

    #[test]
    fn test_parse_with_only_response() {
        let parser = MessageParser::new();

        let response = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello!"}],
            "model": "claude-3-opus-20240229",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let result = parser.parse_by_path("/v1/messages", None, Some(&response));
        assert!(result.is_some());

        match result.unwrap() {
            ParsedApiMessage::AnthropicMessage {
                request: req,
                response: resp,
            } => {
                assert!(req.is_none());
                assert!(resp.is_some());
            }
            _ => panic!("Expected AnthropicMessage"),
        }
    }

    #[test]
    fn test_unknown_path_returns_none() {
        let parser = MessageParser::new();

        let body = serde_json::json!({"model": "gpt-4"});

        let result = parser.parse_by_path("/v1/embeddings", Some(&body), None);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_provider() {
        assert_eq!(
            MessageParser::detect_provider("/v1/messages"),
            Some("anthropic")
        );
        assert_eq!(
            MessageParser::detect_provider("/v1/chat/completions"),
            Some("openai")
        );
        assert_eq!(
            MessageParser::detect_provider("/v1/completions"),
            Some("openai")
        );
        assert_eq!(MessageParser::detect_provider("/v1/embeddings"), None);
    }

    #[test]
    fn test_is_llm_api_path() {
        assert!(MessageParser::is_llm_api_path("/v1/messages"));
        assert!(MessageParser::is_llm_api_path("/v1/chat/completions"));
        assert!(MessageParser::is_llm_api_path("/v1/completions"));
        assert!(!MessageParser::is_llm_api_path("/v1/embeddings"));
        assert!(!MessageParser::is_llm_api_path("/health"));
    }

    #[test]
    fn test_parsed_api_message_methods() {
        let openai_msg = ParsedApiMessage::OpenAICompletion {
            request: Some(OpenAIRequest {
                model: "gpt-4".to_string(),
                messages: vec![],
                temperature: None,
                max_tokens: None,
                stream: Some(true),
                top_p: None,
                n: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                user: None,
                tools: None,
                tool_choice: None,
                response_format: None,
                seed: None,
                logprobs: None,
                top_logprobs: None,
                parallel_tool_calls: None,
            }),
            response: None,
        };

        assert_eq!(openai_msg.provider(), "openai");
        assert_eq!(openai_msg.model(), Some("gpt-4"));
        assert_eq!(openai_msg.is_streaming(), Some(true));
    }

    #[test]
    fn test_full_url_paths() {
        let _parser = MessageParser::new();

        // Should work with full URLs too
        assert!(MessageParser::is_llm_api_path(
            "https://api.openai.com/v1/chat/completions"
        ));
        assert!(MessageParser::is_llm_api_path(
            "https://api.anthropic.com/v1/messages"
        ));
    }

    // -- parse_by_path_sysom_only / parse_by_path_with_sse scoping tests --
    //
    // parse_by_path_with_sse delegates to the full parse_by_path, so all
    // providers (OpenAI/Anthropic/SysOM) are deep-parsed for SSE responses.
    // The genai-builder SSE fallback alone cannot reconstruct Anthropic
    // content blocks from message_start/content_block_delta events.

    #[test]
    fn test_parse_by_path_sysom_only_rejects_openai_and_anthropic() {
        let parser = MessageParser::new();
        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let anthropic_request = serde_json::json!({
            "model": "claude-3-opus-20240229",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        });

        assert!(
            parser
                .parse_by_path_sysom_only("/v1/chat/completions", Some(&openai_request), None)
                .is_none()
        );
        assert!(
            parser
                .parse_by_path_sysom_only("/v1/messages", Some(&anthropic_request), None)
                .is_none()
        );
    }

    #[test]
    fn test_parse_by_path_sysom_only_accepts_sysom() {
        let parser = MessageParser::new();
        let params = serde_json::json!({
            "model": "qwen3-coder-plus",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let request = serde_json::json!({"llmParamString": params.to_string()});

        let result = parser.parse_by_path_sysom_only(
            "/api/v1/copilot/generate_copilot",
            Some(&request),
            None,
        );
        match result {
            Some(ParsedApiMessage::SysomMessage {
                request: Some(req), ..
            }) => {
                assert_eq!(req.params.model, "qwen3-coder-plus");
            }
            other => panic!("expected SysomMessage with request, got {other:?}"),
        }
    }

    /// Build a `ParsedSseEvent` carrying the given raw SSE `data:` payload (no
    /// framing needed — `parse_by_path_with_sse` reads the event body as JSON).
    fn make_sse_event(data: &str) -> ParsedSseEvent {
        use crate::probes::sslsniff::SslEvent;
        use std::rc::Rc;

        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1,
            tid: 1,
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

    #[test]
    fn test_parse_by_path_with_sse_openai_returns_none() {
        // The OpenAI parser does not deep-parse SSE arrays — the genai
        // builder's SSE fallback handles OpenAI streaming instead.
        let parser = MessageParser::new();
        let chunk = serde_json::json!({
            "id": "chatcmpl-123",
            "model": "gpt-4o",
            "choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]
        });
        let events = vec![make_sse_event(&chunk.to_string())];

        let result = parser.parse_by_path_with_sse("/v1/chat/completions", None, &events);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_by_path_with_sse_parses_anthropic() {
        // The Anthropic parser deep-parses SSE arrays (message_start,
        // content_block_delta, etc.) — this must not be skipped.
        let parser = MessageParser::new();
        let message_start = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_123",
                "model": "claude-3-opus",
                "role": "assistant",
                "type": "message",
                "content": [],
                "usage": {"input_tokens": 10, "output_tokens": 0}
            }
        });
        let content_delta = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello"}
        });
        let message_delta = serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 5}
        });
        let events = vec![
            make_sse_event(&message_start.to_string()),
            make_sse_event(&content_delta.to_string()),
            make_sse_event(&message_delta.to_string()),
        ];

        let result = parser.parse_by_path_with_sse("/v1/messages", None, &events);
        assert!(result.is_some(), "Anthropic SSE should be deep-parsed");
    }

    #[test]
    fn test_parse_by_path_with_sse_still_parses_sysom() {
        let parser = MessageParser::new();
        let chunk = serde_json::json!({
            "choices": [{"message": {"content": "hi", "tool_use": null}}]
        });
        let events = vec![make_sse_event(&chunk.to_string())];

        let result =
            parser.parse_by_path_with_sse("/api/v1/copilot/generate_copilot", None, &events);
        match result {
            Some(ParsedApiMessage::SysomMessage {
                response: Some(resp),
                ..
            }) => {
                assert_eq!(resp.choices[0].message.content, "hi");
            }
            other => panic!("expected SysomMessage with response, got {other:?}"),
        }
    }
}
