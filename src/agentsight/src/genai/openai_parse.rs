//! GenAI OpenAI body / SSE parsing & message conversion helpers
//!
//! Pure static helpers for parsing OpenAI request/response bodies and
//! converting between `OpenAIChatMessage` and the parts-based message
//! representation. Logic preserved verbatim from the original `builder.rs`; only
//! visibility was widened to `pub(super)` so siblings (`call_builder`)
//! can call these.

use super::GenAIBuilder;
use super::semantic::{InputMessage, LLMRequest, MessagePart, OutputMessage};
use crate::analyzer::message::types::OpenAIChatMessage;
use crate::analyzer::{HttpRecord, ParsedApiMessage};
use std::collections::HashMap;

/// Upper bound on tool-call slots reconstructed from one response.
///
/// The DashScope native envelope addresses tool calls by a wire-supplied
/// `index`, and slots are allocated up to that index. Real responses use small
/// contiguous indices; the cap keeps a malformed or hostile value from turning
/// a single JSON field into an unbounded allocation.
const MAX_TOOL_CALL_SLOTS: u64 = 256;

impl GenAIBuilder {
    /// 从 HTTP request body 直接解析 LLMRequest（OpenAI/Anthropic 格式）
    pub(super) fn parse_request_body(body: &str) -> Option<LLMRequest> {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        let obj = v.as_object()?;

        // Normalized view: "messages" (chat completions) or "input" + "instructions"
        // (Responses API used by codex 0.137+ via dashscope /v1/responses).
        let (raw_messages, instructions_text) = Self::extract_messages_view(&v)?;

        let mut messages: Vec<InputMessage> = Vec::new();

        // Responses API: prepend top-level "instructions" as a synthetic system message
        if let Some(instr) = instructions_text {
            if !instr.is_empty() {
                messages.push(InputMessage {
                    role: "system".to_string(),
                    parts: vec![MessagePart::Text { content: instr }],
                    name: None,
                });
            }
        }

        for msg in &raw_messages {
            let Some(role) = msg.get("role").and_then(|v| v.as_str()).map(String::from) else {
                continue;
            };
            let mut parts = Vec::new();

            // content can be string or array of blocks. Accept blocks with a
            // "text" field regardless of type (handles "text" / "input_text" /
            // "output_text" alike).
            if let Some(content) = msg.get("content") {
                if let Some(s) = content.as_str() {
                    if !s.is_empty() {
                        parts.push(MessagePart::Text {
                            content: s.to_string(),
                        });
                    }
                } else if let Some(arr) = content.as_array() {
                    for item in arr {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            parts.push(MessagePart::Text {
                                content: text.to_string(),
                            });
                        }
                    }
                }
            }

            // tool_call 结果 (role=tool)
            if role == "tool" {
                if let Some(content) = msg.get("content") {
                    let id = msg
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    parts = vec![MessagePart::ToolCallResponse {
                        id,
                        response: content.clone(),
                    }];
                }
            }

            // tool_calls (role=assistant 发起的 tool calls)
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    let id = tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let func = tc.get("function").unwrap_or(&serde_json::Value::Null);
                    let name = func
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = func.get("arguments").cloned();
                    parts.push(MessagePart::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }

            messages.push(InputMessage {
                role,
                parts,
                name: None,
            });
        }

        if messages.is_empty() {
            return None;
        }

