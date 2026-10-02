//! OpenAI Chat Completions API parser
//!
//! This module provides parsing functionality for OpenAI Chat Completions API
//! request and response bodies.
//!
//! # Supported Endpoints
//! - `/v1/chat/completions`
//! - `/v1/completions` (legacy)
//!
//! # Example
//! ```rust,ignore
//! use agentsight::analyzer::message::{OpenAIParser, OpenAIRequest, OpenAIResponse};
//!
//! let parser = OpenAIParser;
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
//!     println!("Completion ID: {}", response.id);
//! }
//! ```

use super::types::{
    MessageRole, OpenAIChatMessage, OpenAIChoice, OpenAIContent, OpenAIRequest, OpenAIResponse,
    OpenAiSseChunk,
};

/// Parser for OpenAI Chat Completions API
///
/// Provides methods to parse JSON request and response bodies
/// from OpenAI-compatible APIs.
pub struct OpenAIParser;

impl OpenAIParser {
    /// Parse an OpenAI Chat Completions request body from JSON
    ///
    /// # Arguments
    /// * `body` - The JSON value representing the request body
    ///
    /// # Returns
    /// * `Some(OpenAIRequest)` if parsing succeeds
    /// * `None` if the JSON doesn't match the expected format
    ///
    /// # Example
    /// ```rust,ignore
    /// let json = serde_json::json!({
    ///     "model": "gpt-4",
    ///     "messages": [{"role": "user", "content": "Hello"}]
    /// });
    /// let request = OpenAIParser::parse_request(&json);
    /// ```
    pub fn parse_request(body: &serde_json::Value) -> Option<OpenAIRequest> {
        // Responses API normalization: {model, input} → {model, messages}
        if body.get("model").is_some()
            && body.get("messages").is_none()
            && body.get("input").is_some()
        {
            return Self::normalize_responses_request(body);
        }

        // Quick validation - must have model and messages fields
        if body.get("model").is_none() || body.get("messages").is_none() {
            log::trace!("OpenAI request missing required fields: model or messages");
            return None;
        }

        match serde_json::from_value::<OpenAIRequest>(body.clone()) {
            Ok(request) => {
                log::debug!(
                    "Parsed OpenAI request: model={}, messages={}",
                    request.model,
                    request.messages.len()
                );
                Some(request)
            }
            Err(e) => {
                log::trace!("Failed to parse OpenAI request: {e}");
                None
            }
        }
    }

