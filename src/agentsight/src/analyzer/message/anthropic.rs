//! Anthropic Messages API parser
//!
//! This module provides parsing functionality for Anthropic Messages API
//! request and response bodies.
//!
//! # Supported Endpoints
//! - `/v1/messages`
//!
//! # Example
//! ```rust,ignore
//! use agentsight::analyzer::message::{AnthropicParser, AnthropicRequest, AnthropicResponse};
//!
//! let parser = AnthropicParser;
//!
//! // Parse request body
//! let request_json: serde_json::Value = serde_json::from_str(request_body)?;
//! if let Some(request) = parser.parse_request(&request_json) {
//!     println!("Model: {}", request.model);
//! }
//!
//! // Parse response body
//! let response_json: serde_json::Value = serde_json::from_str(response_body)?;
//! if let Some(response) = parser.parse_response(&response_json) {
//!     println!("Message ID: {}", response.id);
//! }
//! ```

use super::types::{
    AnthropicContentBlock, AnthropicRequest, AnthropicResponse, AnthropicSseEvent, AnthropicUsage,
    MessageRole,
};

/// Max of two optional counters, preserving a value when only one is set.
fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Parser for Anthropic Messages API
///
/// Provides methods to parse JSON request and response bodies
/// from Anthropic-compatible APIs.
pub struct AnthropicParser;

impl AnthropicParser {
    /// Parse an Anthropic Messages request body from JSON
    ///
    /// # Arguments
    /// * `body` - The JSON value representing the request body
    ///
    /// # Returns
    /// * `Some(AnthropicRequest)` if parsing succeeds
    /// * `None` if the JSON doesn't match the expected format
    ///
    /// # Example
    /// ```rust,ignore
    /// let json = serde_json::json!({
    ///     "model": "claude-3-opus-20240229",
    ///     "max_tokens": 1024,
    ///     "messages": [{"role": "user", "content": "Hello"}]
    /// });
    /// let request = AnthropicParser::parse_request(&json);
    /// ```
    pub fn parse_request(body: &serde_json::Value) -> Option<AnthropicRequest> {
        // Quick validation - must have model, messages, and max_tokens fields
        if body.get("model").is_none()
            || body.get("messages").is_none()
            || body.get("max_tokens").is_none()
        {
            log::trace!(
                "Anthropic request missing required fields: model, messages, or max_tokens"
            );
            return None;
        }

        match serde_json::from_value::<AnthropicRequest>(body.clone()) {
            Ok(request) => {
                log::debug!(
                    "Parsed Anthropic request: model={}, messages={}, max_tokens={}",
                    request.model,
                    request.messages.len(),
                    request.max_tokens
                );
                Some(request)
            }
            Err(e) => {
                log::trace!("Failed to parse Anthropic request: {e}");
                None
            }
        }
    }

    /// Parse an Anthropic Messages response body from JSON
    ///
    /// # Arguments
    /// * `body` - The JSON value representing the response body
    ///
    /// # Returns
    /// * `Some(AnthropicResponse)` if parsing succeeds
    /// * `None` if the JSON doesn't match the expected format
    ///
    /// # Example
    /// ```rust,ignore
    /// let json = serde_json::json!({
    ///     "id": "msg_123",
    ///     "type": "message",
    ///     "role": "assistant",
    ///     "content": [{"type": "text", "text": "Hello!"}],
    ///     "model": "claude-3-opus-20240229",
    ///     "usage": {"input_tokens": 10, "output_tokens": 5}
    /// });
    /// let response = AnthropicParser::parse_response(&json);
    /// ```
    pub fn parse_response(body: &serde_json::Value) -> Option<AnthropicResponse> {
        // Try standard response format first (has id, type="message", content)
        if body.get("id").is_some()
            && body.get("type").and_then(|v| v.as_str()) == Some("message")
            && body.get("content").is_some()
        {
            match serde_json::from_value::<AnthropicResponse>(body.clone()) {
                Ok(response) => {
                    log::debug!(
                        "Parsed Anthropic response: id={}, model={}, content_blocks={}",
                        response.id,
                        response.model,
                        response.content.len()
                    );
                    return Some(response);
                }
                Err(e) => {
                    log::trace!("Failed to parse Anthropic response: {e}");
                }
            }
        }

        // Try SSE events array format (body is an array of SSE events)
        if let Some(events) = body.as_array() {
            return Self::aggregate_sse_events(events);
        }

        None
    }