        let tools = obj
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.to_vec());

        Some(LLMRequest {
            messages,
            temperature: obj.get("temperature").and_then(|v| v.as_f64()),
            max_tokens: obj
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            frequency_penalty: obj.get("frequency_penalty").and_then(|v| v.as_f64()),
            presence_penalty: obj.get("presence_penalty").and_then(|v| v.as_f64()),
            top_p: obj.get("top_p").and_then(|v| v.as_f64()),
            top_k: obj.get("top_k").and_then(|v| v.as_f64()),
            seed: obj.get("seed").and_then(|v| v.as_i64()),
            stop_sequences: obj.get("stop").and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
            }),
            stream: obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
            tools,
            raw_body: Some(body.to_string()),
        })
    }

    /// Extract the LLM API response ID from parsed message or SSE body.
    ///
    /// Priority:
    /// 1. ParsedApiMessage response.id (OpenAI / Anthropic)
    /// 2. SSE response body first chunk "id" field
    /// 3. None (caller should fall back to call_id)
    pub(super) fn extract_response_id(
        parsed_message: &Option<ParsedApiMessage>,
        http: &HttpRecord,
    ) -> Option<String> {
        // 1. Try parsed message response.id
        if let Some(msg) = parsed_message {
            match msg {
                ParsedApiMessage::OpenAICompletion {
                    response: Some(resp),
                    ..
                } if !resp.id.is_empty() => {
                    return Some(resp.id.clone());
                }
                ParsedApiMessage::AnthropicMessage {
                    response: Some(resp),
                    ..
                } if !resp.id.is_empty() => {
                    return Some(resp.id.clone());
                }
                _ => {}
            }
        }

        // 2. SSE fallback: extract "id" field from response body
        if http.is_sse {
            if let Some(ref body) = http.response_body {
                // Try JSON array format first (from HTTP/2 stream aggregation)
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                    if let Some(arr) = v.as_array() {
                        for chunk in arr {
                            if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
                                if !id.is_empty() {
                                    return Some(id.to_string());
                                }
                            }
                        }
                    }
                }
                // Try SSE line format (from HTTP/1.1: "data: {...}" per line)
                for line in body.lines() {
                    let json_str = line.strip_prefix("data: ").unwrap_or(line).trim();
                    if json_str.is_empty() || json_str == "[DONE]" {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(id) = v.get("id").and_then(|v| v.as_str()) {
                            if !id.is_empty() {
                                return Some(id.to_string());
                            }
                        }
                    }
                }
            }
        }

        None
    }

    /// Convert OpenAI ChatMessage to parts-based InputMessage
    pub(super) fn openai_msg_to_input(m: &OpenAIChatMessage) -> InputMessage {
        let role = format!("{:?}", m.role).to_lowercase();
        let mut parts = Vec::new();

        // Reasoning content first
        if let Some(ref rc) = m.reasoning_content {
            if !rc.is_empty() {
                parts.push(MessagePart::Reasoning {
                    content: rc.clone(),
                });
            }
        }

        // For tool role: content is tool_call_response
        if role == "tool" {
            let response_val = m
                .content
                .as_ref()
                .map(|c| {
                    let text = c.as_text();
                    // Try to parse as JSON, fall back to string
                    serde_json::from_str::<serde_json::Value>(&text)
                        .unwrap_or_else(|_| serde_json::Value::String(text))
                })
                .unwrap_or(serde_json::Value::Null);
            parts.push(MessagePart::ToolCallResponse {
                id: m.tool_call_id.clone(),
                response: response_val,
            });
        } else {
            // Text content
            if let Some(ref c) = m.content {
                let text = c.as_text();
                if !text.is_empty() {
                    parts.push(MessagePart::Text { content: text });
                }
            }
        }

        // Tool calls
        if let Some(ref tcs) = m.tool_calls {
            for tc in tcs {
                if let Some(part) = Self::parse_openai_tool_call_value(tc) {
                    parts.push(part);
                }
            }
        }

        InputMessage {
            role,
            parts,
            name: m.name.clone(),
        }
    }

    /// Convert OpenAI ChatMessage to parts-based OutputMessage
    pub(super) fn openai_msg_to_output(
        m: &OpenAIChatMessage,
        finish_reason: Option<&str>,
    ) -> OutputMessage {
        let role = format!("{:?}", m.role).to_lowercase();
        let mut parts = Vec::new();

        // Reasoning content first
        if let Some(ref rc) = m.reasoning_content {
            if !rc.is_empty() {
                parts.push(MessagePart::Reasoning {
                    content: rc.clone(),
                });
            }
        }

        // Text content
        if let Some(ref c) = m.content {
            let text = c.as_text();
            if !text.is_empty() {
                parts.push(MessagePart::Text { content: text });
            }
        }

        // Tool calls
        if let Some(ref tcs) = m.tool_calls {
            for tc in tcs {
                if let Some(part) = Self::parse_openai_tool_call_value(tc) {
                    parts.push(part);
                }
            }
        }

        OutputMessage {
            role,
            parts,
            name: m.name.clone(),
            finish_reason: finish_reason.map(|s| s.to_string()),
        }
    }

    /// Parse a serde_json::Value tool_call into MessagePart::ToolCall
    pub(super) fn parse_openai_tool_call_value(tc: &serde_json::Value) -> Option<MessagePart> {
        let func = tc.get("function")?;
        let name = func.get("name")?.as_str()?.to_string();
        let id = tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
        // Parse arguments as JSON object (not string)
        let arguments = func.get("arguments").and_then(|v| match v {
            serde_json::Value::String(s) => serde_json::from_str(s).ok(),
            other => Some(other.clone()),
        });
        Some(MessagePart::ToolCall {
            id,
            name,
            arguments,
        })
    }

    // NOTE: token_record_to_parts and parse_tool_call_strings removed.
    // Tool calls and reasoning are now extracted directly from SSE response body
    // via extract_parts_from_sse_body / parse_sse_response_body.

    /// Parse SSE response body (JSON array of chunks) into a complete OutputMessage.
    ///
    /// Merges content/reasoning deltas and tool_call argument fragments by index.
    /// Extracts finish_reason from the last SSE chunk that has one.
    pub(super) fn parse_sse_response_body(
        body: &str,
        fallback_finish_reason: Option<&str>,
    ) -> Option<Vec<OutputMessage>> {
        let (parts, sse_finish_reason) = Self::extract_parts_from_sse_body(body)?;
        if parts.is_empty() {
            return None;
        }
        // Prefer finish_reason from SSE, fall back to caller-supplied value
        let finish_reason =
            sse_finish_reason.or_else(|| fallback_finish_reason.map(|s| s.to_string()));
        Some(vec![OutputMessage {
            role: "assistant".to_string(),
            parts,
            name: None,
            finish_reason,
        }])
    }

    /// Extract MessageParts + finish_reason by aggregating all SSE chunks in response_body.
    ///
    /// Handles OpenAI SSE delta format:
    /// - content deltas → single Text part
    /// - reasoning_content deltas → single Reasoning part
    /// - tool_calls deltas (fragmented by index) → merged ToolCall parts
    /// - finish_reason from the last non-null value in choices
    ///
    /// Falls back to the DashScope/Bailian native envelope (`output.…`) when the
    /// OpenAI shape yields nothing. `body` may be a JSON array of SSE chunks or
    /// a single JSON object (non-streaming native response).
    ///
    /// Returns (parts, finish_reason) or None if no content found.
    pub(super) fn extract_parts_from_sse_body(
        body: &str,
    ) -> Option<(Vec<MessagePart>, Option<String>)> {
        let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
        let chunks: Vec<serde_json::Value> = match parsed {
            serde_json::Value::Array(chunks) => chunks,
            obj @ serde_json::Value::Object(_) => vec![obj],
            _ => return None,
        };

        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut finish_reason: Option<String> = None;
        // tool_call delta merging: index -> (id, name, arguments_accumulated)
        let mut tc_map: HashMap<u32, (String, String, String)> = HashMap::new();

        log::debug!("[GenAI] Parsing SSE body with {} chunks", chunks.len());

        for chunk in chunks.iter() {
            let choices = chunk.get("choices").and_then(|c| c.as_array());
            let choices = match choices {
                Some(c) => c,
                None => continue,
            };
            for choice in choices {
                let delta = match choice.get("delta") {
                    Some(d) => d,
                    None => continue,
                };
                // Content
                if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                    content_buf.push_str(c);
                }
                // Reasoning
                if let Some(r) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                    reasoning_buf.push_str(r);
                }
                // Tool call deltas — merge by index
                if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in calls {
                        let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let entry = tc_map
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
                // Finish reason — take the last non-null value
                if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                    finish_reason = Some(fr.to_string());
                }
            }
        }

        let mut parts = Vec::new();

        // Reasoning first
        if !reasoning_buf.is_empty() {
            parts.push(MessagePart::Reasoning {
                content: reasoning_buf,
            });
        }
        // Text content
        if !content_buf.is_empty() {
            parts.push(MessagePart::Text {
                content: content_buf,
            });
        }
        // Merged tool calls
        if !tc_map.is_empty() {
            let mut indices: Vec<u32> = tc_map.keys().cloned().collect();
            indices.sort();
            for idx in indices {
                if let Some((id, name, arguments)) = tc_map.remove(&idx) {
                    let parsed_args: Option<serde_json::Value> = if arguments.is_empty() {
                        None
                    } else {
                        serde_json::from_str(&arguments).ok()
                    };
                    parts.push(MessagePart::ToolCall {
                        id: if id.is_empty() { None } else { Some(id) },
                        name,
                        arguments: parsed_args,
                    });
                }
            }
        }

        if parts.is_empty() {
            return Self::extract_dashscope_native_parts(&chunks);
        }
        Some((parts, finish_reason))
    }

    /// Reconstruct assistant output from the DashScope/Bailian **native**
    /// envelope, which has no OpenAI-style `choices[].delta`.
    ///
    /// Handled shapes:
    /// - `output.text` (`result_format: text`, the protocol default)
    /// - `output.choices[].message.content` (`result_format: message`), where
    ///   `content` is either a string or multimodal blocks `[{"text": …}]`
    /// - `output.choices[].message.tool_calls` (arguments arrive whole, not
    ///   fragmented, and repeat on every chunk)
    ///
    /// Streaming defaults to `incremental_output=false`, i.e. every chunk
    /// repeats the **cumulative** text; with `incremental_output=true` each
    /// chunk carries only the delta. Both are supported by replacing the buffer
    /// when the new value extends it and appending otherwise — AgentSight is a
    /// passive observer and cannot see the request's `incremental_output` flag
    /// on the response path.
    fn extract_dashscope_native_parts(
        chunks: &[serde_json::Value],
    ) -> Option<(Vec<MessagePart>, Option<String>)> {
        /// Accumulate cumulative-or-incremental text into `buf`.
        fn accumulate(buf: &mut String, next: &str) {
            if next.is_empty() {
                return;
            }
            if next.starts_with(buf.as_str()) {
                *buf = next.to_string();
            } else {
                buf.push_str(next);
            }
        }

        /// Native `content` is a string or an array of `{"text": …}` blocks.
        fn content_text(content: &serde_json::Value) -> String {
            if let Some(s) = content.as_str() {
                return s.to_string();
            }
            content
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default()
        }

        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut finish_reason: Option<String> = None;
        // Native tool calls repeat in full on every chunk, so the last value
        // per index wins rather than being concatenated.
        let mut tool_calls: Vec<(String, String, String)> = Vec::new();
        let mut saw_native_envelope = false;

        for chunk in chunks {
            let Some(output) = chunk.get("output").filter(|o| o.is_object()) else {
                continue;
            };
            saw_native_envelope = true;

            if let Some(text) = output.get("text").and_then(|t| t.as_str()) {
                accumulate(&mut content_buf, text);
            }
            if let Some(fr) = Self::dashscope_terminal_finish_reason(output) {
                finish_reason = Some(fr);
            }

            for choice in output
                .get("choices")
                .and_then(|c| c.as_array())
                .map(|c| c.as_slice())
                .unwrap_or_default()
            {
                if let Some(fr) = Self::dashscope_terminal_finish_reason(choice) {
                    finish_reason = Some(fr);
                }
                let Some(message) = choice.get("message") else {
                    continue;
                };
                if let Some(content) = message.get("content") {
                    accumulate(&mut content_buf, &content_text(content));
                }
                if let Some(reasoning) = message.get("reasoning_content").and_then(|r| r.as_str()) {
                    accumulate(&mut reasoning_buf, reasoning);
                }
                for (idx, tc) in message
                    .get("tool_calls")
                    .and_then(|t| t.as_array())
                    .map(|t| t.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    let idx = tc
                        .get("index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(idx as u64);
                    // `index` comes off the wire, and the slot vector is grown
                    // to fit it. Bound it so a malformed or hostile response
                    // cannot turn one field into a multi-hundred-MB
                    // allocation inside a passive observer.
                    if idx >= MAX_TOOL_CALL_SLOTS {
                        log::debug!(
                            "[GenAI] dropping dashscope tool_call with out-of-range index {idx}"
                        );
                        continue;
                    }
                    let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                    let func = tc.get("function");
                    let name = func
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let args = func
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();

                    if tool_calls.len() <= idx as usize {
                        tool_calls.resize(idx as usize + 1, Default::default());
                    }
                    let entry = &mut tool_calls[idx as usize];
                    if !id.is_empty() {
                        entry.0 = id.to_string();
                    }
                    if !name.is_empty() {
                        entry.1 = name.to_string();
                    }
                    if !args.is_empty() {
                        entry.2 = args.to_string();
                    }
                }
            }
        }

        if !saw_native_envelope {
            return None;
        }

        let mut parts = Vec::new();
        if !reasoning_buf.is_empty() {
            parts.push(MessagePart::Reasoning {
                content: reasoning_buf,
            });
        }
        if !content_buf.is_empty() {
            parts.push(MessagePart::Text {
                content: content_buf,
            });
        }
        for (id, name, arguments) in tool_calls {
            if id.is_empty() && name.is_empty() {
                continue;
            }
            parts.push(MessagePart::ToolCall {
                id: if id.is_empty() { None } else { Some(id) },
                name,
                arguments: serde_json::from_str(&arguments).ok(),
            });
        }

        if parts.is_empty() {
            None
        } else {
            Some((parts, finish_reason))
        }
    }

    /// Read a DashScope native `finish_reason`, ignoring the in-progress
    /// placeholder (the *literal string* `"null"`) and empty values.
    fn dashscope_terminal_finish_reason(node: &serde_json::Value) -> Option<String> {
        node.get("finish_reason")
            .and_then(|r| r.as_str())
            .filter(|r| !r.is_empty() && *r != "null")
            .map(|r| r.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_request_body_openai() {
        let body = r#"{
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"}
            ],
            "temperature": 0.7,
            "max_tokens": 1024,
            "stream": true
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(1024));
        assert!(req.stream);
    }

    #[test]
    fn test_parse_request_body_with_tool_calls() {
        let body = r#"{
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": "", "tool_calls": [{"id": "tc_1", "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}}]},
                {"role": "tool", "tool_call_id": "tc_1", "content": "found 10 results"}
            ]
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.messages.len(), 2);
        // assistant message has ToolCall part
        let parts = &req.messages[0].parts;
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCall { name, .. } if name == "search"))
        );
        // tool message has ToolCallResponse part
        let tool_parts = &req.messages[1].parts;
        assert!(
            tool_parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCallResponse { .. }))
        );
    }

    #[test]
    fn test_parse_request_body_responses_api() {
        let body = r#"{
            "model": "gpt-4",
            "input": [{"role": "user", "content": "Hello"}],
            "instructions": "You are helpful.",
            "stream": true
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert!(req.stream);
    }

    #[test]
    fn test_parse_request_body_responses_api_empty_instructions() {
        let body = r#"{
            "model": "gpt-4",
            "input": [{"role": "user", "content": "Hello"}],
            "instructions": "",
            "stream": true
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        // Empty instructions should be skipped, so only the input message remains.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }

    #[test]
    fn test_parse_request_body_content_array() {
        let body = r#"{
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": [{"type": "input_text", "text": "Hello"}]}
            ]
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert!(
            matches!(&req.messages[0].parts[0], MessagePart::Text { content } if content == "Hello")
        );
    }

    #[test]
    fn test_parse_request_body_skips_missing_role() {
        let body = r#"{
            "model": "gpt-4",
            "messages": [
                {"content": "no role"},
                {"role": "user", "content": "Hello"}
            ]
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
    }

    #[test]
    fn test_parse_request_body_empty_messages() {
        let body = r#"{"model": "gpt-4", "messages": []}"#;
        assert!(GenAIBuilder::parse_request_body(body).is_none());
    }

    #[test]
    fn test_parse_request_body_invalid_json() {
        assert!(GenAIBuilder::parse_request_body("not json").is_none());
    }

    #[test]
    fn test_parse_openai_tool_call_value() {
        let tc = json!({
            "id": "call_abc",
            "function": {
                "name": "get_weather",
                "arguments": "{\"city\":\"Beijing\"}"
            }
        });
        let part = GenAIBuilder::parse_openai_tool_call_value(&tc).unwrap();
        match part {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.unwrap(), "call_abc");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments.unwrap()["city"], "Beijing");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_parse_openai_tool_call_value_no_function() {
        let tc = json!({"id": "call_abc"});
        assert!(GenAIBuilder::parse_openai_tool_call_value(&tc).is_none());
    }

    #[test]
    fn test_extract_parts_from_sse_body_content() {
        let body = r#"[{"choices":[{"delta":{"content":"Hello "}}]},{"choices":[{"delta":{"content":"world"},"finish_reason":"stop"}]}]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            MessagePart::Text { content } => assert_eq!(content, "Hello world"),
            _ => panic!("expected Text"),
        }
        assert_eq!(finish, Some("stop".to_string()));
    }

    #[test]
    fn test_extract_parts_from_sse_body_reasoning() {
        let body = r#"[{"choices":[{"delta":{"reasoning_content":"thinking..."}}]},{"choices":[{"delta":{"content":"answer"}}]}]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(
            matches!(&parts[0], MessagePart::Reasoning { content } if content == "thinking...")
        );
        assert!(matches!(&parts[1], MessagePart::Text { content } if content == "answer"));
    }

    #[test]
    fn test_extract_parts_from_sse_body_tool_calls() {
        let body = r#"[
            {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc_1","function":{"name":"search","arguments":""}}]}}]},
            {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\""}}]}}]},
            {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":": \"rust\"}"}}]},"finish_reason":"tool_calls"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("tc_1"));
                assert_eq!(name, "search");
                assert_eq!(arguments.as_ref().unwrap()["q"], "rust");
            }
            _ => panic!("expected ToolCall"),
        }
        assert_eq!(finish, Some("tool_calls".to_string()));
    }

    /// Regression: deepseek models on DashScope repeat `id:""` + `name:""` on every
    /// continuation delta. The empty name used to overwrite the real one, so reported
    /// tool calls ended up with an empty name.
    #[test]
    fn test_extract_parts_from_sse_body_tool_calls_empty_name_in_continuation() {
        let body = r#"[
            {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]}}]},
            {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","type":"function","function":{"name":"","arguments":"{\"file_path\""}}]}}]},
            {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","type":"function","function":{"name":"","arguments":": \"/tmp/a.md\"}"}}]}}]},
            {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_2","type":"function","function":{"name":"list_dir","arguments":""}}]}}]},
            {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"","type":"function","function":{"name":"","arguments":"{\"path\": \"/tmp\"}"}}]},"finish_reason":"tool_calls"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name, "read_file");
                assert_eq!(arguments.as_ref().unwrap()["file_path"], "/tmp/a.md");
            }
            _ => panic!("expected ToolCall"),
        }
        match &parts[1] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_2"));
                assert_eq!(name, "list_dir");
                assert_eq!(arguments.as_ref().unwrap()["path"], "/tmp");
            }
            _ => panic!("expected ToolCall"),
        }
        assert_eq!(finish, Some("tool_calls".to_string()));
    }

    #[test]
    fn test_extract_parts_from_sse_body_empty() {
        let body = r#"[{"choices":[{"delta":{}}]}]"#;
        assert!(GenAIBuilder::extract_parts_from_sse_body(body).is_none());
    }

    /// DashScope native streaming defaults to `incremental_output=false`, so
    /// every chunk repeats the *cumulative* text. Concatenating would produce
    /// "你你好你好吗". The reconstruction must replace instead.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_cumulative() {
        let body = r#"[
            {"output":{"choices":[{"finish_reason":"null","message":{"role":"assistant","content":"你"}}]},"usage":{"input_tokens":10,"output_tokens":1,"total_tokens":11},"request_id":"r1"},
            {"output":{"choices":[{"finish_reason":"null","message":{"role":"assistant","content":"你好"}}]},"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12},"request_id":"r1"},
            {"output":{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"你好吗"}}]},"usage":{"input_tokens":10,"output_tokens":3,"total_tokens":13},"request_id":"r1"}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "你好吗"));
        assert_eq!(finish, Some("stop".to_string()));
    }

    /// With `incremental_output=true` each chunk carries only the delta.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_incremental() {
        let body = r#"[
            {"output":{"choices":[{"finish_reason":"null","message":{"role":"assistant","content":"Hello "}}]}},
            {"output":{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"world"}}]}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "Hello world"));
        assert_eq!(finish, Some("stop".to_string()));
    }

    /// `result_format` defaults to `text`, which puts the answer directly on
    /// `output.text` with no `choices` array.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_text_format() {
        let body = r#"[
            {"output":{"finish_reason":"null","text":"1, 2"}},
            {"output":{"finish_reason":"stop","text":"1, 2, 3."}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "1, 2, 3."));
        assert_eq!(finish, Some("stop".to_string()));
    }

    /// Native tool calls arrive whole (arguments are not fragmented) under
    /// `output.choices[].message.tool_calls`, and repeat cumulatively.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_tool_calls() {
        let body = r#"[
            {"output":{"choices":[{"finish_reason":"null","message":{"role":"assistant","content":"","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}},
            {"output":{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":"","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\": \"Beijing\"}"}}]}}]}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name, "get_weather");
                assert_eq!(arguments.as_ref().unwrap()["city"], "Beijing");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        assert_eq!(finish, Some("tool_calls".to_string()));
    }

    /// Non-streaming native responses are a single JSON object, not an array.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_single_object() {
        let body = r#"{"output":{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"1, 2, 3."}}]},"usage":{"input_tokens":8,"output_tokens":4,"total_tokens":12},"request_id":"r1"}"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "1, 2, 3."));
        assert_eq!(finish, Some("stop".to_string()));
    }

    /// Multimodal content is an array of `{"text": ...}` / `{"image": ...}` blocks.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_multimodal_content_blocks() {
        let body = r#"{"output":{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":[{"text":"这是一只猫。"}]}}]},"usage":{"input_tokens":30,"output_tokens":12,"image_tokens":1247}}"#;
        let (parts, _finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "这是一只猫。"));
    }

    /// A hostile or malformed `index` must not be honoured: slots are allocated
    /// up to that value, so an unbounded one would let a single JSON field
    /// drive a huge allocation inside a passive observer.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_rejects_absurd_tool_call_index() {
        let body = r#"[
            {"output":{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":"hi","tool_calls":[{"index":4294967295,"id":"call_x","function":{"name":"boom","arguments":"{}"}}]}}]}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        // Text is still recovered; only the out-of-range tool call is dropped.
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "hi"));
        assert_eq!(finish, Some("tool_calls".to_string()));
    }

    /// A large-but-permitted index still works, so the cap is not over-tight.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_allows_in_range_tool_call_index() {
        let body = r#"[
            {"output":{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":"","tool_calls":[{"index":8,"id":"call_y","function":{"name":"ok","arguments":"{}"}}]}}]}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCall { name, .. } if name == "ok"))
        );
    }

    /// A native envelope with no content at all yields nothing.
    #[test]
    fn test_extract_parts_from_sse_body_dashscope_native_no_content() {
        let body = r#"[{"output":{"choices":[{"finish_reason":"null","message":{"role":"assistant","content":""}}]}}]"#;
        assert!(GenAIBuilder::extract_parts_from_sse_body(body).is_none());
    }

    #[test]
    fn test_extract_parts_from_sse_body_invalid() {
        assert!(GenAIBuilder::extract_parts_from_sse_body("not json").is_none());
    }

    #[test]
    fn test_parse_sse_response_body() {
        let body = r#"[{"choices":[{"delta":{"content":"Hi"}}]}]"#;
        let msgs = GenAIBuilder::parse_sse_response_body(body, Some("stop")).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn test_openai_msg_to_input_basic() {
        let msg = OpenAIChatMessage {
            role: crate::analyzer::message::types::MessageRole::User,
            content: Some(crate::analyzer::message::types::OpenAIContent::Text(
                "Hello".to_string(),
            )),
            reasoning_content: None,
            refusal: None,
            function_call: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            annotations: None,
            audio: None,
        };
        let input = GenAIBuilder::openai_msg_to_input(&msg);
        assert_eq!(input.role, "user");
        assert_eq!(input.parts.len(), 1);
        match &input.parts[0] {
            MessagePart::Text { content } => assert_eq!(content, "Hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn test_openai_msg_to_input_tool_role() {
        let msg = OpenAIChatMessage {
            role: crate::analyzer::message::types::MessageRole::Tool,
            content: Some(crate::analyzer::message::types::OpenAIContent::Text(
                "result data".to_string(),
            )),
            reasoning_content: None,
            refusal: None,
            function_call: None,
            tool_calls: None,
            tool_call_id: Some("tc_1".to_string()),
            name: None,
            annotations: None,
            audio: None,
        };
        let input = GenAIBuilder::openai_msg_to_input(&msg);
        assert_eq!(input.role, "tool");
        assert!(
            matches!(&input.parts[0], MessagePart::ToolCallResponse { id, .. } if id.as_deref() == Some("tc_1"))
        );
    }

    #[test]
    fn test_openai_msg_to_output() {
        let msg = OpenAIChatMessage {
            role: crate::analyzer::message::types::MessageRole::Assistant,
            content: Some(crate::analyzer::message::types::OpenAIContent::Text(
                "Response".to_string(),
            )),
            reasoning_content: Some("thinking".to_string()),
            refusal: None,
            function_call: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            annotations: None,
            audio: None,
        };
        let output = GenAIBuilder::openai_msg_to_output(&msg, Some("stop"));
        assert_eq!(output.role, "assistant");
        assert_eq!(output.finish_reason.as_deref(), Some("stop"));
        // Should have reasoning + text = 2 parts
        assert_eq!(output.parts.len(), 2);
        assert!(
            matches!(&output.parts[0], MessagePart::Reasoning { content } if content == "thinking")
        );
        assert!(matches!(&output.parts[1], MessagePart::Text { content } if content == "Response"));
    }
}
