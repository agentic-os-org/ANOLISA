//! Aliyun SysOM Copilot API parser
//!
//! This module parses request/response bodies for the Aliyun SysOM Copilot API,
//! which is used when cosh runs under AK/SK (AccessKey) authentication mode.
//!
//! # API Endpoints
//! - `/api/v1/copilot/generate_copilot_response`       (non-streaming)
//! - `/api/v1/copilot/generate_copilot_stream_response` (streaming / SSE)
//!
//! # Request Format
//! The request body contains a single JSON string field `llmParamString` which
//! itself is a JSON-encoded object with the real LLM parameters:
//! ```json
//! {
//!   "llmParamString": "{\"model\":\"qwen3-coder-plus\",\"messages\":[...],\"stream\":true,\"use_dashscope\":true}"
//! }
//! ```
//!
//! # Response Format (non-streaming)
//! The SysOM API wraps the actual LLM response in `body.data`:
//! ```json
//! { "data": "{\"choices\":[{\"message\":{\"content\":\"...\",\"tool_use\":[...]}}]}" }
//! ```
//!
//! The inner `choices` structure differs from standard OpenAI:
//! - `message.tool_use` is an **array** (not `tool_calls`)
//! - Each tool_use item: `{id, type, function: {name, arguments}}`
//!
//! # Response Format (streaming / SSE)
//! Each SSE chunk is a JSON object parsed the same way as non-streaming,
//! but the content is delivered incrementally (accumulated by agentsight's
//! SSE parser into a JSON array before reaching this parser).

use serde::{Deserialize, Serialize};

// ============================================================================
// SysOM-specific request / response types
// ============================================================================

/// Inner LLM parameters decoded from `llmParamString`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomLlmParams {
    /// Model name (e.g. "qwen3-coder-plus")
    pub model: String,

    /// Conversation messages
    #[serde(default)]
    pub messages: Vec<SysomMessage>,

    /// Whether streaming is requested
    #[serde(default)]
    pub stream: bool,

    /// Sampling temperature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Maximum tokens to generate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Top-p sampling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// Tool definitions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,

    /// Whether to route via DashScope backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_dashscope: Option<bool>,
}

/// A single message in the SysOM conversation (OpenAI-compatible format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomMessage {
    /// Role: "system" | "user" | "assistant" | "tool"
    pub role: String,

    /// Text content
    #[serde(default)]
    pub content: String,

    /// Tool call ID (for role=tool messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// Tool call name (for role=tool messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Tool calls issued by the assistant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<SysomToolCall>>,
}

/// An assistant-issued tool call (inside request messages)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: SysomFunction,
}

/// Function name + arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomFunction {
    pub name: String,
    pub arguments: String,
}

/// Parsed outer request body (contains `llmParamString`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomRequest {
    /// The actual LLM parameters (decoded from `llmParamString`)
    pub params: SysomLlmParams,
}

/// A single tool-use item in the SysOM response (array format, differs from OpenAI)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomToolUseItem {
    pub index: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub function: SysomFunction,
}

/// A single response choice from the SysOM API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomResponseChoice {
    pub message: SysomResponseMessage,
}

/// The message part of a SysOM response choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomResponseMessage {
    /// Text content of the response
    #[serde(default)]
    pub content: String,

    /// Tool calls in the response (`tool_use` array, NOT `tool_calls`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<Vec<SysomToolUseItem>>,
}

/// Parsed SysOM API response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysomResponse {
    /// Response ID (from DashScope backend, e.g. "chatcmpl-xxx")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Response choices
    pub choices: Vec<SysomResponseChoice>,
}

// ============================================================================
// SysomParser
// ============================================================================

/// Parser for the Aliyun SysOM Copilot API
pub struct SysomParser;

impl SysomParser {
    /// Returns `true` if the HTTP path targets the SysOM Copilot API
    pub fn matches_path(path: &str) -> bool {
        path.contains("/api/v1/copilot/generate_copilot")
    }

    /// Parse the outer request body, decoding `llmParamString` into [`SysomRequest`]
    ///
    /// Returns `None` if `llmParamString` is absent or cannot be decoded.
    pub fn parse_request(body: &serde_json::Value) -> Option<SysomRequest> {
        let llm_param_string = body.get("llmParamString").and_then(|v| v.as_str())?;

        let params: SysomLlmParams = serde_json::from_str(llm_param_string)
            .map_err(|e| {
                log::trace!("[SysomParser] Failed to decode llmParamString: {e}");
            })
            .ok()?;

        log::debug!(
            "[SysomParser] Parsed request: model={}, messages={}, stream={}",
            params.model,
            params.messages.len(),
            params.stream,
        );

        Some(SysomRequest { params })
    }