    /// Aggregate SSE events into a single AnthropicResponse
    ///
    /// Handles both text and tool_use content blocks from the streaming event sequence:
    /// - `MessageStart` → extract message metadata (id, model, usage)
    /// - `ContentBlockStart` → begin a new text or tool_use block
    /// - `ContentBlockDelta` → append text (TextDelta) or tool args (InputJsonDelta)
    /// - `ContentBlockStop` → finalize and push current block to content list
    /// - `MessageDelta` → extract stop_reason and final usage
    fn aggregate_sse_events(events: &[serde_json::Value]) -> Option<AnthropicResponse> {
        let mut content_blocks: Vec<AnthropicContentBlock> = Vec::new();
        let mut stop_reason: Option<String> = None;
        let mut usage: Option<AnthropicUsage> = None;
        let mut message_start: Option<AnthropicSseEvent> = None;

        // State for the current content block being streamed
        enum CurrentBlock {
            Text {
                text: String,
            },
            Thinking {
                thinking: String,
                signature: String,
            },
            ToolUse {
                id: String,
                name: String,
                input_json: String,
            },
        }
        let mut current_block: Option<CurrentBlock> = None;

        for event_value in events {
            // Try to parse as AnthropicSseEvent
            if let Ok(sse_event) = serde_json::from_value::<AnthropicSseEvent>(event_value.clone())
            {
                match &sse_event {
                    AnthropicSseEvent::MessageStart { message } => {
                        message_start = Some(sse_event.clone());
                        usage = Some(message.usage.clone());
                    }
                    AnthropicSseEvent::ContentBlockStart { content_block, .. } => {
                        // Begin a new content block based on its type
                        current_block = match content_block {
                            AnthropicContentBlock::ToolUse { id, name, .. } => {
                                Some(CurrentBlock::ToolUse {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input_json: String::new(),
                                })
                            }
                            AnthropicContentBlock::Thinking { .. } => {
                                Some(CurrentBlock::Thinking {
                                    thinking: String::new(),
                                    signature: String::new(),
                                })
                            }
                            _ => {
                                // Text or any other block type
                                Some(CurrentBlock::Text {
                                    text: String::new(),
                                })
                            }
                        };
                    }
                    AnthropicSseEvent::ContentBlockDelta { delta, .. } => {
                        use super::types::AnthropicSseDelta;
                        match delta {
                            AnthropicSseDelta::TextDelta { text } => {
                                if let Some(CurrentBlock::Text { text: ref mut buf }) =
                                    current_block
                                {
                                    buf.push_str(text);
                                } else if current_block.is_none() {
                                    // Fallback: no ContentBlockStart seen, create text block
                                    current_block = Some(CurrentBlock::Text { text: text.clone() });
                                }
                            }
                            AnthropicSseDelta::ThinkingDelta { thinking } => {
                                if let Some(CurrentBlock::Thinking {
                                    thinking: ref mut buf,
                                    ..
                                }) = current_block
                                {
                                    buf.push_str(thinking);
                                } else if current_block.is_none() {
                                    current_block = Some(CurrentBlock::Thinking {
                                        thinking: thinking.clone(),
                                        signature: String::new(),
                                    });
                                }
                            }
                            AnthropicSseDelta::SignatureDelta { signature } => {
                                if let Some(CurrentBlock::Thinking {
                                    signature: ref mut sig,
                                    ..
                                }) = current_block
                                {
                                    sig.push_str(signature);
                                }
                            }
                            AnthropicSseDelta::InputJsonDelta { partial_json } => {
                                if let Some(CurrentBlock::ToolUse {
                                    ref mut input_json, ..
                                }) = current_block
                                {
                                    input_json.push_str(partial_json);
                                }
                            }
                        }
                    }
                    AnthropicSseEvent::ContentBlockStop { .. } => {
                        // Finalize and push the current block
                        if let Some(block) = current_block.take() {
                            match block {
                                CurrentBlock::Text { text } => {
                                    if !text.is_empty() {
                                        content_blocks.push(AnthropicContentBlock::Text {
                                            text,
                                            cache_control: None,
                                        });
                                    }
                                }
                                CurrentBlock::Thinking {
                                    thinking,
                                    signature,
                                } => {
                                    if !thinking.is_empty() {
                                        content_blocks.push(AnthropicContentBlock::Thinking {
                                            thinking,
                                            signature: if signature.is_empty() {
                                                None
                                            } else {
                                                Some(signature)
                                            },
                                        });
                                    }
                                }
                                CurrentBlock::ToolUse {
                                    id,
                                    name,
                                    input_json,
                                } => {
                                    let input =
                                        serde_json::from_str::<serde_json::Value>(&input_json)
                                            .unwrap_or(serde_json::Value::Object(
                                                serde_json::Map::new(),
                                            ));
                                    content_blocks.push(AnthropicContentBlock::ToolUse {
                                        id,
                                        name,
                                        input,
                                    });
                                }
                            }
                        }
                    }
                    AnthropicSseEvent::MessageDelta {
                        delta,
                        usage: delta_usage,
                    } => {
                        stop_reason = delta.stop_reason.clone();
                        if let Some(du) = delta_usage {
                            // Counters are cumulative and split across events:
                            // official Anthropic puts input+cache in
                            // message_start, while some proxies report the full
                            // terminal usage here instead. Take the max of each
                            // so neither layout loses data.
                            let prev = usage.as_ref();
                            usage = Some(AnthropicUsage {
                                input_tokens: prev
                                    .map(|u| u.input_tokens)
                                    .unwrap_or(0)
                                    .max(du.input_tokens.unwrap_or(0)),
                                output_tokens: prev
                                    .map(|u| u.output_tokens)
                                    .unwrap_or(0)
                                    .max(du.output_tokens),
                                cache_creation_input_tokens: max_opt(
                                    prev.and_then(|u| u.cache_creation_input_tokens),
                                    du.cache_creation_input_tokens,
                                ),
                                cache_read_input_tokens: max_opt(
                                    prev.and_then(|u| u.cache_read_input_tokens),
                                    du.cache_read_input_tokens,
                                ),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        // Flush any remaining block that didn't get a ContentBlockStop
        if let Some(block) = current_block.take() {
            match block {
                CurrentBlock::Text { text } => {
                    if !text.is_empty() {
                        content_blocks.push(AnthropicContentBlock::Text {
                            text,
                            cache_control: None,
                        });
                    }
                }
                CurrentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    if !thinking.is_empty() {
                        content_blocks.push(AnthropicContentBlock::Thinking {
                            thinking,
                            signature: if signature.is_empty() {
                                None
                            } else {
                                Some(signature)
                            },
                        });
                    }
                }
                CurrentBlock::ToolUse {
                    id,
                    name,
                    input_json,
                } => {
                    let input = serde_json::from_str::<serde_json::Value>(&input_json)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    content_blocks.push(AnthropicContentBlock::ToolUse { id, name, input });
                }
            }
        }

        // Build aggregated response
        // If message_start is available, use its metadata; otherwise use defaults.
        // Some proxies (e.g. DashScope) strip the message_start event from the
        // SSE stream, so we must still return parsed content blocks.
        if let Some(AnthropicSseEvent::MessageStart { message }) = message_start {
            Some(AnthropicResponse {
                id: message.id,
                type_: "message".to_string(),
                role: MessageRole::Assistant,
                content: content_blocks,
                model: message.model,
                stop_reason,
                stop_sequence: None,
                usage: usage.unwrap_or(AnthropicUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                }),
            })
        } else if !content_blocks.is_empty() {
            // No message_start but we still parsed content blocks — return with defaults
            log::debug!(
                "aggregate_sse_events: no message_start found, returning {} content blocks with defaults",
                content_blocks.len()
            );
            Some(AnthropicResponse {
                id: String::new(),
                type_: "message".to_string(),
                role: MessageRole::Assistant,
                content: content_blocks,
                model: String::new(),
                stop_reason,
                stop_sequence: None,
                usage: usage.unwrap_or(AnthropicUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                }),
            })
        } else {
            None
        }
    }

    /// Check if a path matches Anthropic API endpoints
    ///
    /// # Arguments
    /// * `path` - The HTTP request path
    ///
    /// # Returns
    /// * `true` if the path matches Anthropic endpoints
    pub fn matches_path(path: &str) -> bool {
        path.contains("/v1/messages")
    }
}

impl Default for AnthropicParser {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request_simple() {
        let json = serde_json::json!({
            "model": "claude-3-opus-20240229",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "Hello, how are you?"}
            ]
        });

        let request = AnthropicParser::parse_request(&json);
        assert!(request.is_some());

        let request = request.unwrap();
        assert_eq!(request.model, "claude-3-opus-20240229");
        assert_eq!(request.max_tokens, 1024);
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn test_parse_request_with_system() {
        let json = serde_json::json!({
            "model": "claude-3-sonnet-20240229",
            "max_tokens": 2048,
            "system": "You are a helpful AI assistant.",
            "messages": [
                {"role": "user", "content": "Tell me a joke."}
            ],
            "temperature": 0.7,
            "stream": true
        });

        let request = AnthropicParser::parse_request(&json);
        assert!(request.is_some());

        let request = request.unwrap();
        assert_eq!(request.model, "claude-3-sonnet-20240229");
        assert_eq!(request.max_tokens, 2048);
        assert!(request.system.is_some());
        assert_eq!(request.temperature, Some(0.7));
        assert_eq!(request.stream, Some(true));
    }

    #[test]
    fn test_parse_request_with_system_blocks() {
        let json = serde_json::json!({
            "model": "claude-3-opus-20240229",
            "max_tokens": 1024,
            "system": [
                {
                    "type": "text",
                    "text": "You are a helpful assistant.",
                    "cache_control": {"type": "ephemeral"}
                }
            ],
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let request = AnthropicParser::parse_request(&json);
        assert!(request.is_some());

        let request = request.unwrap();
        assert!(request.system.is_some());
    }

    #[test]
    fn test_parse_request_missing_model() {
        let json = serde_json::json!({
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let request = AnthropicParser::parse_request(&json);
        assert!(request.is_none());
    }

    #[test]
    fn test_parse_request_missing_max_tokens() {
        let json = serde_json::json!({
            "model": "claude-3-opus-20240229",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let request = AnthropicParser::parse_request(&json);
        assert!(request.is_none());
    }

    #[test]
    fn test_parse_response_simple() {
        let json = serde_json::json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "Hello! I'm doing well, thank you for asking."
                }
            ],
            "model": "claude-3-opus-20240229",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12,
                "output_tokens": 15
            }
        });

        let response = AnthropicParser::parse_response(&json);
        assert!(response.is_some());

        let response = response.unwrap();
        assert_eq!(response.id, "msg_01XFDUDYJgAACzvnptvVoYEL");
        assert_eq!(response.model, "claude-3-opus-20240229");
        assert_eq!(response.content.len(), 1);
        assert_eq!(response.stop_reason, Some("end_turn".to_string()));
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(response.usage.output_tokens, 15);
    }

    #[test]
    fn test_parse_response_with_cache_tokens() {
        let json = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Hello!"}
            ],
            "model": "claude-3-opus-20240229",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 10,
                "cache_read_input_tokens": 20
            }
        });

        let response = AnthropicParser::parse_response(&json);
        assert!(response.is_some());

        let response = response.unwrap();
        assert_eq!(response.usage.input_tokens, 100);
        assert_eq!(response.usage.output_tokens, 50);
        assert_eq!(response.usage.cache_creation_input_tokens, Some(10));
        assert_eq!(response.usage.cache_read_input_tokens, Some(20));
    }