    fn normalize_responses_request(body: &serde_json::Value) -> Option<OpenAIRequest> {
        let mut normalized = body.clone();
        let input = body.get("input")?;

        let mut messages = Vec::new();
        if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
            messages.push(serde_json::json!({"role": "system", "content": instructions}));
        }
        if let Some(text) = input.as_str() {
            messages.push(serde_json::json!({"role": "user", "content": text}));
        } else if let Some(arr) = input.as_array() {
            for item in arr {
                if item.get("role").is_some() {
                    let mut msg = item.clone();
                    if let Some(parts) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                        for part in parts.iter_mut() {
                            if part.get("type").and_then(|t| t.as_str()) == Some("input_text") {
                                part["type"] = serde_json::json!("text");
                            }
                        }
                    }
                    messages.push(msg);
                } else if let Some(t) = item.get("type").and_then(|t| t.as_str()) {
                    match t {
                        "input_text" => {
                            let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            messages.push(serde_json::json!({"role": "user", "content": text}));
                        }
                        _ => {
                            messages.push(
                                serde_json::json!({"role": "user", "content": item.to_string()}),
                            );
                        }
                    }
                } else {
                    messages.push(item.clone());
                }
            }
        } else {
            return None;
        }

        normalized["messages"] = serde_json::Value::Array(messages);
        if let Some(stream) = body.get("stream") {
            normalized["stream"] = stream.clone();
        }

        serde_json::from_value::<OpenAIRequest>(normalized).ok()
    }

    /// Parse an OpenAI Chat Completions response body from JSON
    ///
    /// # Arguments
    /// * `body` - The JSON value representing the response body
    ///
    /// # Returns
    /// * `Some(OpenAIResponse)` if parsing succeeds
    /// * `None` if the JSON doesn't match the expected format
    ///
    /// # Example
    /// ```rust,ignore
    /// let json = serde_json::json!({
    ///     "id": "chatcmpl-123",
    ///     "object": "chat.completion",
    ///     "created": 1677652288,
    ///     "model": "gpt-4",
    ///     "choices": [...]
    /// });
    /// let response = OpenAIParser::parse_response(&json);
    /// ```
    pub fn parse_response(body: &serde_json::Value) -> Option<OpenAIResponse> {
        // Responses API format: object=="response" + output[]
        if body.get("output").is_some()
            && body
                .get("object")
                .and_then(|v| v.as_str())
                .map(|s| s == "response")
                .unwrap_or(false)
        {
            return Self::normalize_responses_response(body);
        }

        // Try standard response format first (has id and choices)
        if body.get("id").is_some() && body.get("choices").is_some() {
            match serde_json::from_value::<OpenAIResponse>(body.clone()) {
                Ok(response) => {
                    log::debug!(
                        "Parsed OpenAI response: id={}, model={}, choices={}",
                        response.id,
                        response.model,
                        response.choices.len()
                    );
                    return Some(response);
                }
                Err(e) => {
                    log::trace!("Failed to parse OpenAI response: {e}");
                }
            }
        }

        // Try SSE chunks array format (body is an array of SSE chunks)
        if let Some(chunks) = body.as_array() {
            // Detect Responses API SSE format vs chat/completions SSE
            if let Some(first) = chunks.first() {
                if first
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(|t| t.starts_with("response."))
                    .unwrap_or(false)
                {
                    return Self::aggregate_responses_sse_chunks(chunks);
                }
            }
            return Self::aggregate_sse_chunks(chunks);
        }

        None
    }

    fn normalize_responses_response(body: &serde_json::Value) -> Option<OpenAIResponse> {
        let output = body.get("output")?.as_array()?;

        let mut content_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        let mut finish_reason = Some("stop".to_string());

        for item in output {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                        for part in content {
                            if part
                                .get("type")
                                .and_then(|t| t.as_str())
                                .map(|t| t == "output_text")
                                .unwrap_or(false)
                            {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    content_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                "function_call" => {
                    let tc = serde_json::json!({
                        "id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                        "type": "function",
                        "function": {
                            "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": item.get("arguments").and_then(|v| v.as_str()).unwrap_or(""),
                        }
                    });
                    tool_calls.push(tc);
                }
                _ => {}
            }
        }

        let message_content = content_parts.join("");
        let mut message = serde_json::json!({
            "role": "assistant",
            "content": if message_content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(message_content) },
        });
        if !tool_calls.is_empty() {
            message["tool_calls"] = serde_json::Value::Array(tool_calls);
            finish_reason = Some("tool_calls".to_string());
        }

        let usage_val = body.get("usage").map(|u| {
            serde_json::json!({
                "prompt_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                "completion_tokens": u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                "total_tokens": u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            })
        });

        let resp = serde_json::json!({
            "id": body.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            "object": "chat.completion",
            "created": body.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0),
            "model": body.get("model").and_then(|v| v.as_str()).unwrap_or(""),
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason,
            }],
            "usage": usage_val,
        });

        serde_json::from_value::<OpenAIResponse>(resp).ok()
    }

    fn aggregate_responses_sse_chunks(chunks: &[serde_json::Value]) -> Option<OpenAIResponse> {
        let mut content_buf = String::new();
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        let mut tc_name = String::new();
        let mut tc_id = String::new();
        let mut tc_args = String::new();
        let mut model = String::new();
        let mut resp_id = String::new();
        let mut usage: Option<serde_json::Value> = None;

        for chunk in chunks {
            let event_type = chunk.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match event_type {
                "response.output_text.delta" => {
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        content_buf.push_str(delta);
                    }
                }
                "response.output_item.added" => {
                    if let Some(item) = chunk.get("item") {
                        if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                            tc_name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            tc_id = item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            tc_args.clear();
                        }
                    }
                }
                "response.function_call_arguments.delta" => {
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        tc_args.push_str(delta);
                    }
                }
                "response.function_call_arguments.done" => {
                    if !tc_name.is_empty() {
                        tool_calls.push(serde_json::json!({
                            "id": tc_id,
                            "type": "function",
                            "function": {"name": tc_name, "arguments": tc_args}
                        }));
                        tc_name.clear();
                        tc_id.clear();
                        tc_args.clear();
                    }
                }
                "response.completed" => {
                    if let Some(resp) = chunk.get("response") {
                        model = resp
                            .get("model")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        resp_id = resp
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        usage = resp.get("usage").map(|u| {
                            serde_json::json!({
                                "prompt_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                "completion_tokens": u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                "total_tokens": u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                            })
                        });
                    }
                }
                "response.created" => {
                    if let Some(resp) = chunk.get("response") {
                        if resp_id.is_empty() {
                            resp_id = resp
                                .get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                }
                _ => {}
            }
        }

        // Flush any in-flight tool call (truncated stream without "done" event)
        if !tc_name.is_empty() {
            tool_calls.push(serde_json::json!({
                "id": tc_id,
                "type": "function",
                "function": {"name": tc_name, "arguments": tc_args}
            }));
        }

        let mut message = serde_json::json!({
            "role": "assistant",
            "content": if content_buf.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(content_buf) },
        });
        let finish_reason = if !tool_calls.is_empty() {
            message["tool_calls"] = serde_json::Value::Array(tool_calls);
            "tool_calls"
        } else {
            "stop"
        };

        let resp = serde_json::json!({
            "id": resp_id,
            "object": "chat.completion",
            "created": 0u64,
            "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
            "usage": usage,
        });

        serde_json::from_value::<OpenAIResponse>(resp).ok()
    }

    /// Aggregate SSE chunks into a single OpenAIResponse
    fn aggregate_sse_chunks(chunks: &[serde_json::Value]) -> Option<OpenAIResponse> {
        use std::collections::HashMap;

        let mut content_parts: Vec<String> = Vec::new();
        let mut reasoning_parts: Vec<String> = Vec::new();
        let mut finish_reason: Option<String> = None;
        let mut first_chunk: Option<&serde_json::Value> = None;
        // Merge tool_call deltas by index: index -> (id, name, arguments_accumulated)
        let mut tool_call_map: HashMap<u32, (String, String, String)> = HashMap::new();

        for chunk in chunks {
            // Try to parse as OpenAiSseChunk
            if let Ok(sse_chunk) = serde_json::from_value::<OpenAiSseChunk>(chunk.clone()) {
                if first_chunk.is_none() {
                    first_chunk = Some(chunk);
                }
                // Extract content delta for aggregation
                for choice in &sse_chunk.choices {
                    if let Some(content) = &choice.delta.content {
                        if !content.is_empty() {
                            content_parts.push(content.clone());
                        }
                    }
                    // Extract reasoning_content delta
                    if let Some(reasoning) = &choice.delta.reasoning_content {
                        if !reasoning.is_empty() {
                            reasoning_parts.push(reasoning.clone());
                        }
                    }
                    // Extract and merge tool_call deltas by index
                    if let Some(calls) = &choice.delta.tool_calls {
                        for tc in calls {
                            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let entry = tool_call_map
                                .entry(idx)
                                .or_insert_with(|| (String::new(), String::new(), String::new()));
                            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                if !id.is_empty() {
                                    entry.0 = id.to_string();
                                }
                                // 空字符串不覆盖已有的 id
                            }
                            if let Some(func) = tc.get("function") {
                                if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                    if !name.is_empty() {
                                        entry.1 = name.to_string();
                                    }
                                    // Empty string must not overwrite an existing name: some
                                    // backends (e.g. deepseek models on DashScope) repeat
                                    // name:"" on every continuation delta.
                                }
                                if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                    entry.2.push_str(args);
                                }
                            }
                        }
                    }
                    if finish_reason.is_none() && choice.finish_reason.is_some() {
                        finish_reason = choice.finish_reason.clone();
                    }
                }
            }
        }

        // Build merged tool_calls
        let tool_calls = if tool_call_map.is_empty() {
            None
        } else {
            let mut sorted_indices: Vec<u32> = tool_call_map.keys().cloned().collect();
            sorted_indices.sort();
            let merged: Vec<serde_json::Value> = sorted_indices
                .into_iter()
                .filter_map(|idx| {
                    tool_call_map.remove(&idx).map(|(id, name, arguments)| {
                        serde_json::json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments
                            }
                        })
                    })
                })
                .collect();
            if merged.is_empty() {
                None
            } else {
                Some(merged)
            }
        };

        // Build aggregated response from chunks
        first_chunk.and_then(|first| {
            serde_json::from_value::<OpenAiSseChunk>(first.clone())
                .ok()
                .map(|chunk| {
                    let combined_content = content_parts.join("");
                    let combined_reasoning = if reasoning_parts.is_empty() {
                        None
                    } else {
                        Some(reasoning_parts.join(""))
                    };
                    OpenAIResponse {
                        id: chunk.id,
                        object: "chat.completion".to_string(),
                        created: chunk.created,
                        model: chunk.model,
                        choices: vec![OpenAIChoice {
                            index: 0,
                            message: OpenAIChatMessage {
                                role: MessageRole::Assistant,
                                content: Some(OpenAIContent::Text(combined_content)),
                                reasoning_content: combined_reasoning,
                                refusal: None,
                                function_call: None,
                                tool_calls,
                                tool_call_id: None,
                                name: None,
                                annotations: None,
                                audio: None,
                            },
                            finish_reason,
                            logprobs: None,
                        }],
                        usage: None,
                        system_fingerprint: chunk.system_fingerprint,
                    }
                })
        })
    }

    /// Check if a path matches OpenAI API endpoints
    ///
    /// # Arguments
    /// * `path` - The HTTP request path
    ///
    /// # Returns
    /// * `true` if the path matches OpenAI endpoints
    pub fn matches_path(path: &str) -> bool {
        path.contains("/v1/chat/completions")
            || path.contains("/v1/completions")
            || path.contains("/v1/responses")
    }
}