    /// Parse a SysOM response body.
    ///
    /// Accepts two formats:
    /// 1. **Non-streaming**: `{ "choices": [...] }` (inner JSON already decoded by caller)
    /// 2. **SSE / streaming**: JSON array of chunks — each chunk has the same
    ///    `choices` structure; the last non-empty chunk's content is used.
    pub fn parse_response(body: &serde_json::Value) -> Option<SysomResponse> {
        // Non-streaming: direct `choices` object
        if body.get("choices").is_some() {
            return serde_json::from_value::<SysomResponse>(body.clone())
                .map_err(|e| log::trace!("[SysomParser] Failed to parse response: {e}"))
                .ok();
        }

        // Streaming: JSON array of SSE chunks — aggregate content and last tool_use
        if let Some(chunks) = body.as_array() {
            return Self::aggregate_sse_chunks(chunks);
        }

        None
    }

    /// Aggregate SSE chunk array into a single [`SysomResponse`]
    ///
    /// SysOM SSE chunks carry **cumulative** content (each chunk contains the
    /// full text so far, not a delta).  We therefore take the **last** chunk
    /// that contains non-empty content/tool_use data.
    fn aggregate_sse_chunks(chunks: &[serde_json::Value]) -> Option<SysomResponse> {
        let mut last_content = String::new();
        let mut last_tool_use: Option<Vec<SysomToolUseItem>> = None;
        let mut last_id: Option<String> = None;

        for chunk in chunks {
            if let Ok(resp) = serde_json::from_value::<SysomResponse>(chunk.clone()) {
                if resp.id.is_some() {
                    last_id = resp.id;
                }
                if let Some(choice) = resp.choices.into_iter().next() {
                    if !choice.message.content.is_empty() {
                        last_content = choice.message.content;
                    }
                    if choice.message.tool_use.is_some() {
                        last_tool_use = choice.message.tool_use;
                    }
                }
            }
        }

        if last_content.is_empty() && last_tool_use.is_none() {
            return None;
        }

        Some(SysomResponse {
            id: last_id,
            choices: vec![SysomResponseChoice {
                message: SysomResponseMessage {
                    content: last_content,
                    tool_use: last_tool_use,
                },
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_path() {
        assert!(SysomParser::matches_path(
            "/api/v1/copilot/generate_copilot_response"
        ));
        assert!(SysomParser::matches_path(
            "/api/v1/copilot/generate_copilot_stream_response"
        ));
        assert!(!SysomParser::matches_path("/v1/chat/completions"));
        assert!(!SysomParser::matches_path("/v1/messages"));
    }

    #[test]
    fn test_parse_request() {
        let params = serde_json::json!({
            "model": "qwen3-coder-plus",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello"}
            ],
            "stream": true,
            "use_dashscope": true
        });
        let body = serde_json::json!({
            "llmParamString": params.to_string()
        });

        let req = SysomParser::parse_request(&body).unwrap();
        assert_eq!(req.params.model, "qwen3-coder-plus");
        assert_eq!(req.params.messages.len(), 2);
        assert!(req.params.stream);
    }

    #[test]
    fn test_parse_response_non_streaming() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Hello there!",
                    "tool_use": null
                }
            }]
        });

        let resp = SysomParser::parse_response(&body).unwrap();
        assert_eq!(resp.choices[0].message.content, "Hello there!");
    }

    #[test]
    fn test_parse_response_with_tool_use() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_use": [{
                        "index": 0,
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"/tmp/test.txt\"}"
                        }
                    }]
                }
            }]
        });

        let resp = SysomParser::parse_response(&body).unwrap();
        let tool_use = resp.choices[0].message.tool_use.as_ref().unwrap();
        assert_eq!(tool_use[0].function.name, "read_file");
        assert_eq!(tool_use[0].id, "call_abc123");
    }

    #[test]
    fn test_parse_response_sse_chunks() {
        // Simulate cumulative SSE chunks
        let chunks = serde_json::json!([
            {"choices": [{"message": {"content": "Hel"}}]},
            {"choices": [{"message": {"content": "Hello"}}]},
            {"choices": [{"message": {"content": "Hello there!"}}]}
        ]);

        let resp = SysomParser::parse_response(&chunks).unwrap();
        assert_eq!(resp.choices[0].message.content, "Hello there!");
    }
}