    #[test]
    fn test_parse_response_missing_id() {
        let json = serde_json::json!({
            "type": "message",
            "content": []
        });

        let response = AnthropicParser::parse_response(&json);
        assert!(response.is_none());
    }

    #[test]
    fn test_parse_response_wrong_type() {
        let json = serde_json::json!({
            "id": "msg_123",
            "type": "error",
            "content": []
        });

        let response = AnthropicParser::parse_response(&json);
        assert!(response.is_none());
    }

    #[test]
    fn test_matches_path() {
        assert!(AnthropicParser::matches_path("/v1/messages"));
        assert!(AnthropicParser::matches_path(
            "https://api.anthropic.com/v1/messages"
        ));
        assert!(!AnthropicParser::matches_path("/v1/chat/completions"));
        assert!(!AnthropicParser::matches_path("/v1/completions"));
    }

    #[test]
    fn test_parse_response_with_tool_use() {
        let json = serde_json::json!({
            "id": "msg_456",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "I'll check the weather for you."
                },
                {
                    "type": "tool_use",
                    "id": "toolu_01A09q90qw90lq917835lhl",
                    "name": "get_weather",
                    "input": {"location": "San Francisco, CA"}
                }
            ],
            "model": "claude-3-opus-20240229",
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 50,
                "output_tokens": 30
            }
        });

        let response = AnthropicParser::parse_response(&json);
        assert!(response.is_some());

        let response = response.unwrap();
        assert_eq!(response.content.len(), 2);
        assert_eq!(response.stop_reason, Some("tool_use".to_string()));
    }

    #[test]
    fn test_parse_request_with_content_blocks() {
        let json = serde_json::json!({
            "model": "claude-3-opus-20240229",
            "max_tokens": 1024,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/jpeg",
                                "data": "base64_encoded_data_here"
                            }
                        },
                        {
                            "type": "text",
                            "text": "What is in this image?"
                        }
                    ]
                }
            ]
        });

        let request = AnthropicParser::parse_request(&json);
        assert!(request.is_some());

        let request = request.unwrap();
        assert_eq!(request.messages.len(), 1);
    }

    /// Test: SSE stream with text + tool_use mixed content (Claude Code typical pattern)
    #[test]
    fn test_aggregate_sse_with_tool_use() {
        let events = serde_json::json!([
            {
                "type": "message_start",
                "message": {
                    "id": "msg_01",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-20250514",
                    "content": [],
                    "usage": {"input_tokens": 100, "output_tokens": 0}
                }
            },
            {
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            },
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Let me read "}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "that file."}},
            {"type": "content_block_stop", "index": 0},
            {
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_01ABC",
                    "name": "Read",
                    "input": {}
                }
            },
            {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"/src/"}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "main.rs\"}"}},
            {"type": "content_block_stop", "index": 1},
            {
                "type": "message_delta",
                "delta": {"stop_reason": "tool_use"},
                "usage": {"output_tokens": 42}
            },
            {"type": "message_stop"}
        ]);

        let response = AnthropicParser::parse_response(&events);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(resp.id, "msg_01");
        assert_eq!(resp.content.len(), 2);

        // First block: text
        match &resp.content[0] {
            AnthropicContentBlock::Text { text, .. } => {
                assert_eq!(text, "Let me read that file.");
            }
            other => panic!("Expected Text block, got {other:?}"),
        }

        // Second block: tool_use
        match &resp.content[1] {
            AnthropicContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_01ABC");
                assert_eq!(name, "Read");
                assert_eq!(input["path"], "/src/main.rs");
            }
            other => panic!("Expected ToolUse block, got {other:?}"),
        }

        assert_eq!(resp.stop_reason, Some("tool_use".to_string()));
        assert_eq!(resp.usage.output_tokens, 42);
    }

    /// Official Anthropic layout: input + cache in message_start, output only in
    /// message_delta. Both must survive the merge.
    #[test]
    fn test_aggregate_sse_usage_official_split() {
        let events = serde_json::json!([
            {
                "type": "message_start",
                "message": {
                    "id": "msg_split",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-5",
                    "content": [],
                    "usage": {
                        "input_tokens": 1234,
                        "output_tokens": 1,
                        "cache_creation_input_tokens": 5678,
                        "cache_read_input_tokens": 90
                    }
                }
            },
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}},
            {"type": "content_block_stop", "index": 0},
            {
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": 42}
            }
        ]);

        let resp = AnthropicParser::parse_response(&events).expect("should aggregate");
        assert_eq!(resp.usage.input_tokens, 1234);
        assert_eq!(resp.usage.output_tokens, 42);
        assert_eq!(resp.usage.cache_creation_input_tokens, Some(5678));
        assert_eq!(resp.usage.cache_read_input_tokens, Some(90));
    }

    /// Proxy layout: zero-placeholder message_start, full terminal usage in
    /// message_delta. The zero start must not mask the delta's counters.
    #[test]
    fn test_aggregate_sse_usage_delta_carries_full_usage() {
        let events = serde_json::json!([
            {
                "type": "message_start",
                "message": {
                    "id": "msg_proxy",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-5",
                    "content": [],
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            },
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}},
            {"type": "content_block_stop", "index": 0},
            {
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn"},
                "usage": {
                    "output_tokens": 13,
                    "input_tokens": 10,
                    "cache_creation_input_tokens": 22178,
                    "cache_read_input_tokens": 7
                }
            }
        ]);

        let resp = AnthropicParser::parse_response(&events).expect("should aggregate");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 13);
        assert_eq!(resp.usage.cache_creation_input_tokens, Some(22178));
        assert_eq!(resp.usage.cache_read_input_tokens, Some(7));
    }

    /// Test: SSE stream with multiple tool calls
    #[test]
    fn test_aggregate_sse_multiple_tool_calls() {
        let events = serde_json::json!([
            {
                "type": "message_start",
                "message": {
                    "id": "msg_02",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-20250514",
                    "content": [],
                    "usage": {"input_tokens": 200, "output_tokens": 0}
                }
            },
            {
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_A", "name": "Bash", "input": {}}
            },
            {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"command\": \"ls\"}"}},
            {"type": "content_block_stop", "index": 0},
            {
                "type": "content_block_start",
                "index": 1,
                "content_block": {"type": "tool_use", "id": "toolu_B", "name": "Read", "input": {}}
            },
            {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"Cargo.toml\"}"}},
            {"type": "content_block_stop", "index": 1},
            {
                "type": "message_delta",
                "delta": {"stop_reason": "tool_use"},
                "usage": {"output_tokens": 30}
            }
        ]);

        let response = AnthropicParser::parse_response(&events);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(resp.content.len(), 2);

        // Both should be ToolUse
        match &resp.content[0] {
            AnthropicContentBlock::ToolUse { name, .. } => assert_eq!(name, "Bash"),
            other => panic!("Expected ToolUse, got {other:?}"),
        }
        match &resp.content[1] {
            AnthropicContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "Read");
                assert_eq!(input["path"], "Cargo.toml");
            }
            other => panic!("Expected ToolUse, got {other:?}"),
        }
    }

    /// Test: InputJsonDelta fragments are correctly concatenated
    #[test]
    fn test_aggregate_sse_input_json_delta() {
        let events = serde_json::json!([
            {
                "type": "message_start",
                "message": {
                    "id": "msg_03",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-20250514",
                    "content": [],
                    "usage": {"input_tokens": 50, "output_tokens": 0}
                }
            },
            {
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_C", "name": "Write", "input": {}}
            },
            {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"/tmp/"}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "test.txt\", "}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"content\": \"hello\"}"}},
            {"type": "content_block_stop", "index": 0},
            {
                "type": "message_delta",
                "delta": {"stop_reason": "tool_use"},
                "usage": {"output_tokens": 20}
            }
        ]);

        let response = AnthropicParser::parse_response(&events);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(resp.content.len(), 1);

        match &resp.content[0] {
            AnthropicContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_C");
                assert_eq!(name, "Write");
                assert_eq!(input["path"], "/tmp/test.txt");
                assert_eq!(input["content"], "hello");
            }
            other => panic!("Expected ToolUse, got {other:?}"),
        }
    }
}