impl Default for OpenAIParser {
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
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello, how are you?"}
            ]
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_some());

        let request = request.unwrap();
        assert_eq!(request.model, "gpt-4");
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn test_parse_request_with_options() {
        let json = serde_json::json!({
            "model": "gpt-4-turbo",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Tell me a joke."}
            ],
            "temperature": 0.7,
            "max_tokens": 1000,
            "stream": true,
            "top_p": 0.9
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_some());

        let request = request.unwrap();
        assert_eq!(request.model, "gpt-4-turbo");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.temperature, Some(0.7));
        assert_eq!(request.max_tokens, Some(1000));
        assert_eq!(request.stream, Some(true));
        assert_eq!(request.top_p, Some(0.9));
    }

    #[test]
    fn test_parse_request_missing_model() {
        let json = serde_json::json!({
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_none());
    }

    #[test]
    fn test_parse_request_missing_messages() {
        let json = serde_json::json!({
            "model": "gpt-4"
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_none());
    }

    #[test]
    fn test_parse_response_simple() {
        let json = serde_json::json!({
            "id": "chatcmpl-123456",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello! I'm doing well, thank you for asking."
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 15,
                "total_tokens": 25
            }
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_some());

        let response = response.unwrap();
        assert_eq!(response.id, "chatcmpl-123456");
        assert_eq!(response.model, "gpt-4");
        assert_eq!(response.choices.len(), 1);

        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 15);
        assert_eq!(usage.total_tokens, 25);
    }

    #[test]
    fn test_parse_response_missing_id() {
        let json = serde_json::json!({
            "object": "chat.completion",
            "choices": []
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_none());
    }

    #[test]
    fn test_parse_response_missing_choices() {
        let json = serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion"
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_none());
    }

    #[test]
    fn test_matches_path() {
        assert!(OpenAIParser::matches_path("/v1/chat/completions"));
        assert!(OpenAIParser::matches_path("/v1/completions"));
        assert!(OpenAIParser::matches_path(
            "https://api.openai.com/v1/chat/completions"
        ));
        assert!(!OpenAIParser::matches_path("/v1/messages"));
        assert!(!OpenAIParser::matches_path("/v1/embeddings"));
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let json = serde_json::json!({
            "id": "chatcmpl-789",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_abc123",
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "arguments": "{\"location\": \"Boston\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }
            ]
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_some());

        let response = response.unwrap();
        assert_eq!(
            response.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
    }

    // ---- Responses API tests ----

    #[test]
    fn test_matches_path_responses() {
        assert!(OpenAIParser::matches_path("/v1/responses"));
        assert!(OpenAIParser::matches_path(
            "https://dashscope.aliyuncs.com/compatible-mode/v1/responses"
        ));
        // bare /responses should NOT match (too broad, would catch non-LLM traffic)
        assert!(!OpenAIParser::matches_path("/responses"));
        assert!(!OpenAIParser::matches_path("/api/survey/responses"));
    }

    #[test]
    fn test_parse_request_responses_string_input() {
        let json = serde_json::json!({
            "model": "qwen-plus",
            "input": "What is 2+2?"
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_some());

        let req = request.unwrap();
        assert_eq!(req.model, "qwen-plus");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, MessageRole::User);
    }

    #[test]
    fn test_parse_request_responses_array_input() {
        let json = serde_json::json!({
            "model": "qwen-plus",
            "input": [
                {"role": "user", "content": "Hello"}
            ],
            "instructions": "You are a helpful assistant."
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_some());

        let req = request.unwrap();
        assert_eq!(req.model, "qwen-plus");
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, MessageRole::System);
        assert_eq!(req.messages[1].role, MessageRole::User);
    }

    #[test]
    fn test_parse_request_responses_input_text_format() {
        let json = serde_json::json!({
            "model": "gpt-4.1",
            "input": [
                {"type": "input_text", "text": "What is Rust?"}
            ]
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_some());

        let req = request.unwrap();
        assert_eq!(req.model, "gpt-4.1");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, MessageRole::User);
    }

    #[test]
    fn test_parse_request_responses_role_with_typed_content() {
        let json = serde_json::json!({
            "model": "gpt-4.1",
            "input": [
                {
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "Hello from typed array"}
                    ]
                }
            ]
        });

        let request = OpenAIParser::parse_request(&json);
        assert!(
            request.is_some(),
            "role item with typed array content must parse"
        );

        let req = request.unwrap();
        assert_eq!(req.model, "gpt-4.1");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, MessageRole::User);
    }

    #[test]
    fn test_parse_response_responses_format() {
        let json = serde_json::json!({
            "id": "resp_abc123",
            "object": "response",
            "status": "completed",
            "model": "qwen-plus",
            "output": [
                {
                    "type": "message",
                    "id": "msg_001",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "4", "annotations": []}]
                }
            ],
            "usage": {"input_tokens": 56, "output_tokens": 1, "total_tokens": 57}
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(resp.id, "resp_abc123");
        assert_eq!(resp.model, "qwen-plus");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].finish_reason, Some("stop".to_string()));
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 56);
        assert_eq!(usage.completion_tokens, 1);
    }

    #[test]
    fn test_parse_response_responses_tool_call() {
        let json = serde_json::json!({
            "id": "resp_tc001",
            "object": "response",
            "status": "completed",
            "model": "qwen-plus",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_001",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Beijing\"}",
                    "call_id": "call_xyz",
                    "status": "completed"
                }
            ],
            "usage": {"input_tokens": 100, "output_tokens": 20, "total_tokens": 120}
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
        let tc = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        let func = tc[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap().as_str().unwrap(), "get_weather");
        assert_eq!(
            func.get("arguments").unwrap().as_str().unwrap(),
            "{\"city\":\"Beijing\"}"
        );
    }

    #[test]
    fn test_aggregate_responses_sse_chunks_text() {
        let chunks = vec![
            serde_json::json!({"type": "response.created", "response": {"id": "resp_s001", "model": "qwen-plus", "status": "queued"}}),
            serde_json::json!({"type": "response.in_progress"}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "message", "id": "msg_001", "role": "assistant"}}),
            serde_json::json!({"type": "response.output_text.delta", "delta": "Hello"}),
            serde_json::json!({"type": "response.output_text.delta", "delta": " world"}),
            serde_json::json!({"type": "response.output_text.done", "text": "Hello world"}),
            serde_json::json!({"type": "response.completed", "response": {"id": "resp_s001", "model": "qwen-plus", "status": "completed", "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}}),
        ];

        let body = serde_json::Value::Array(chunks);
        let response = OpenAIParser::parse_response(&body);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(resp.id, "resp_s001");
        assert_eq!(resp.model, "qwen-plus");
        let content = resp.choices[0].message.content.as_ref().unwrap();
        match content {
            OpenAIContent::Text(t) => assert_eq!(t, "Hello world"),
            _ => panic!("expected text content"),
        }
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 2);
    }

    #[test]
    fn test_parse_response_responses_real_format() {
        let json = serde_json::json!({
            "background": false,
            "completed_at": 1780560263,
            "created_at": 1780560263,
            "frequency_penalty": 0.0,
            "id": "resp_d3352584-cb0f-98e7-867d-cc6a30ac04dd",
            "metadata": {},
            "model": "qwen-plus",
            "object": "response",
            "output": [{
                "content": [{"annotations": [], "text": "4", "type": "output_text"}],
                "id": "msg_04e1ac3a-d566-4c31-9beb-f37ac425f1d4",
                "role": "assistant",
                "status": "completed",
                "type": "message"
            }],
            "parallel_tool_calls": true,
            "presence_penalty": 0.0,
            "service_tier": "default",
            "status": "completed",
            "store": true,
            "temperature": 1.0,
            "tool_choice": "auto",
            "tools": [],
            "top_logprobs": 0,
            "top_p": 1.0,
            "usage": {
                "input_tokens": 57,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 1,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 58,
                "x_details": []
            }
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(
            response.is_some(),
            "Failed to parse real-format Responses API response"
        );
        let resp = response.unwrap();
        assert_eq!(resp.model, "qwen-plus");
        assert_eq!(resp.id, "resp_d3352584-cb0f-98e7-867d-cc6a30ac04dd");
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 57);
        assert_eq!(usage.completion_tokens, 1);
        assert_eq!(usage.total_tokens, 58);
    }

    #[test]
    fn test_parse_response_responses_mixed_text_and_tool_call() {
        let json = serde_json::json!({
            "id": "resp_mix001",
            "object": "response",
            "status": "completed",
            "model": "qwen-plus",
            "output": [
                {
                    "type": "message",
                    "id": "msg_001",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "Let me check that for you."}]
                },
                {
                    "type": "function_call",
                    "id": "fc_001",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Beijing\"}",
                    "call_id": "call_mix",
                    "status": "completed"
                }
            ],
            "usage": {"input_tokens": 80, "output_tokens": 15, "total_tokens": 95}
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_some());

        let resp = response.unwrap();
        // finish_reason must be "tool_calls" even when text is present
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
        let tc = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        // text content should also be preserved
        match &resp.choices[0].message.content {
            Some(OpenAIContent::Text(t)) => assert_eq!(t, "Let me check that for you."),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn test_aggregate_responses_sse_truncated_no_done() {
        // Simulate a truncated stream: output_item.added + argument deltas but NO done event
        let chunks = vec![
            serde_json::json!({"type": "response.created", "response": {"id": "resp_trunc", "model": "qwen-plus"}}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "function_call", "name": "search", "call_id": "call_trunc"}}),
            serde_json::json!({"type": "response.function_call_arguments.delta", "delta": "{\"q\":\"test"}),
            serde_json::json!({"type": "response.function_call_arguments.delta", "delta": "\"}"}),
            // NO response.function_call_arguments.done event — stream was cut
            serde_json::json!({"type": "response.completed", "response": {"id": "resp_trunc", "model": "qwen-plus", "status": "completed", "usage": {"input_tokens": 30, "output_tokens": 5, "total_tokens": 35}}}),
        ];

        let body = serde_json::Value::Array(chunks);
        let response = OpenAIParser::parse_response(&body);
        assert!(response.is_some());

        let resp = response.unwrap();
        // Tool call should still be captured via post-loop flush
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
        let tc = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].get("id").unwrap().as_str().unwrap(), "call_trunc");
        let func = tc[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap().as_str().unwrap(), "search");
        assert_eq!(
            func.get("arguments").unwrap().as_str().unwrap(),
            "{\"q\":\"test\"}"
        );
    }

    #[test]
    fn test_aggregate_responses_sse_chunks_tool_call() {
        let chunks = vec![
            serde_json::json!({"type": "response.created", "response": {"id": "resp_t001", "model": "qwen-plus"}}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "function_call", "name": "get_weather", "call_id": "call_001"}}),
            serde_json::json!({"type": "response.function_call_arguments.delta", "delta": "{\"city\""}),
            serde_json::json!({"type": "response.function_call_arguments.delta", "delta": ":\"Beijing\"}"}),
            serde_json::json!({"type": "response.function_call_arguments.done", "arguments": "{\"city\":\"Beijing\"}"}),
            serde_json::json!({"type": "response.completed", "response": {"id": "resp_t001", "model": "qwen-plus", "status": "completed", "usage": {"input_tokens": 50, "output_tokens": 10, "total_tokens": 60}}}),
        ];

        let body = serde_json::Value::Array(chunks);
        let response = OpenAIParser::parse_response(&body);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
        let tc = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].get("id").unwrap().as_str().unwrap(), "call_001");
        let func = tc[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap().as_str().unwrap(), "get_weather");
        assert_eq!(
            func.get("arguments").unwrap().as_str().unwrap(),
            "{\"city\":\"Beijing\"}"
        );
    }

    /// Regression: deepseek models on DashScope repeat `id:""` + `name:""` on every
    /// continuation delta. The empty name used to overwrite the real one, so the
    /// aggregated tool call ended up with an empty name.
    #[test]
    fn test_aggregate_sse_chunks_empty_name_in_continuation() {
        let chunk = |tc: serde_json::Value, finish: Option<&str>| {
            serde_json::json!({
                "id": "chatcmpl-1",
                "object": "chat.completion.chunk",
                "created": 1_786_504_982u64,
                "model": "deepseek-v4-flash",
                "choices": [{"index": 0, "delta": {"tool_calls": [tc]}, "finish_reason": finish}]
            })
        };
        let chunks = vec![
            chunk(
                serde_json::json!({"index": 0, "id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": ""}}),
                None,
            ),
            chunk(
                serde_json::json!({"index": 0, "id": "", "type": "function", "function": {"name": "", "arguments": "{\"file_path\""}}),
                None,
            ),
            chunk(
                serde_json::json!({"index": 0, "id": "", "type": "function", "function": {"name": "", "arguments": ": \"/tmp/a.md\"}"}}),
                Some("tool_calls"),
            ),
        ];

        let body = serde_json::Value::Array(chunks);
        let resp = OpenAIParser::parse_response(&body).expect("chat SSE chunks should aggregate");
        assert_eq!(
            resp.choices[0].finish_reason,
            Some("tool_calls".to_string())
        );
        let tc = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].get("id").unwrap().as_str().unwrap(), "call_1");
        let func = tc[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap().as_str().unwrap(), "read_file");
        assert_eq!(
            func.get("arguments").unwrap().as_str().unwrap(),
            "{\"file_path\": \"/tmp/a.md\"}"
        );
    }
}
