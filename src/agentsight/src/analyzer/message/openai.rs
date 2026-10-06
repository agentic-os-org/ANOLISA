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

/// Upper bound on tool-call slots reconstructed from one streamed response.
///
/// `index` comes off the wire and slots are allocated up to that value, so the
/// same bound the semantic builder applies to the DashScope native envelope
/// keeps a malformed or hostile value from selecting an unrelated slot.
const MAX_TOOL_CALL_SLOTS: u64 = 256;

/// Bounded per-item state shared by live parsing and drain enrichment.
///
/// Responses argument events identify their output item, so a new call must
/// not flush a previous call that can still receive deltas. Unidentified
/// compatible-provider events retain the sequential, most-recent-call fallback.
#[derive(Default)]
pub(crate) struct ResponsesToolCalls {
    calls: Vec<ResponseToolCall>,
    current: Option<usize>,
}

struct ResponseToolCall {
    index: Option<u64>,
    item_id: Option<String>,
    id: String,
    name: String,
    arguments: String,
    done: bool,
}

impl ResponsesToolCalls {
    /// Apply a call lifecycle event without redirecting unknown item IDs.
    pub(crate) fn observe(&mut self, event: &serde_json::Value) {
        let kind = event.get("type").and_then(|v| v.as_str());
        let index = event.get("output_index").and_then(|v| v.as_u64());
        if event.get("output_index").is_some() && index.is_none() {
            return;
        }
        let added = kind == Some("response.output_item.added");
        let item = event.get("item");
        let item_id = if added {
            item.and_then(|i| i.get("id"))
        } else {
            event.get("item_id")
        }
        .and_then(|v| v.as_str());
        let identified = index.is_some() || item_id.is_some();
        let position = if identified {
            self.calls.iter().position(|call| {
                let matches = index.is_some_and(|i| call.index == Some(i))
                    || item_id.is_some_and(|id| call.item_id.as_deref() == Some(id));
                let conflict = index.zip(call.index).is_some_and(|(a, b)| a != b)
                    || item_id
                        .zip(call.item_id.as_deref())
                        .is_some_and(|(a, b)| a != b);
                matches && !conflict
            })
        } else {
            self.current
        };

        match kind {
            Some("response.output_item.added") => {
                let Some(item) = item else { return };
                if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                    return;
                }
                // A reused index with a different item ID is contradictory;
                // retaining both would make index-only deltas ambiguous.
                if identified
                    && position.is_none()
                    && self.calls.iter().any(|call| {
                        index.is_some_and(|i| call.index == Some(i))
                            || item_id.is_some_and(|id| call.item_id.as_deref() == Some(id))
                    })
                {
                    return;
                }
                // Repeated identified add events must not duplicate the call.
                if identified && position.is_some() {
                    self.current = position;
                    return;
                }
                if self.calls.len() >= MAX_TOOL_CALL_SLOTS as usize {
                    self.current = None;
                    return;
                }
                self.calls.push(ResponseToolCall {
                    index,
                    item_id: item_id.map(str::to_owned),
                    id: item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    name: item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    arguments: item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    done: false,
                });
                self.current = Some(self.calls.len() - 1);
            }
            Some("response.function_call_arguments.delta") => {
                if let Some(call) = position.and_then(|p| self.calls.get_mut(p)) {
                    if !call.done {
                        if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                            call.arguments.push_str(delta);
                        }
                    }
                }
            }
            Some("response.function_call_arguments.done") => {
                if let Some(call) = position.and_then(|p| self.calls.get_mut(p)) {
                    // A full done payload supersedes partial captured deltas.
                    if let Some(arguments) = event.get("arguments").and_then(|v| v.as_str()) {
                        call.arguments = arguments.to_owned();
                    }
                    call.done = true;
                }
                if !identified {
                    self.current = None;
                }
            }
            _ => {}
        }
    }

    /// Emit every known call once, in output-item arrival order.
    pub(crate) fn into_calls(self) -> impl Iterator<Item = (String, String, String)> {
        self.calls
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .map(|call| (call.id, call.name, call.arguments))
    }
}

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

        // Modern chat clients send the output cap as `max_completion_tokens`
        // (the o-series accepts only that spelling). Copy it onto the legacy
        // key the typed request reads — an explicit `max_tokens` always wins —
        // the same way `normalize_responses_request` maps `max_output_tokens`.
        // Only a value the typed field can hold is copied: a malformed or
        // out-of-range value used to be ignored as an unknown key, and it must
        // not turn the whole request into a parse failure. A serde alias would
        // instead reject a request carrying both spellings as a duplicate
        // field, losing the request.
        let mut body = body.clone();
        if body.get("max_tokens").is_none() {
            if let Some(cap) = body
                .get("max_completion_tokens")
                .and_then(|cap| cap.as_u64())
                .and_then(|cap| u32::try_from(cap).ok())
            {
                body["max_tokens"] = serde_json::json!(cap);
            }
        }

        match serde_json::from_value::<OpenAIRequest>(body) {
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
        // The Responses API spells the output cap `max_output_tokens`; the
        // normalized chat view must carry it into `max_tokens` so the
        // downstream consumers of the chat shape (token-limit interruption
        // rules, telemetry) see the cap.
        if body.get("max_tokens").is_none() {
            if let Some(max_output_tokens) = body.get("max_output_tokens") {
                normalized["max_tokens"] = max_output_tokens.clone();
            }
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
        let mut reasoning_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        let mut finish_reason = Some("stop".to_string());
        // A capped response ends with status="incomplete" and the cap reason
        // in incomplete_details. Surface that as the chat-completions
        // "length" finish so a truncated answer is not reported as a clean
        // completion (the interruption detector's token-limit rules key on
        // exactly that spelling).
        let output_capped = body.get("status").and_then(|v| v.as_str()) == Some("incomplete")
            && body
                .pointer("/incomplete_details/reason")
                .and_then(|v| v.as_str())
                == Some("max_output_tokens");

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
                // Reasoning items carry their text as `content` blocks
                // (dashscope reasoning_text) and/or `summary` blocks (the
                // o-series default when the thinking itself is not returned).
                // Accept blocks with a "text" field regardless of type, like
                // the request-side content reader.
                "reasoning" => {
                    for key in ["content", "summary"] {
                        if let Some(blocks) = item.get(key).and_then(|c| c.as_array()) {
                            for part in blocks {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    reasoning_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let message_content = content_parts.join("");
        let mut message = serde_json::json!({
            "role": "assistant",
            "content": if message_content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(message_content) },
        });
        let reasoning_content = reasoning_parts.join("");
        if !reasoning_content.is_empty() {
            message["reasoning_content"] = serde_json::Value::String(reasoning_content);
        }
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
                "finish_reason": if output_capped {
                    Some("length".to_string())
                } else {
                    finish_reason
                },
            }],
            "usage": usage_val,
        });

        serde_json::from_value::<OpenAIResponse>(resp).ok()
    }

    fn aggregate_responses_sse_chunks(chunks: &[serde_json::Value]) -> Option<OpenAIResponse> {
        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut calls = ResponsesToolCalls::default();
        let mut model = String::new();
        let mut resp_id = String::new();
        let mut usage: Option<serde_json::Value> = None;
        // Set by the terminal `response.incomplete` event when the stream
        // was cut by the output cap.
        let mut output_capped = false;

        for chunk in chunks {
            calls.observe(chunk);
            let event_type = chunk.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match event_type {
                "response.output_text.delta" => {
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        content_buf.push_str(delta);
                    }
                }
                // Reasoning models stream their thinking as text deltas on
                // the same event channel (qwen3-coder via dashscope sends
                // reasoning_text, the o-series summary_text); both belong in
                // the chat view's reasoning_content like the chat-completions
                // reasoning_content delta.
                "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        reasoning_buf.push_str(delta);
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
                // A capped stream terminates with response.incomplete
                // instead of response.completed: the terminal event carries
                // the final usage, and the cap reason must surface as the
                // chat-completions "length" finish rather than a clean
                // "stop".
                "response.incomplete" => {
                    if let Some(resp) = chunk.get("response") {
                        model = resp
                            .get("model")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(id) = resp.get("id").and_then(|i| i.as_str()) {
                            if !id.is_empty() {
                                resp_id = id.to_string();
                            }
                        }
                        usage = resp.get("usage").map(|u| {
                            serde_json::json!({
                                "prompt_tokens": u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                "completion_tokens": u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                                "total_tokens": u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                            })
                        });
                        if resp.get("status").and_then(|v| v.as_str()) == Some("incomplete")
                            && resp
                                .pointer("/incomplete_details/reason")
                                .and_then(|v| v.as_str())
                                == Some("max_output_tokens")
                        {
                            output_capped = true;
                        }
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
        let tool_calls: Vec<_> = calls
            .into_calls()
            .map(|(id, name, arguments)| {
                serde_json::json!({"id": id, "type": "function",
                "function": {"name": name, "arguments": arguments}})
            })
            .collect();

        let mut message = serde_json::json!({
            "role": "assistant",
            "content": if content_buf.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(content_buf) },
        });
        if !reasoning_buf.is_empty() {
            message["reasoning_content"] = serde_json::Value::String(reasoning_buf);
        }
        let finish_reason = if output_capped {
            // The cap ended the stream: report the truncation even when a
            // tool call was in flight (its arguments may be cut mid-JSON,
            // so a normal "tool_calls" terminal would overstate the turn).
            "length"
        } else if !tool_calls.is_empty() {
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
                            // `index` comes off the wire. A value outside the
                            // slot range must not be truncated into another
                            // slot, which would overwrite a valid tool call's
                            // id, name and arguments.
                            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                            if idx >= MAX_TOOL_CALL_SLOTS {
                                log::debug!(
                                    "[OpenAI] dropping SSE tool_call with out-of-range index {idx}"
                                );
                                continue;
                            }
                            let idx = idx as u32;
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
            // The Responses API's per-id sub-endpoints (GET retrieve, POST
            // cancel, DELETE) share the /v1/responses prefix but are not
            // inference calls: the retrieval response IS the stored
            // response object (object=="response" + output[] + usage), so
            // deep-parsing a poll would re-record the create call's output
            // and re-count its usage tokens as a second llm_call. Must
            // stay in lockstep with parser::llm::is_llm_api_path.
            || (path.contains("/v1/responses")
                && !path.contains("/v1/responses/"))
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

    /// Modern chat clients send the output cap as `max_completion_tokens`
    /// (the o-series accepts only that spelling); the typed request used to
    /// drop it, so `LLMRequest.max_tokens` stayed `None` and neither the
    /// TokenLimit rule nor the `gen_ai.request.max_tokens` telemetry saw it.
    #[test]
    fn test_parse_request_reads_max_completion_tokens() {
        let json = serde_json::json!({
            "model": "o3",
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 2048
        });

        let request = OpenAIParser::parse_request(&json).expect("modern chat request");
        assert_eq!(request.max_tokens, Some(2048));
    }

    /// A request carrying both spellings must still parse, with `max_tokens`
    /// winning — a serde alias would reject it as a duplicate field and lose
    /// the whole request.
    #[test]
    fn test_parse_request_prefers_max_tokens_over_max_completion_tokens() {
        let json = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100,
            "max_completion_tokens": 2048
        });

        let request = OpenAIParser::parse_request(&json).expect("both spellings must parse");
        assert_eq!(request.max_tokens, Some(100));
    }

    /// A malformed cap used to be ignored as an unknown key; reading it must
    /// not turn the whole request into a parse failure.
    #[test]
    fn test_parse_request_ignores_a_malformed_max_completion_tokens() {
        let json = serde_json::json!({
            "model": "o3",
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": "2048"
        });

        let request = OpenAIParser::parse_request(&json).expect("request must still parse");
        assert_eq!(request.max_tokens, None);
        assert_eq!(request.messages.len(), 1);
    }

    /// A cap that does not fit the typed u32 field must be ignored like any
    /// other malformed value, not copied over and rejected by serde.
    #[test]
    fn test_parse_request_ignores_an_out_of_range_max_completion_tokens() {
        let json = serde_json::json!({
            "model": "o3",
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 4_294_967_296u64
        });

        let request = OpenAIParser::parse_request(&json).expect("request must still parse");
        assert_eq!(request.max_tokens, None);
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
    fn test_matches_path_rejects_responses_sub_endpoints() {
        // GET /v1/responses/{id} (retrieve), POST /v1/responses/{id}/cancel
        // and DELETE /v1/responses/{id} share the /v1/responses prefix but
        // are not inference calls: the retrieval response IS the stored
        // response object (object=="response" + output[] + usage), so
        // deep-parsing a poll would re-record the create call's output and
        // re-count its usage tokens as a second llm_call.
        assert!(!OpenAIParser::matches_path("/v1/responses/resp_abc123"));
        assert!(!OpenAIParser::matches_path(
            "https://api.openai.com/v1/responses/resp_abc123"
        ));
        assert!(!OpenAIParser::matches_path(
            "/v1/responses/resp_abc123/cancel"
        ));
        // The create endpoint keeps matching, in both bare-path and
        // full-URL shapes.
        assert!(OpenAIParser::matches_path("/v1/responses"));
        assert!(OpenAIParser::matches_path(
            "https://api.openai.com/v1/responses"
        ));
        assert!(OpenAIParser::matches_path(
            "https://dashscope.aliyuncs.com/compatible-mode/v1/responses"
        ));
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

    /// Reasoning models stream their thinking as `response.reasoning_text.delta`
    /// / `response.reasoning_summary_text.delta` events (qwen3-coder via
    /// dashscope `/v1/responses`, o-series via OpenAI). The aggregator matched
    /// neither, so a reasoning Responses stream kept only its final text — the
    /// chat-completions and Anthropic paths both carry reasoning, and the
    /// token extractor and the latency marker already count these deltas.
    #[test]
    fn test_aggregate_responses_sse_chunks_reasoning() {
        let chunks = vec![
            serde_json::json!({"type": "response.created", "response": {"id": "resp_r001", "model": "qwen3-coder-plus", "status": "queued"}}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "reasoning", "id": "rs_001"}}),
            serde_json::json!({"type": "response.reasoning_text.delta", "delta": "Think "}),
            serde_json::json!({"type": "response.reasoning_text.delta", "delta": "step by step"}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "message", "id": "msg_001", "role": "assistant"}}),
            serde_json::json!({"type": "response.output_text.delta", "delta": "The answer"}),
            serde_json::json!({"type": "response.completed", "response": {"id": "resp_r001", "model": "qwen3-coder-plus", "status": "completed", "usage": {"input_tokens": 10, "output_tokens": 8, "total_tokens": 18}}}),
        ];

        let body = serde_json::Value::Array(chunks);
        let response = OpenAIParser::parse_response(&body);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("Think step by step"),
            "reasoning deltas must concatenate into reasoning_content"
        );
        let content = resp.choices[0].message.content.as_ref().unwrap();
        match content {
            OpenAIContent::Text(t) => assert_eq!(t, "The answer"),
            _ => panic!("expected text content"),
        }
    }

    /// A summary-only reasoning stream (o-series default: the thinking itself
    /// is not returned, only its summary) must reach the same field.
    #[test]
    fn test_aggregate_responses_sse_chunks_reasoning_summary() {
        let chunks = vec![
            serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "concise plan"}),
            serde_json::json!({"type": "response.output_text.delta", "delta": "done"}),
            serde_json::json!({"type": "response.completed", "response": {"id": "resp_r002", "model": "o-series", "status": "completed"}}),
        ];

        let body = serde_json::Value::Array(chunks);
        let resp = OpenAIParser::parse_response(&body).expect("response");
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("concise plan")
        );
    }

    #[test]
    fn test_parse_response_responses_reasoning_item() {
        // Non-streaming counterpart: the reasoning arrives as an output item
        // with summary (OpenAI) or content (dashscope) text blocks, which the
        // normalizer skipped while it copied message and function_call items.
        let json = serde_json::json!({
            "id": "resp_r101",
            "object": "response",
            "status": "completed",
            "model": "qwen3-coder-plus",
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_101",
                    "summary": [{"type": "summary_text", "text": "pondered"}],
                    "content": [{"type": "reasoning_text", "text": "Think "}]
                },
                {
                    "type": "message",
                    "id": "msg_101",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "42"}]
                }
            ],
            "usage": {"input_tokens": 30, "output_tokens": 12, "total_tokens": 42}
        });

        let resp = OpenAIParser::parse_response(&json).expect("response");
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("Think pondered"),
            "content reasoning text comes first, then the summary"
        );
        let content = resp.choices[0].message.content.as_ref().unwrap();
        match content {
            OpenAIContent::Text(t) => assert_eq!(t, "42"),
            _ => panic!("expected text content"),
        }
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
    fn test_parse_response_responses_incomplete_max_output_tokens() {
        // A Responses API call that hit its output cap ends with
        // status="incomplete" and incomplete_details.reason="max_output_tokens".
        // The normalized chat view must surface that as finish_reason
        // "length" instead of a normal "stop", or the interruption
        // detector reports a capped answer as a clean completion.
        let json = serde_json::json!({
            "id": "resp_cap001",
            "object": "response",
            "created_at": 1780560263,
            "model": "qwen-plus",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{
                "content": [{"text": "partial answer", "type": "output_text"}],
                "id": "msg_cap001",
                "role": "assistant",
                "status": "incomplete",
                "type": "message"
            }],
            "usage": {"input_tokens": 57, "output_tokens": 1024, "total_tokens": 1081}
        });

        let response = OpenAIParser::parse_response(&json);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(resp.choices[0].finish_reason, Some("length".to_string()));
        match &resp.choices[0].message.content {
            Some(OpenAIContent::Text(t)) => assert_eq!(t, "partial answer"),
            _ => panic!("expected text content"),
        }
        let usage = resp.usage.unwrap();
        assert_eq!(usage.completion_tokens, 1024);
    }

    #[test]
    fn test_aggregate_responses_sse_chunks_incomplete() {
        // A capped Responses stream terminates with response.incomplete —
        // not response.completed. That terminal event carries the final
        // usage, and the aggregated view must report finish_reason
        // "length" with that usage instead of a clean "stop" with none.
        let chunks = vec![
            serde_json::json!({"type": "response.created", "response": {"id": "resp_cap_sse", "model": "qwen-plus", "status": "queued"}}),
            serde_json::json!({"type": "response.in_progress"}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "message", "id": "msg_001", "role": "assistant"}}),
            serde_json::json!({"type": "response.output_text.delta", "delta": "partial"}),
            serde_json::json!({"type": "response.incomplete", "response": {
                "id": "resp_cap_sse",
                "model": "qwen-plus",
                "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
                "usage": {"input_tokens": 50, "output_tokens": 1024, "total_tokens": 1074}
            }}),
        ];

        let body = serde_json::Value::Array(chunks);
        let response = OpenAIParser::parse_response(&body);
        assert!(response.is_some());

        let resp = response.unwrap();
        assert_eq!(resp.id, "resp_cap_sse");
        assert_eq!(resp.choices[0].finish_reason, Some("length".to_string()));
        let content = resp.choices[0].message.content.as_ref().unwrap();
        match content {
            OpenAIContent::Text(t) => assert_eq!(t, "partial"),
            _ => panic!("expected text content"),
        }
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 50);
        assert_eq!(usage.completion_tokens, 1024);
    }

    #[test]
    fn test_parse_request_responses_max_output_tokens() {
        // The Responses API spells the output cap max_output_tokens; the
        // normalized chat view must carry it into max_tokens so downstream
        // consumers (token-limit interruption rules, telemetry) see the cap.
        let json = serde_json::json!({
            "model": "gpt-5",
            "input": "Hello",
            "max_output_tokens": 512
        });
        let request = OpenAIParser::parse_request(&json);
        assert!(request.is_some());
        assert_eq!(request.unwrap().max_tokens, Some(512));
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
    fn test_aggregate_responses_sse_two_calls_without_done() {
        // Two function calls in flight with no
        // `response.function_call_arguments.done` (the truncated shape the
        // post-loop flush exists for). Starting the second call used to clear
        // the first one's name/id/arguments, so only the last call survived.
        let chunks = vec![
            serde_json::json!({"type": "response.created", "response": {"id": "resp_par", "model": "gpt-5"}}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "function_call", "name": "get_weather", "call_id": "call_1"}}),
            serde_json::json!({"type": "response.function_call_arguments.delta", "delta": "{\"city\":\"Beijing\"}"}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "function_call", "name": "get_time", "call_id": "call_2"}}),
            serde_json::json!({"type": "response.function_call_arguments.delta", "delta": "{\"zone\":\"UTC\"}"}),
            serde_json::json!({"type": "response.completed", "response": {"id": "resp_par", "model": "gpt-5", "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}}}),
        ];

        let body = serde_json::Value::Array(chunks);
        let resp = OpenAIParser::parse_response(&body).expect("should aggregate");

        let tc = resp.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("both calls must be reported");
        assert_eq!(
            tc.len(),
            2,
            "a second in-flight call must not discard the first: {tc:?}"
        );
        assert_eq!(tc[0].get("id").unwrap().as_str().unwrap(), "call_1");
        assert_eq!(
            tc[0]
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str()
                .unwrap(),
            "get_weather"
        );
        assert_eq!(
            tc[0]
                .get("function")
                .unwrap()
                .get("arguments")
                .unwrap()
                .as_str()
                .unwrap(),
            "{\"city\":\"Beijing\"}"
        );
        assert_eq!(tc[1].get("id").unwrap().as_str().unwrap(), "call_2");
        assert_eq!(
            tc[1]
                .get("function")
                .unwrap()
                .get("arguments")
                .unwrap()
                .as_str()
                .unwrap(),
            "{\"zone\":\"UTC\"}"
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

    #[test]
    fn test_aggregate_responses_sse_chunks_tool_call_from_done_event() {
        // The done event carries the complete arguments. A capture that
        // missed the deltas (stream joined late, events dropped) must not
        // record an empty argument list.
        let chunks = vec![
            serde_json::json!({"type": "response.created", "response": {"id": "resp_d01", "model": "qwen-plus"}}),
            serde_json::json!({"type": "response.output_item.added", "item": {"type": "function_call", "name": "get_weather", "call_id": "call_d01"}}),
            serde_json::json!({"type": "response.function_call_arguments.done", "arguments": "{\"city\":\"Beijing\"}"}),
            serde_json::json!({"type": "response.completed", "response": {"id": "resp_d01", "model": "qwen-plus", "status": "completed", "usage": {"input_tokens": 50, "output_tokens": 10, "total_tokens": 60}}}),
        ];

        let body = serde_json::Value::Array(chunks);
        let resp = OpenAIParser::parse_response(&body).expect("response parses");
        let tc = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(
            tc[0]
                .get("function")
                .unwrap()
                .get("arguments")
                .unwrap()
                .as_str()
                .unwrap(),
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

    /// `index` is wire input: a value outside the slot range must not be
    /// truncated into another slot, which would overwrite a valid tool call's
    /// id, name and arguments in the recorded message.
    #[test]
    fn test_aggregate_sse_chunks_rejects_absurd_tool_call_index() {
        let chunk = |tc: serde_json::Value| {
            serde_json::json!({
                "id": "chatcmpl-1",
                "object": "chat.completion.chunk",
                "created": 1_786_504_982u64,
                "model": "gpt-4o",
                "choices": [{"index": 0, "delta": {"tool_calls": [tc]}, "finish_reason": null}]
            })
        };
        let chunks = vec![
            chunk(
                serde_json::json!({"index": 0, "id": "call_a", "type": "function", "function": {"name": "alpha", "arguments": "{\"x\":1}"}}),
            ),
            chunk(
                serde_json::json!({"index": 4294967296u64, "id": "call_b", "type": "function", "function": {"name": "beta", "arguments": "{\"y\":2}"}}),
            ),
        ];

        let resp = OpenAIParser::parse_response(&serde_json::Value::Array(chunks))
            .expect("chat SSE chunks should aggregate");
        let tc = resp.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("tool calls");

        assert_eq!(
            tc.len(),
            1,
            "the out-of-range index must not be merged into slot 0: {tc:?}"
        );
        assert_eq!(tc[0].get("id").unwrap().as_str().unwrap(), "call_a");
        let func = tc[0].get("function").unwrap();
        assert_eq!(func.get("name").unwrap().as_str().unwrap(), "alpha");
        assert_eq!(
            func.get("arguments").unwrap().as_str().unwrap(),
            "{\"x\":1}"
        );
    }
}
