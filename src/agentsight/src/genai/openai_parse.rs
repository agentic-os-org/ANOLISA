//! GenAI OpenAI body / SSE parsing & message conversion helpers
//!
//! Pure static helpers for parsing OpenAI request/response bodies and
//! converting between `OpenAIChatMessage` and the parts-based message
//! representation. Logic preserved verbatim from the original `builder.rs`; only
//! visibility was widened to `pub(super)` so siblings (`call_builder`)
//! can call these.

use super::GenAIBuilder;
use super::semantic::{InputMessage, LLMRequest, MessagePart, OutputMessage};
use crate::analyzer::message::ResponsesToolCalls;
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

/// Accumulate cumulative-or-incremental text into `buf`.
///
/// A frame that repeats the whole text so far replaces the buffer; a frame that
/// carries only an increment appends. Shared by the DashScope native envelope
/// and by the `choices[].message` snapshots some gateways send, because the
/// request's `incremental_output` flag is not visible on the response path.
fn accumulate_text(buf: &mut String, next: &str) {
    if next.is_empty() {
        return;
    }
    if next.starts_with(buf.as_str()) {
        *buf = next.to_string();
    } else {
        buf.push_str(next);
    }
}

impl GenAIBuilder {
    /// 从 HTTP request body 直接解析 LLMRequest（OpenAI/Anthropic 格式）
    pub(super) fn parse_request_body(body: &str) -> Option<LLMRequest> {
        let v: serde_json::Value = serde_json::from_str(body).ok()?;
        let mut request = Self::parse_request_value(&v)?;
        request.raw_body = Some(body.to_owned());
        Some(request)
    }

    pub(super) fn parse_request_value(v: &serde_json::Value) -> Option<LLMRequest> {
        let obj = v.as_object()?;

        // Normalized view: "messages" (chat completions) or "input" + "instructions"
        // (Responses API used by codex 0.137+ via dashscope /v1/responses).
        let (raw_messages, instructions_text) = crate::parser::llm::extract_messages_view(v)?;

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
            // Responses API items carry no `role`: the `type` identifies them.
            // A replayed conversation sends the assistant's tool request as
            // `function_call` and the tool output as `function_call_output`;
            // skipping them dropped the whole tool interaction from the
            // request event.
            if msg.get("role").is_none() {
                match msg.get("type").and_then(|v| v.as_str()) {
                    Some("function_call_output") => {
                        messages.push(InputMessage {
                            role: "tool".to_string(),
                            parts: vec![MessagePart::ToolCallResponse {
                                id: msg
                                    .get("call_id")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                response: msg
                                    .get("output")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null),
                            }],
                            name: None,
                        });
                        continue;
                    }
                    Some("function_call") => {
                        messages.push(InputMessage {
                            role: "assistant".to_string(),
                            parts: vec![MessagePart::ToolCall {
                                id: msg
                                    .get("call_id")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                name: msg
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                arguments: msg.get("arguments").cloned(),
                            }],
                            name: None,
                        });
                        continue;
                    }
                    _ => continue,
                }
            }
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
                        // Anthropic replays its tool interaction as content
                        // blocks: `tool_use` for the request, `tool_result` for
                        // the output. Reading only a block's `text` dropped
                        // both, so a request captured verbatim by the crash
                        // drain lost the tool results the ATIF observation and
                        // the tool-failure readers are built from. The mapping
                        // mirrors `anthropic_content_block_to_part`.
                        match item.get("type").and_then(|t| t.as_str()) {
                            // `server_tool_use` / `mcp_tool_use` are the
                            // provider-side spellings of the same call; the
                            // typed parser already reads them as `tool_use`,
                            // so a replayed server-tool turn extracted to an
                            // empty assistant message here.
                            Some("tool_use" | "server_tool_use" | "mcp_tool_use") => {
                                if let (Some(id), Some(name)) = (
                                    item.get("id").and_then(|v| v.as_str()),
                                    item.get("name").and_then(|v| v.as_str()),
                                ) {
                                    parts.push(MessagePart::ToolCall {
                                        id: Some(id.to_string()),
                                        name: name.to_string(),
                                        arguments: item.get("input").cloned(),
                                    });
                                }
                            }
                            // The server-tool results carry `tool_use_id` plus
                            // a caller-defined `content`, exactly like
                            // `tool_result`; the typed parser aliases every
                            // spelling, so a replayed search/fetch turn kept
                            // the result here but lost it through this reader.
                            Some(
                                "tool_result"
                                | "web_search_tool_result"
                                | "web_fetch_tool_result"
                                | "code_execution_tool_result"
                                | "bash_code_execution_tool_result"
                                | "text_editor_code_execution_tool_result"
                                | "tool_search_tool_result"
                                | "mcp_tool_result",
                            ) => {
                                if let Some(tool_use_id) =
                                    item.get("tool_use_id").and_then(|v| v.as_str())
                                {
                                    let response = match (
                                        item.get("content").cloned(),
                                        item.get("is_error").and_then(|v| v.as_bool()),
                                    ) {
                                        (Some(value), Some(is_error)) => serde_json::json!({
                                            "content": value,
                                            "is_error": is_error,
                                        }),
                                        (Some(value), None) => value,
                                        (None, Some(is_error)) => {
                                            serde_json::json!({ "is_error": is_error })
                                        }
                                        (None, None) => serde_json::Value::Null,
                                    };
                                    parts.push(MessagePart::ToolCallResponse {
                                        id: Some(tool_use_id.to_string()),
                                        response,
                                    });
                                }
                            }
                            _ => {
                                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                    parts.push(MessagePart::Text {
                                        content: text.to_string(),
                                    });
                                }
                            }
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

            // Legacy function_call (pre-tool_calls spelling of a tool request).
            if let Some(part) = msg
                .get("function_call")
                .and_then(Self::parse_legacy_function_call)
            {
                parts.push(part);
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

        // DashScope/Bailian native requests nest their sampling parameters
        // under "parameters" (the OpenAI-compatible spelling is top level), and
        // that protocol has no typed parser, so this fallback is the only place
        // that can read them. Without it the output cap is invisible to the
        // token-limit interruption rules and the request telemetry.
        let parameters = obj.get("parameters").and_then(|v| v.as_object());
        let param = |key: &str| {
            obj.get(key)
                .or_else(|| parameters.and_then(|params| params.get(key)))
        };

        let tools = param("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.to_vec());

        Some(LLMRequest {
            messages,
            temperature: param("temperature").and_then(|v| v.as_f64()),
            // The output cap has three spellings: the legacy chat
            // `max_tokens`, the newer chat `max_completion_tokens` (the only
            // one the o-series accepts), and the Responses API
            // `max_output_tokens`. Read whichever the request carries so
            // the token-limit interruption rules and the
            // `gen_ai.request.max_tokens` telemetry keep working.
            // A null legacy `max_tokens` is not a cap and must not stop the
            // search before a newer spelling that carries one.
            // Gemini adds a fourth, camelCase spelling — `maxOutputTokens`
            // nested under `generationConfig` — which the `param` helper
            // cannot reach: it probes keys as spelled at the top level and
            // under the DashScope `parameters` object only.
            max_tokens: ["max_tokens", "max_completion_tokens", "max_output_tokens"]
                .iter()
                .find_map(|key| param(key).and_then(|v| v.as_u64()))
                .or_else(|| {
                    obj.get("generationConfig")
                        .and_then(|cfg| cfg.get("maxOutputTokens"))
                        .and_then(|v| v.as_u64())
                })
                .map(|v| v as u32),
            frequency_penalty: param("frequency_penalty").and_then(|v| v.as_f64()),
            presence_penalty: param("presence_penalty").and_then(|v| v.as_f64()),
            top_p: param("top_p").and_then(|v| v.as_f64()),
            top_k: param("top_k").and_then(|v| v.as_f64()),
            seed: param("seed").and_then(|v| v.as_i64()),
            stop_sequences: param("stop").and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
            }),
            stream: obj.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
            tools,
            raw_body: None,
        })
    }

    /// Extract the LLM API response ID from parsed message or SSE body.
    ///
    /// Priority:
    /// 1. ParsedApiMessage response.id (OpenAI / Anthropic)
    /// 2. SSE response body first chunk "id" field
    /// 3. None (caller should fall back to call_id)
    pub(super) fn extract_response_id(
        parsed_message: &Option<&ParsedApiMessage>,
        http: &HttpRecord,
    ) -> Option<String> {
        // 1. Try parsed message response.id. `ParsedApiMessage::response_id`
        // covers every protocol the parser knows; matching the variants by hand
        // missed SysOM, whose call then carried the generated call id instead of
        // the provider's response id.
        if let Some(msg) = parsed_message {
            if let Some(id) = msg.response_id().filter(|id| !id.is_empty()) {
                return Some(id.to_string());
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

        // Legacy function_call: the same tool request in the older spelling.
        if let Some(part) = m
            .function_call
            .as_ref()
            .and_then(Self::parse_legacy_function_call)
        {
            parts.push(part);
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

        // Refusal text: the answer when the model declines instead of replying.
        if let Some(ref r) = m.refusal {
            if !r.is_empty() {
                parts.push(MessagePart::Text { content: r.clone() });
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

        // Legacy function_call: the same tool request in the older spelling.
        if let Some(part) = m
            .function_call
            .as_ref()
            .and_then(Self::parse_legacy_function_call)
        {
            parts.push(part);
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

    /// Legacy `function_call` payload, the pre-`tool_calls` spelling that
    /// older models (and clients replaying their history) still use:
    /// `{"name": "…", "arguments": "{…}"}`. Without this the tool request was
    /// dropped from the semantic model, so tool-use metrics saw a plain text
    /// turn where the model had actually asked to run a tool.
    pub(super) fn parse_legacy_function_call(value: &serde_json::Value) -> Option<MessagePart> {
        if value.get("function").is_some() {
            // Already in the modern nesting.
            return Self::parse_openai_tool_call_value(value);
        }
        let name = value.get("name")?.as_str()?.to_string();
        if name.is_empty() {
            return None;
        }
        let arguments = value.get("arguments").and_then(|v| match v {
            serde_json::Value::String(s) => serde_json::from_str(s).ok(),
            other => Some(other.clone()),
        });
        Some(MessagePart::ToolCall {
            id: None,
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
        // Prefer finish_reason from SSE, fall back to caller-supplied value
        let finish_reason =
            sse_finish_reason.or_else(|| fallback_finish_reason.map(|s| s.to_string()));
        if parts.is_empty() && finish_reason.is_none() {
            return None;
        }
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

        let (parts, finish_reason) = Self::merge_sse_chunks(&chunks);
        if parts.is_empty() {
            return Self::extract_dashscope_native_parts(&chunks)
                .or_else(|| finish_reason.map(|reason| (parts, Some(reason))));
        }
        Some((parts, finish_reason))
    }

    /// Merge already-parsed OpenAI-style SSE chunks into parts. Shared by the
    /// live response path (`extract_parts_from_sse_body`) and the drain
    /// enrichment (`GenAIBuilder::extract_sse_enrichment`) so both persist
    /// identical, deserializable `output_messages` — content, reasoning, and
    /// index-merged tool-call deltas alike.
    ///
    /// Anthropic streams carry no `choices` array at all, so when none of the
    /// chunks is OpenAI-shaped the Anthropic aggregation below runs instead —
    /// the live path reconstructs Anthropic content through the analyzer's
    /// message parser, but the drain path has only this merger. The OpenAI
    /// **Responses** protocol (`response.*` events) is the same story and gets
    /// its own aggregation first.
    pub(super) fn merge_sse_chunks(
        chunks: &[serde_json::Value],
    ) -> (Vec<MessagePart>, Option<String>) {
        if !chunks.iter().any(|c| c.get("choices").is_some()) {
            if let Some(merged) = Self::merge_responses_sse_chunks(chunks) {
                return merged;
            }
            if let Some(merged) = Self::merge_anthropic_sse_chunks(chunks) {
                return merged;
            }
            if let Some(merged) = Self::merge_gemini_sse_chunks(chunks) {
                return merged;
            }
        }
        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut refusal_buf = String::new();
        let mut finish_reason: Option<String> = None;
        // tool_call delta merging: (choice index, index) ->
        // (id, name, arguments_accumulated). OpenAI defines the wire index
        // per choice, so every choice starts its calls at index 0 and the
        // key must carry both or two choices' calls fold into one slot.
        let mut tc_map: HashMap<(u64, u32), (String, String, String)> = HashMap::new();
        // Slot each choice's latest fragment landed in, so an index-less
        // fragment without a name extends the call its choice is holding.
        let mut current_slot: HashMap<u64, u32> = HashMap::new();
        // Legacy `function_call` delta merging: the pre-`tool_calls` spelling
        // streams its single call as fragments (the name once, then bare
        // argument fragments), so like tc_map it accumulates (name, arguments).
        let mut legacy_fc: Option<(String, String)> = None;

        log::debug!("[GenAI] Merging SSE chunks ({} chunks)", chunks.len());

        for chunk in chunks.iter() {
            let choices = chunk.get("choices").and_then(|c| c.as_array());
            let choices = match choices {
                Some(c) => c,
                None => continue,
            };
            for (choice_pos, choice) in choices.iter().enumerate() {
                let choice_index = choice
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(choice_pos as u64);
                // A choice carries either a streaming `delta` or a cumulative
                // `message` snapshot. SysOM's Copilot stream — and some
                // OpenAI-compatible gateways, whose final frame is a whole
                // `message` instead of a last `delta` — send the latter, so
                // reading only `delta` dropped those frames entirely: their
                // text, and the `finish_reason` sitting beside them. A snapshot
                // repeats the answer so far, so it accumulates like the
                // DashScope native envelope rather than appending blindly.
                let (delta, cumulative) = match (choice.get("delta"), choice.get("message")) {
                    (Some(delta), _) => (delta, false),
                    (None, Some(message)) => (message, true),
                    (None, None) => {
                        // Legacy `/v1/completions` streams carry their
                        // fragment directly on the choice, with neither a
                        // `delta` nor a `message` object, so skipping the
                        // choice dropped the whole answer of a drained call.
                        if let Some(text) = choice.get("text").and_then(|v| v.as_str()) {
                            content_buf.push_str(text);
                        }
                        // Even a choice with no payload can carry the terminal
                        // finish reason.
                        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                            finish_reason = Some(fr.to_string());
                        }
                        continue;
                    }
                };
                // Content
                if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                    if cumulative {
                        accumulate_text(&mut content_buf, c);
                    } else {
                        content_buf.push_str(c);
                    }
                }
                // Reasoning
                if let Some(r) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                    if cumulative {
                        accumulate_text(&mut reasoning_buf, r);
                    } else {
                        reasoning_buf.push_str(r);
                    }
                }
                // Refusal: OpenAI's safety refusal arrives in its own delta
                // field, with no content delta alongside it. Message snapshots
                // repeat the refusal accumulated so far, so replace/extend it
                // using the same rule as snapshot text.
                if let Some(r) = delta.get("refusal").and_then(|v| v.as_str()) {
                    if cumulative {
                        accumulate_text(&mut refusal_buf, r);
                    } else {
                        refusal_buf.push_str(r);
                    }
                }
                // OpenAI tool_calls deltas are fragments; SysOM tool_use and
                // OpenAI message.tool_calls are complete snapshots. Merge both
                // by index while preserving each format's update semantics.
                let tool_calls = delta
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .or_else(|| {
                        if cumulative {
                            delta.get("tool_use").and_then(|v| v.as_array())
                        } else {
                            None
                        }
                    });
                if let Some(calls) = tool_calls {
                    for (call_pos, tc) in calls.iter().enumerate() {
                        // `index` comes off the wire. A value outside the slot
                        // range must not be truncated into another slot, which
                        // would overwrite a valid tool call's id, name and
                        // arguments; drop it like the native envelope path does.
                        let idx = match tc.get("index").and_then(|v| v.as_u64()) {
                            Some(idx) => idx,
                            // A cumulative frame repeats the whole call list, so
                            // the position inside it names the call, exactly as
                            // the token-side reconstruction reads it.
                            None if cumulative => call_pos as u64,
                            None => {
                                // Compatible providers stream calls without an
                                // index and mark each one with its name: a named
                                // fragment starts the next call, an unnamed one
                                // extends the call it belongs to. Keying every
                                // index-less fragment to slot 0 folded a later
                                // call into the first one's slot.
                                let starts_call = tc
                                    .get("function")
                                    .and_then(|func| func.get("name"))
                                    .and_then(|v| v.as_str())
                                    .is_some_and(|name| !name.is_empty());
                                if starts_call {
                                    // Allocate within the current choice so a
                                    // second choice's first call does not
                                    // extend the first choice's last slot.
                                    tc_map
                                        .keys()
                                        .filter(|(choice, _)| *choice == choice_index)
                                        .map(|(_, slot)| u64::from(*slot))
                                        .max()
                                        .map_or(0, |slot| slot.saturating_add(1))
                                } else {
                                    u64::from(current_slot.get(&choice_index).copied().unwrap_or(0))
                                }
                            }
                        };
                        if idx >= MAX_TOOL_CALL_SLOTS {
                            log::debug!(
                                "[GenAI] dropping SSE tool_call with out-of-range index {idx}"
                            );
                            continue;
                        }
                        let idx = idx as u32;
                        current_slot.insert(choice_index, idx);
                        let entry = tc_map
                            .entry((choice_index, idx))
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
                                if cumulative {
                                    if !args.is_empty() {
                                        entry.2 = args.to_string();
                                    }
                                } else {
                                    entry.2.push_str(args);
                                }
                            }
                        }
                    }
                }
                // Legacy function_call deltas — the pre-`tool_calls` spelling
                // of a tool request, streamed as `delta.function_call`
                // fragments: the name arrives once (later fragments repeat it
                // empty or omit it), the arguments as string fragments. The
                // message converters read the whole-message form; without
                // this arm the enrich and drain paths dropped the streamed
                // call entirely. A snapshot's `message.function_call` is
                // whole, not a fragment, and is left to the typed parsers —
                // the same rule the tool_calls merge above follows.
                if let (false, Some(fc)) = (cumulative, delta.get("function_call")) {
                    let entry = legacy_fc.get_or_insert_with(|| (String::new(), String::new()));
                    if let Some(name) = fc.get("name").and_then(|v| v.as_str()) {
                        if !name.is_empty() {
                            entry.0 = name.to_string();
                        }
                    }
                    if let Some(args) = fc.get("arguments").and_then(|v| v.as_str()) {
                        entry.1.push_str(args);
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
        // Refusal text: the model's answer when it declines a request.
        if !refusal_buf.is_empty() {
            parts.push(MessagePart::Text {
                content: refusal_buf,
            });
        }
        // Merged tool calls
        if !tc_map.is_empty() {
            let mut indices: Vec<(u64, u32)> = tc_map.keys().cloned().collect();
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
        // Merged legacy function_call (pre-tool_calls spelling): the same
        // ToolCall shape the message converters produce for the whole-message
        // form — no id on the wire, arguments a JSON string when complete.
        if let Some((name, arguments)) = legacy_fc {
            if !name.is_empty() {
                let parsed_args: Option<serde_json::Value> = if arguments.is_empty() {
                    None
                } else {
                    serde_json::from_str(&arguments).ok()
                };
                parts.push(MessagePart::ToolCall {
                    id: None,
                    name,
                    arguments: parsed_args,
                });
            }
        }

        (parts, finish_reason)
    }

    /// Aggregate Anthropic-protocol SSE chunks into parts.
    ///
    /// Anthropic streams usage and content in separate events (`message_start`
    /// carries input tokens, `message_delta` the output tokens and
    /// `stop_reason`), and the content itself arrives as `content_block_start`
    /// plus `content_block_delta` events addressed by a wire-supplied `index`.
    /// The live path reconstructs this through the analyzer's message parser
    /// (`ParsedApiMessage::AnthropicMessage`), but the dead-pid/flush drain
    /// path persists through `extract_sse_enrichment` → `merge_sse_chunks`,
    /// which understood only OpenAI `choices[].delta` — a drained Anthropic
    /// stream therefore lost its entire output (`output_messages = None`)
    /// while the same stream captured live kept it.
    ///
    /// Mirrors the analyzer's aggregation: text deltas → one Text part per
    /// block, thinking deltas → Reasoning, tool_use blocks (including the
    /// provider-side `server_tool_use` / `mcp_tool_use` spellings) → ToolCall
    /// with the concatenated `input_json_delta` fragments, all in block-index
    /// order.
    /// Block indices are capped like `MAX_TOOL_CALL_SLOTS` so a hostile index
    /// cannot turn one JSON field into an unbounded map. Returns `None` when
    /// no Anthropic event shape is present (caller falls back).
    pub(super) fn merge_anthropic_sse_chunks(
        chunks: &[serde_json::Value],
    ) -> Option<(Vec<MessagePart>, Option<String>)> {
        enum Block {
            Text(String),
            Thinking(String),
            ToolUse {
                id: String,
                name: String,
                args_json: String,
            },
        }

        let mut blocks: std::collections::BTreeMap<u64, Block> = std::collections::BTreeMap::new();
        let mut finish_reason: Option<String> = None;
        let mut saw_anthropic_event = false;

        for chunk in chunks {
            let event_type = chunk.get("type").and_then(|v| v.as_str());
            match event_type {
                Some("content_block_start") => {
                    saw_anthropic_event = true;
                    let Some(index) = chunk.get("index").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    if index >= MAX_TOOL_CALL_SLOTS {
                        continue;
                    }
                    let Some(content_block) = chunk.get("content_block") else {
                        continue;
                    };
                    let block = match content_block.get("type").and_then(|v| v.as_str()) {
                        // `server_tool_use` and `mcp_tool_use` are the same
                        // call made by a tool the provider runs on its side
                        // (web search, code execution, the MCP connector). The
                        // typed parser reads all three spellings as one block
                        // type, so the drain merger must not fold them into
                        // the text accumulator: a text block has no argument
                        // buffer, so the call vanished and every
                        // `input_json_delta` fragment after it was dropped
                        // silently.
                        Some("tool_use" | "server_tool_use" | "mcp_tool_use") => Block::ToolUse {
                            id: content_block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            name: content_block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            // A compatible gateway may send the whole call on the
                            // start event and no deltas at all. The standard
                            // stream sends the placeholder `input: {}` that its
                            // `input_json_delta` fragments replace, so an empty
                            // object must not become the arguments.
                            args_json: match content_block.get("input") {
                                Some(serde_json::Value::Null) | None => String::new(),
                                Some(serde_json::Value::Object(map)) if map.is_empty() => {
                                    String::new()
                                }
                                Some(value) => value.to_string(),
                            },
                        },
                        Some("thinking") => Block::Thinking(
                            content_block
                                .get("thinking")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        // Text and any future block kind default to a text
                        // accumulator, seeded from the start block so a gateway
                        // that sends the finished text there and no deltas keeps
                        // it; unknown deltas are then ignored.
                        _ => Block::Text(
                            content_block
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        ),
                    };
                    blocks.insert(index, block);
                }
                Some("content_block_delta") => {
                    saw_anthropic_event = true;
                    let Some(index) = chunk.get("index").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    // Match `content_block_start`: a delta must not revive an
                    // index the start event refuses to allocate.
                    if index >= MAX_TOOL_CALL_SLOTS {
                        continue;
                    }
                    let Some(delta) = chunk.get("delta") else {
                        continue;
                    };
                    match delta.get("type").and_then(|v| v.as_str()) {
                        Some("text_delta") => {
                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                // A drain capture can attach after the
                                // block-start event. The live analyzer
                                // creates the missing block on the first
                                // delta; dropping it here lost every text
                                // fragment of a mid-stream capture.
                                let block = blocks
                                    .entry(index)
                                    .or_insert_with(|| Block::Text(String::new()));
                                if let Block::Text(buf) = block {
                                    buf.push_str(text);
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(text) = delta.get("thinking").and_then(|v| v.as_str()) {
                                let block = blocks
                                    .entry(index)
                                    .or_insert_with(|| Block::Thinking(String::new()));
                                if let Block::Thinking(buf) = block {
                                    buf.push_str(text);
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(fragment) =
                                delta.get("partial_json").and_then(|v| v.as_str())
                            {
                                let block = blocks.entry(index).or_insert_with(|| Block::ToolUse {
                                    id: String::new(),
                                    name: String::new(),
                                    args_json: String::new(),
                                });
                                if let Block::ToolUse { args_json, .. } = block {
                                    args_json.push_str(fragment);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("message_delta") => {
                    saw_anthropic_event = true;
                    if let Some(stop) = chunk.pointer("/delta/stop_reason").and_then(|v| v.as_str())
                    {
                        finish_reason = Some(stop.to_string());
                    }
                }
                _ => {}
            }
        }

        if !saw_anthropic_event {
            return None;
        }

        let mut parts = Vec::new();
        for block in blocks.into_values() {
            match block {
                Block::Text(text) if !text.is_empty() => {
                    parts.push(MessagePart::Text { content: text });
                }
                Block::Thinking(thinking) if !thinking.is_empty() => {
                    parts.push(MessagePart::Reasoning { content: thinking });
                }
                Block::ToolUse {
                    id,
                    name,
                    args_json,
                } => {
                    let arguments = if args_json.trim().is_empty() {
                        None
                    } else {
                        serde_json::from_str(&args_json).ok()
                    };
                    parts.push(MessagePart::ToolCall {
                        id: if id.is_empty() { None } else { Some(id) },
                        name,
                        arguments,
                    });
                }
                _ => {}
            }
        }

        Some((parts, finish_reason))
    }

    /// Merge Gemini `streamGenerateContent` SSE chunks into parts.
    ///
    /// Each chunk is a standalone generation payload — there is no sentinel
    /// frame and no `choices` array. Text streams as
    /// `candidates[].content.parts[].text`, calls as whole
    /// `parts[].functionCall` objects (not deltas), thinking as parts marked
    /// `thought: true`, and only the terminal chunk carries a non-empty
    /// `candidates[].finishReason`. `usageMetadata` rides the terminal chunk
    /// but is a token concern, not a message part. Returns `None` when no
    /// chunk is Gemini-shaped so the remaining mergers still run.
    pub(super) fn merge_gemini_sse_chunks(
        chunks: &[serde_json::Value],
    ) -> Option<(Vec<MessagePart>, Option<String>)> {
        let mut text_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut tool_calls: Vec<MessagePart> = Vec::new();
        let mut finish_reason: Option<String> = None;
        let mut saw_gemini = false;

        for chunk in chunks {
            let Some(candidates) = chunk
                .get("candidates")
                .and_then(serde_json::Value::as_array)
            else {
                continue;
            };
            for candidate in candidates {
                if let Some(reason) = candidate
                    .get("finishReason")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    saw_gemini = true;
                    finish_reason = Some(reason.to_string());
                }
                let Some(parts) = candidate
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(serde_json::Value::as_array)
                else {
                    continue;
                };
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        saw_gemini = true;
                        if part.get("thought").and_then(|v| v.as_bool()) == Some(true) {
                            reasoning_buf.push_str(text);
                        } else {
                            text_buf.push_str(text);
                        }
                    }
                    if let Some(call) = part.get("functionCall") {
                        saw_gemini = true;
                        tool_calls.push(MessagePart::ToolCall {
                            id: None,
                            name: call
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            arguments: call.get("args").cloned(),
                        });
                    }
                }
            }
        }

        if !saw_gemini {
            return None;
        }
        let mut parts = Vec::new();
        if !reasoning_buf.is_empty() {
            parts.push(MessagePart::Reasoning {
                content: reasoning_buf,
            });
        }
        if !text_buf.is_empty() {
            parts.push(MessagePart::Text { content: text_buf });
        }
        parts.extend(tool_calls);
        Some((parts, finish_reason))
    }

    /// Answer text of a finished `message` output item, or `None`.
    ///
    /// Mirrors `OpenAIParser::closing_message_item_text`: reasoning items
    /// carry thinking rather than the answer and are skipped, and every
    /// `text` part of a message item is concatenated in order.
    fn closing_message_item_text(chunk: &serde_json::Value) -> Option<String> {
        let item = chunk.get("item")?;
        if item.get("type").and_then(|t| t.as_str()) == Some("reasoning") {
            return None;
        }
        let parts = item.get("content")?.as_array()?;
        let text: String = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
            .collect();
        (!text.is_empty()).then_some(text)
    }

    /// Refusal text of a finished `message` output item, or `None`.
    ///
    /// The answer a model declines to give is its own content part, typed
    /// `refusal` and carrying the text in `refusal` rather than `text` — the
    /// shape the non-streaming normalizer and the token extractor's
    /// `read_output_item` read. A capture that attached after the refusal
    /// deltas has this part as its only copy of the answer, so it is read
    /// here as well; the deltas win over it, as they do for text.
    fn closing_message_item_refusal(chunk: &serde_json::Value) -> Option<String> {
        let item = chunk.get("item")?;
        if item.get("type").and_then(|t| t.as_str()) == Some("reasoning") {
            return None;
        }
        let parts = item.get("content")?.as_array()?;
        let refusal: String = parts
            .iter()
            .filter(|part| part.get("type").and_then(|t| t.as_str()) == Some("refusal"))
            .filter_map(|part| part.get("refusal").and_then(|r| r.as_str()))
            .collect();
        (!refusal.is_empty()).then_some(refusal)
    }

    /// Thinking text of a finished `reasoning` output item, or `None`.
    ///
    /// The closing item carries its thinking in `content[]` parts (dashscope
    /// `reasoning_text`) and/or `summary[]` (the o-series default), the same
    /// two fields the token extractor's `read_output_item` routes to its
    /// thinking half. Unlike the answer, reasoning items do not repeat one
    /// another: an agentic response interleaves one before each call it makes
    /// and every item carries different thinking, so callers accumulate the
    /// results in arrival order instead of replacing.
    fn closing_reasoning_item_text(chunk: &serde_json::Value) -> Option<String> {
        let item = chunk.get("item")?;
        if item.get("type").and_then(|t| t.as_str()) != Some("reasoning") {
            return None;
        }
        let text: String = ["content", "summary"]
            .iter()
            .filter_map(|key| item.get(*key).and_then(|c| c.as_array()))
            .flatten()
            .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
            .collect();
        (!text.is_empty()).then_some(text)
    }

    /// Aggregate OpenAI **Responses** API SSE chunks into parts.
    ///
    /// The Responses protocol (used by codex 0.137+ via `/v1/responses`) emits
    /// `response.*` events: text arrives as `response.output_text.delta`,
    /// reasoning as `response.reasoning_text.delta` /
    /// `response.reasoning_summary_text.delta`, function calls as
    /// `response.output_item.added` (type `function_call`)
    /// plus `response.function_call_arguments.delta`, and the stream closes
    /// with `response.completed` or, when the output cap ended it,
    /// `response.incomplete`. The live path reconstructs this through the
    /// analyzer's message parser (`aggregate_responses_sse_chunks`), but the
    /// dead-pid/flush drain path persists through `extract_sse_enrichment` →
    /// `merge_sse_chunks`, which understood none of it — a drained Responses
    /// stream lost its entire output (`output_messages = None`) while the same
    /// stream captured live kept it.
    ///
    /// Shares the analyzer's per-item tool state so interleaved arguments and
    /// done payloads stay attached to their own call. Incomplete calls survive
    /// without a per-call done event. A done event keyed by an identity no
    /// `output_item.added` carried (a capture that attached after the headers)
    /// cannot be routed by the item state; its complete arguments are still
    /// kept, re-attached to the call that was current when it arrived.
    pub(super) fn merge_responses_sse_chunks(
        chunks: &[serde_json::Value],
    ) -> Option<(Vec<MessagePart>, Option<String>)> {
        let mut text_buf = String::new();
        // Whether a non-empty text delta arrived. Closing events repeat the
        // deltas, so they are read only when this stays false — the same
        // delta-wins rule as `aggregate_responses_sse_chunks`.
        let mut saw_text_delta = false;
        // Answer text carried by a closing event, kept aside until the whole
        // stream is read: a delta captured later supersedes it.
        let mut closing_text: Option<String> = None;
        // Whether a non-empty reasoning delta arrived, and the thinking a
        // finished `reasoning` item carries. Same delta-wins rule as the text
        // above, but the items accumulate rather than replace: each one holds
        // different thinking.
        let mut saw_reasoning_delta = false;
        let mut closing_reasoning_text = String::new();
        let mut reasoning_buf = String::new();
        // Whether a non-empty refusal delta arrived, and the refusal a closing
        // message item carries, on the same delta-wins rule as the text above.
        let mut saw_refusal_delta = false;
        let mut closing_refusal: Option<String> = None;
        let mut refusal_buf = String::new();
        let mut calls = ResponsesToolCalls::default();
        let mut saw_responses_event = false;
        // Done payloads the item router cannot attribute: the router matches a
        // done event by the item id / output index its `output_item.added`
        // carried, and a capture that started mid-stream can attach before
        // those headers were seen. Dropping such a payload persists the call
        // with no arguments even though the done event carries them, so record
        // it against the last added function_call (the router's current call)
        // and re-attach it below.
        let mut last_added: Option<(String, String)> = None;
        let mut added_item_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut added_indexes: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut orphan_done: Vec<(String, String, String)> = Vec::new();
        // Set by the terminal `response.incomplete` event when the stream was
        // cut by the output cap or the safety filter, mirroring
        // `aggregate_responses_sse_chunks`; `response.completed` sets
        // `completed` for the finish fallback below.
        let mut completed = false;
        let mut output_capped = false;
        let mut output_filtered = false;

        for chunk in chunks {
            calls.observe(chunk);
            let event_type = chunk.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match event_type {
                "response.output_text.delta" => {
                    saw_responses_event = true;
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        if !delta.is_empty() {
                            saw_text_delta = true;
                        }
                        text_buf.push_str(delta);
                    }
                }
                // The closing event carries the finalized answer and repeats
                // what the deltas delivered, so it is read only when no
                // non-empty delta was captured: a capture that attached after
                // the deltas has this event as its only copy of the text.
                "response.output_text.done" => {
                    if !saw_text_delta
                        && let Some(text) = chunk
                            .get("text")
                            .and_then(|t| t.as_str())
                            .filter(|text| !text.is_empty())
                    {
                        closing_text = Some(text.to_string());
                        saw_responses_event = true;
                    }
                }
                // Reasoning models stream their thinking as text deltas on the
                // same event channel (qwen3-coder via dashscope sends
                // reasoning_text, the o-series summary_text); both become a
                // `Reasoning` part like the live parser's reasoning_content.
                "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                    saw_responses_event = true;
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        if !delta.is_empty() {
                            saw_reasoning_delta = true;
                        }
                        reasoning_buf.push_str(delta);
                    }
                }
                "response.created" => {
                    saw_responses_event = true;
                }
                "response.completed" => {
                    saw_responses_event = true;
                    completed = true;
                }
                "response.output_item.added" => {
                    saw_responses_event = true;
                    let item = chunk.get("item");
                    if item.and_then(|i| i.get("type")).and_then(|t| t.as_str())
                        == Some("function_call")
                    {
                        if let Some(id) = item.and_then(|i| i.get("id")).and_then(|v| v.as_str()) {
                            added_item_ids.insert(id.to_string());
                        }
                        if let Some(index) = chunk.get("output_index").and_then(|v| v.as_u64()) {
                            added_indexes.insert(index);
                        }
                        last_added = Some((
                            item.and_then(|i| i.get("call_id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            item.and_then(|i| i.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        ));
                    }
                }
                "response.refusal.delta" => {
                    saw_responses_event = true;
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        if !delta.is_empty() {
                            saw_refusal_delta = true;
                        }
                        refusal_buf.push_str(delta);
                    }
                }
                "response.refusal.done" => {
                    // The done event carries the finalized text; the deltas
                    // may be missing when capture started mid-stream.
                    saw_responses_event = true;
                    if let Some(refusal) = chunk.get("refusal").and_then(|r| r.as_str()) {
                        refusal_buf.clear();
                        refusal_buf.push_str(refusal);
                    }
                }
                "response.output_item.done" => {
                    // A capture that started mid-stream may open with a done
                    // item instead of an added one; the item router recovers
                    // the complete call from it, so a function_call done item
                    // proves this is a Responses stream with recoverable
                    // content.
                    if chunk
                        .get("item")
                        .and_then(|i| i.get("type"))
                        .and_then(|t| t.as_str())
                        == Some("function_call")
                    {
                        saw_responses_event = true;
                    }
                    // A finished `message` item carries the whole answer in
                    // its `content[]` parts and is the only copy when capture
                    // attached after the deltas were emitted. Same delta-wins
                    // rule as `response.output_text.done` above.
                    if !saw_text_delta && let Some(text) = Self::closing_message_item_text(chunk) {
                        closing_text = Some(text);
                        saw_responses_event = true;
                    }
                    // A `reasoning` item carries the thinking the deltas
                    // streamed; a capture that attached after them has the
                    // item as its only copy. Every item is kept, in order.
                    if !saw_reasoning_delta
                        && let Some(text) = Self::closing_reasoning_item_text(chunk)
                    {
                        closing_reasoning_text.push_str(&text);
                        saw_responses_event = true;
                    }
                    // The answer a model declines to give rides a `refusal`
                    // part of the same item instead of a `text` part, so it is
                    // read separately; a capture that attached after the
                    // refusal deltas has it as the only copy.
                    if !saw_refusal_delta
                        && let Some(refusal) = Self::closing_message_item_refusal(chunk)
                    {
                        closing_refusal = Some(refusal);
                        saw_responses_event = true;
                    }
                }
                "response.function_call_arguments.done" => {
                    if let Some(arguments) = chunk.get("arguments").and_then(|a| a.as_str()) {
                        let item_id = chunk.get("item_id").and_then(|v| v.as_str());
                        let output_index = chunk.get("output_index").and_then(|v| v.as_u64());
                        let identified = item_id.is_some() || output_index.is_some();
                        let routed = (item_id.is_some_and(|id| added_item_ids.contains(id)))
                            || (output_index.is_some_and(|i| added_indexes.contains(&i)));
                        if identified && !routed && last_added.is_some() {
                            if let Some((call_id, name)) = &last_added {
                                orphan_done.push((
                                    call_id.clone(),
                                    name.clone(),
                                    arguments.to_string(),
                                ));
                            }
                        }
                    }
                }
                // A capped stream terminates with response.incomplete instead
                // of response.completed; the cap reason must surface as the
                // chat-completions "length" finish rather than a clean "stop",
                // and a safety-filter cut as "content_filter" — the same
                // reason mapping the live parser's aggregator applies.
                //
                // Every incomplete reason is a terminal event, including
                // `max_messages` and `steered`: leaving `completed` false made
                // the finish fall through to None, which the detector reports
                // as `sse_truncated` — a transport failure — for a turn the
                // provider ended itself.
                "response.incomplete" => {
                    saw_responses_event = true;
                    completed = true;
                    if let Some(resp) = chunk.get("response") {
                        if resp.get("status").and_then(|v| v.as_str()) == Some("incomplete") {
                            match resp
                                .pointer("/incomplete_details/reason")
                                .and_then(|v| v.as_str())
                            {
                                Some("max_output_tokens") => output_capped = true,
                                Some("content_filter") => output_filtered = true,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if !saw_responses_event {
            return None;
        }

        // Deltas win; a closing event supplies the answer only when no
        // non-empty delta was captured.
        if !saw_text_delta && let Some(text) = closing_text {
            text_buf = text;
        }
        if !saw_reasoning_delta && !closing_reasoning_text.is_empty() {
            reasoning_buf = closing_reasoning_text;
        }
        if !saw_refusal_delta && let Some(refusal) = closing_refusal {
            refusal_buf = refusal;
        }

        let mut tool_parts: Vec<_> = calls
            .into_calls()
            .map(|(id, name, arguments)| MessagePart::ToolCall {
                id: if id.is_empty() { None } else { Some(id) },
                name,
                arguments: serde_json::from_str(&arguments).ok(),
            })
            .collect();
        for part in &mut tool_parts {
            if let MessagePart::ToolCall {
                id,
                name,
                arguments,
            } = part
            {
                if arguments.is_some() {
                    continue;
                }
                if let Some((_, (_, _, payload))) =
                    orphan_done.iter().enumerate().find(|(_, (oid, oname, _))| {
                        *oid == id.as_deref().unwrap_or("") && oname == name
                    })
                {
                    *arguments = serde_json::from_str(payload).ok();
                }
            }
        }

        let mut parts = Vec::new();
        // Reasoning first, same order as the OpenAI chat-completions merger.
        if !reasoning_buf.is_empty() {
            parts.push(MessagePart::Reasoning {
                content: reasoning_buf,
            });
        }
        if !text_buf.is_empty() {
            parts.push(MessagePart::Text { content: text_buf });
        }
        if !refusal_buf.is_empty() {
            parts.push(MessagePart::Text {
                content: refusal_buf,
            });
        }
        parts.extend(tool_parts);

        // Same finish-reason convention as the analyzer's aggregator: the
        // terminal event decides. A capped stream is the chat "length"
        // finish — the cap wins even when a tool call was in flight, because
        // its arguments may be cut mid-JSON — a filtered one is
        // "content_filter" in the aggregator's order (cap first), and a
        // stream whose capture never saw a terminal event at all keeps an
        // unknown finish instead of a fabricated clean stop, the same None
        // the Anthropic merger leaves on a missing message_delta.
        let finish_reason = if output_capped {
            Some("length".to_string())
        } else if output_filtered {
            Some("content_filter".to_string())
        } else if !completed {
            None
        } else if parts
            .iter()
            .any(|p| matches!(p, MessagePart::ToolCall { .. }))
        {
            Some("tool_calls".to_string())
        } else {
            Some("stop".to_string())
        };

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
    pub(super) fn extract_dashscope_native_parts(
        chunks: &[serde_json::Value],
    ) -> Option<(Vec<MessagePart>, Option<String>)> {
        /// Accumulate cumulative-or-incremental text into `buf`.
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
                accumulate_text(&mut content_buf, text);
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
                    accumulate_text(&mut content_buf, &content_text(content));
                }
                if let Some(reasoning) = message.get("reasoning_content").and_then(|r| r.as_str()) {
                    accumulate_text(&mut reasoning_buf, reasoning);
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

    /// Legacy `function_call` payloads (finish_reason `function_call`) carry the
    /// tool request in the pre-`tool_calls` spelling; dropping them made the
    /// turn look like plain text.
    #[test]
    fn legacy_function_call_becomes_a_tool_call_part() {
        let body = r#"{
            "model": "gpt-3.5-turbo",
            "messages": [
                {"role": "user", "content": "weather in Beijing?"},
                {"role": "assistant", "content": null,
                 "function_call": {"name": "get_weather", "arguments": "{\"city\":\"Beijing\"}"}}
            ]
        }"#;
        let req = GenAIBuilder::parse_request_body(body).expect("body parses");
        match &req.messages[1].parts[0] {
            MessagePart::ToolCall {
                name, arguments, ..
            } => {
                assert_eq!(name, "get_weather");
                assert_eq!(
                    arguments
                        .as_ref()
                        .and_then(|a| a.get("city"))
                        .and_then(|c| c.as_str()),
                    Some("Beijing")
                );
            }
            other => panic!("expected a tool call, got {other:?}"),
        }

        // The typed response converter does the same.
        let output =
            serde_json::from_value::<crate::analyzer::message::OpenAIResponse>(serde_json::json!({
                "id": "chatcmpl-legacy",
                "object": "chat.completion",
                "created": 0,
                "model": "gpt-3.5-turbo",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": null,
                                "function_call": {"name": "get_weather", "arguments": "{}"}},
                    "finish_reason": "function_call"
                }]
            }))
            .expect("response parses");
        let msg = &output.choices[0].message;
        let parts =
            GenAIBuilder::openai_msg_to_output(msg, output.choices[0].finish_reason.as_deref());
        assert!(matches!(parts.parts[0], MessagePart::ToolCall { .. }));
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

    /// The SysOM (Aliyun Copilot) response carries its id inside `response.id`;
    /// ignoring it made the row fall back to the generated call id, breaking
    /// correlation with the provider's identifier.
    #[test]
    fn sysom_responses_keep_their_id() {
        let parsed = Some(ParsedApiMessage::SysomMessage {
            request: None,
            response: Some(crate::analyzer::message::sysom::SysomResponse {
                id: Some("chatcmpl-sysom-1".to_string()),
                choices: Vec::new(),
            }),
        });
        let http = HttpRecord {
            timestamp_ns: 0,
            pid: 1,
            comm: "t".to_string(),
            method: "POST".to_string(),
            path: "/api/v1/copilot/generate_copilot".to_string(),
            status_code: 200,
            request_headers: "{}".to_string(),
            request_body: None,
            response_headers: "{}".to_string(),
            response_body: None,
            duration_ns: 0,
            first_output_timestamp_ns: None,
            is_sse: false,
            sse_event_count: 0,
        };

        assert_eq!(
            GenAIBuilder::extract_response_id(&parsed.as_ref(), &http).as_deref(),
            Some("chatcmpl-sysom-1")
        );
    }

    #[test]
    fn test_parse_request_body_max_completion_tokens() {
        // Modern OpenAI clients send max_completion_tokens; the o-series
        // models reject the legacy max_tokens spelling outright, so a
        // captured request carries only the new field.
        let body = r#"{
            "model": "o3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_completion_tokens": 2048
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.max_tokens, Some(2048));
    }

    /// A null legacy spelling is not a cap: the chain must keep looking so a
    /// newer key carrying the real cap is not shadowed.
    #[test]
    fn test_parse_request_body_null_max_tokens_falls_back() {
        let body = r#"{
            "model": "o3",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": null,
            "max_completion_tokens": 2048
        }"#;
        let req = GenAIBuilder::parse_request_body(body).expect("body parses");
        assert_eq!(req.max_tokens, Some(2048));
    }

    /// The DashScope/Bailian native protocol nests its sampling parameters
    /// under `parameters`; reading only top level left the output cap and every
    /// other parameter empty for that traffic.
    #[test]
    fn test_parse_request_body_dashscope_native_parameters() {
        let body = r#"{
            "model": "qwen-plus",
            "input": {"messages": [{"role": "user", "content": "hi"}]},
            "parameters": {
                "result_format": "message",
                "max_tokens": 1024,
                "temperature": 0.7,
                "top_p": 0.8,
                "seed": 42,
                "stop": ["END"]
            }
        }"#;

        let req = GenAIBuilder::parse_request_body(body).expect("native body parses");
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.top_p, Some(0.8));
        assert_eq!(req.seed, Some(42));
        assert_eq!(req.stop_sequences, Some(vec!["END".to_string()]));

        // A top-level value still wins over the nested one.
        let body = r#"{
            "model": "qwen-plus",
            "input": {"messages": [{"role": "user", "content": "hi"}]},
            "max_tokens": 7,
            "parameters": {"max_tokens": 1024}
        }"#;
        let req = GenAIBuilder::parse_request_body(body).expect("body parses");
        assert_eq!(req.max_tokens, Some(7));
    }

    /// The Gemini protocol nests its sampling parameters under
    /// `generationConfig` with camelCase keys; reading only the top level and
    /// the DashScope `parameters` object left the output cap invisible, so the
    /// token-limit rules never fired for a stream the provider itself capped
    /// at `maxOutputTokens`.
    #[test]
    fn test_parse_request_body_gemini_generation_config_cap() {
        let body = r#"{
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {"maxOutputTokens": 100}
        }"#;
        let req = GenAIBuilder::parse_request_body(body).expect("gemini body parses");
        assert_eq!(req.max_tokens, Some(100));

        // Without the cap the request stays uncapped.
        let body = r#"{
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "generationConfig": {"temperature": 0.7}
        }"#;
        let req = GenAIBuilder::parse_request_body(body).expect("body parses");
        assert_eq!(req.max_tokens, None);
    }

    #[test]
    fn test_parse_request_body_responses_max_output_tokens() {
        // The Responses API (codex 0.137+ via dashscope /v1/responses)
        // spells the output cap max_output_tokens.
        let body = r#"{
            "model": "gpt-5",
            "input": [{"role": "user", "content": "Hello"}],
            "max_output_tokens": 1024
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.max_tokens, Some(1024));
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
    fn test_parse_request_body_responses_tool_items_without_role() {
        // /v1/responses replays the conversation as typed items; the tool
        // request and its output carry no role, so they must be classified
        // by `type` instead of being dropped.
        let body = r#"{
            "model": "gpt-5",
            "input": [
                {"role": "user", "content": "list /tmp"},
                {"type": "function_call", "call_id": "call_1", "name": "list_dir", "arguments": "{\"path\":\"/tmp\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "a.txt"}
            ]
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();
        assert_eq!(req.messages.len(), 3);

        match &req.messages[1].parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name, "list_dir");
                assert_eq!(
                    arguments.as_ref().unwrap().as_str(),
                    Some(r#"{"path":"/tmp"}"#)
                );
            }
            other => panic!("expected a tool call, got {other:?}"),
        }

        match &req.messages[2].parts[0] {
            MessagePart::ToolCallResponse { id, response } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(response.as_str(), Some("a.txt"));
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_request_body_anthropic_content_blocks() {
        // A request captured verbatim from an interrupted Anthropic call keeps
        // the wire content blocks: the assistant's tool request arrives as a
        // `tool_use` block and the tool output as a `tool_result` block. The
        // converter only read a block's `text`, so both were dropped and the
        // ATIF observation built from this column lost the tool results.
        let body = r#"{
            "model": "claude-opus-4",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/tmp/a"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "file contents"}
                ]}
            ]
        }"#;
        let req = GenAIBuilder::parse_request_body(body).unwrap();

        match &req.messages[0].parts[..] {
            [
                MessagePart::Text { content },
                MessagePart::ToolCall {
                    id,
                    name,
                    arguments,
                },
            ] => {
                assert_eq!(content, "checking");
                assert_eq!(id.as_deref(), Some("toolu_1"));
                assert_eq!(name, "Read");
                assert_eq!(arguments.as_ref().unwrap()["file_path"], "/tmp/a");
            }
            other => panic!("expected a text part and a tool call, got {other:?}"),
        }

        match &req.messages[1].parts[..] {
            [MessagePart::ToolCallResponse { id, response }] => {
                assert_eq!(id.as_deref(), Some("toolu_1"));
                assert_eq!(response.as_str(), Some("file contents"));
            }
            other => panic!("expected the tool result, got {other:?}"),
        }
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

    /// OpenAI reports a safety refusal in a dedicated `delta.refusal` field
    /// (no content delta arrives), so a refusal-only stream produced no parts
    /// at all: both the live path and the drain enrichment persisted the call
    /// with `output_messages = None`.
    #[test]
    fn test_extract_parts_from_sse_body_refusal() {
        let body = r#"[{"choices":[{"delta":{"refusal":"I can't help with that."}}]},{"choices":[{"delta":{},"finish_reason":"stop"}]}]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a refusal must still yield output");
        assert_eq!(parts.len(), 1);
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "I can't help with that.")
        );
        assert_eq!(finish, Some("stop".to_string()));
    }

    /// Legacy models streamed their single tool request as
    /// `delta.function_call` fragments — the name once, then bare argument
    /// fragments — the pre-`tool_calls` spelling the message converters
    /// already read. The merger only read `delta.tool_calls`, so a streamed
    /// legacy call left no ToolCall part on the enrich and drain paths.
    #[test]
    fn legacy_streamed_function_call_becomes_a_tool_call_part() {
        let body = r#"[
            {"choices":[{"delta":{"role":"assistant","function_call":{"name":"get_weather","arguments":""}}}]},
            {"choices":[{"delta":{"function_call":{"arguments":"{\"city\":"}}}]},
            {"choices":[{"delta":{"function_call":{"arguments":"\"Beijing\"}"}}}]},
            {"choices":[{"delta":{},"finish_reason":"function_call"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a streamed legacy call must still yield output");
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, &None);
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, &Some(serde_json::json!({"city": "Beijing"})));
            }
            other => panic!("expected ToolCall part, got {other:?}"),
        }
        assert_eq!(finish.as_deref(), Some("function_call"));
    }

    /// Anthropic SSE bodies carry no `choices` array, so the merger must
    /// aggregate `content_block_start`/`content_block_delta` events instead of
    /// yielding no parts at all. This is the same merger the dead-pid drain
    /// path persists through, so a captured Anthropic body reconstructs its
    /// blocks on both paths.
    #[test]
    fn test_extract_parts_from_sse_body_anthropic_blocks() {
        let body = r#"[
            {"type":"message_start","message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":1234,"output_tokens":1}}},
            {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},
            {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}},
            {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}},
            {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}},
            {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}},
            {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"Paris\"}"}},
            {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(matches!(
            &parts[0],
            MessagePart::Text { content } if content == "Hello"
        ));
        match &parts[1] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("toolu_1"));
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, &Some(serde_json::json!({"city": "Paris"})));
            }
            other => panic!("expected ToolCall part, got {other:?}"),
        }
        assert_eq!(finish.as_deref(), Some("tool_use"));
    }

    /// A compatible gateway may send a content block's complete payload on
    /// `content_block_start` and no deltas at all. The analyzer's typed parser
    /// already seeds its buffers from the start block for that capture shape;
    /// this merger — the only aggregation the drain path has — started every
    /// block empty, so a drained such stream recorded a tool call with
    /// `arguments: None` and dropped its text and thinking while the same
    /// stream captured live kept them.
    #[test]
    fn test_merge_anthropic_sse_chunks_keeps_the_start_block_payload() {
        let body = r#"[
            {"type":"message_start","message":{"id":"msg_9","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":1}}},
            {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"weigh the options"}},
            {"type":"content_block_stop","index":0},
            {"type":"content_block_start","index":1,"content_block":{"type":"text","text":"It is 4."}},
            {"type":"content_block_stop","index":1},
            {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_9","name":"read_file","input":{"path":"/tmp/a"}}},
            {"type":"content_block_stop","index":2},
            {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("an Anthropic stream with content blocks must yield parts");
        assert_eq!(finish.as_deref(), Some("tool_use"));
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { content } if content == "It is 4.")),
            "the text carried by content_block_start must survive: {parts:?}"
        );
        assert!(
            parts.iter().any(
                |p| matches!(p, MessagePart::Reasoning { content } if content == "weigh the options")
            ),
            "the thinking carried by content_block_start must survive: {parts:?}"
        );
        assert!(
            parts.iter().any(|p| matches!(
                p,
                MessagePart::ToolCall { name, arguments, .. }
                    if name == "read_file"
                        && arguments == &Some(serde_json::json!({"path": "/tmp/a"}))
            )),
            "the input carried by content_block_start must survive: {parts:?}"
        );
    }

    /// The placeholder `input: {}` a standard stream sends before its
    /// `input_json_delta` fragments must not become the arguments, and seeding
    /// must not disturb the ordinary delta accumulation.
    #[test]
    fn test_merge_anthropic_sse_chunks_placeholder_input_is_not_arguments() {
        let body = r#"[
            {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},
            {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}},
            {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}},
            {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Paris\"}"}},
            {"type":"message_delta","delta":{"stop_reason":"tool_use"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert!(matches!(
            &parts[0],
            MessagePart::Text { content } if content == "Hel"
        ));
        assert!(matches!(
            &parts[1],
            MessagePart::ToolCall { arguments, .. }
                if arguments == &Some(serde_json::json!({"city": "Paris"}))
        ));
    }

    /// A drain capture often attaches after `content_block_start`. The live
    /// analyzer creates the missing block on the first delta; the drain merger
    /// used to drop every delta whose start was never seen, leaving the call
    /// with no output at all.
    #[test]
    fn test_merge_anthropic_sse_chunks_deltas_without_block_starts() {
        let body = r#"[
            {"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":5}}},
            {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weigh"}},
            {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hello"}},
            {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"/tmp/a\"}"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a delta-only Anthropic capture must still yield parts");
        assert!(matches!(
            &parts[0],
            MessagePart::Reasoning { content } if content == "weigh"
        ));
        assert!(matches!(
            &parts[1],
            MessagePart::Text { content } if content == "hello"
        ));
        assert!(
            parts.iter().any(|p| matches!(
                p,
                MessagePart::ToolCall { arguments, .. }
                    if arguments == &Some(serde_json::json!({"path": "/tmp/a"}))
            )),
            "the streamed tool input must survive: {parts:?}"
        );
    }

    /// `content_block_start` refuses indices at or above
    /// `MAX_TOOL_CALL_SLOTS`; a delta on the same index must not revive the
    /// block the start path rejected.
    #[test]
    fn test_merge_anthropic_sse_chunks_ignores_out_of_range_delta_index() {
        let body = r#"[
            {"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":5}}},
            {"type":"content_block_delta","index":256,"delta":{"type":"text_delta","text":"ghost"}}
        ]"#;
        // The merger falls back when it has no block to report, so `None` and
        // an empty part list both mean the delta was dropped.
        let parts = GenAIBuilder::extract_parts_from_sse_body(body)
            .map(|(parts, _)| parts)
            .unwrap_or_default();
        assert!(
            !parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { content } if content == "ghost")),
            "an out-of-range delta index must be dropped: {parts:?}"
        );
    }

    /// A provider-side call is the same `content_block_start` shape as a
    /// classic `tool_use` under the spellings `server_tool_use` and
    /// `mcp_tool_use`. The typed parser aliases all three; this merger matched
    /// only the literal `tool_use`, so the block fell into the text
    /// accumulator and every `input_json_delta` fragment aimed at its index was
    /// discarded — a drained search-only turn persisted no output at all, while
    /// the same stream captured live kept the call.
    #[test]
    fn test_merge_anthropic_sse_chunks_keeps_a_server_tool_call() {
        let body = r#"[
            {"type":"message_start","message":{"id":"msg_srv","model":"claude-sonnet-4-5","usage":{"input_tokens":10}}},
            {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},
            {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Searching."}},
            {"type":"content_block_stop","index":0},
            {"type":"content_block_start","index":1,"content_block":{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{}}},
            {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\":"}},
            {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"rust 2026\"}"}},
            {"type":"content_block_stop","index":1},
            {"type":"content_block_start","index":2,"content_block":{"type":"mcp_tool_use","id":"mcptoolu_1","name":"github_search","input":{}}},
            {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"q\":\"drain\"}"}},
            {"type":"content_block_stop","index":2},
            {"type":"message_delta","delta":{"stop_reason":"end_turn"}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a stream carrying provider-side calls must yield output");
        assert_eq!(finish.as_deref(), Some("end_turn"));
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "Searching."),
            "the text block must keep its own buffer: {parts:?}"
        );
        assert!(
            parts.iter().any(|p| matches!(
                p,
                MessagePart::ToolCall { id, name, arguments }
                    if id.as_deref() == Some("srvtoolu_1")
                        && name == "web_search"
                        && arguments == &Some(serde_json::json!({"query": "rust 2026"}))
            )),
            "the server-side call and its streamed input must survive: {parts:?}"
        );
        assert!(
            parts.iter().any(|p| matches!(
                p,
                MessagePart::ToolCall { id, name, arguments }
                    if id.as_deref() == Some("mcptoolu_1")
                        && name == "github_search"
                        && arguments == &Some(serde_json::json!({"q": "drain"}))
            )),
            "the MCP call and its streamed input must survive: {parts:?}"
        );
    }

    /// The drain merger and the typed parser must agree on the same stream:
    /// a provider-side call is a `ToolCall` part here and a `ToolUse` block
    /// there, and neither may report it as text.
    #[test]
    fn test_merge_anthropic_sse_chunks_matches_the_typed_parser_on_a_server_tool_call() {
        let body = r#"[
            {"type":"message_start","message":{"id":"msg_parity","model":"claude-sonnet-4-5","usage":{"input_tokens":7}}},
            {"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_9","name":"web_fetch","input":{}}},
            {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"url\":\"https://example.com\"}"}},
            {"type":"content_block_stop","index":0},
            {"type":"message_delta","delta":{"stop_reason":"end_turn"}}
        ]"#;
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        let live = crate::analyzer::message::AnthropicParser::parse_response(&value)
            .expect("the typed parser reads the same stream");
        let live_calls: Vec<(&str, &str, &serde_json::Value)> = live
            .content
            .iter()
            .filter_map(|block| match block {
                crate::analyzer::message::AnthropicContentBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                _ => None,
            })
            .collect();
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("the drain merger reads the same stream");
        let drain_calls: Vec<(&str, &str, &serde_json::Value)> = parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some((
                    id.as_deref().unwrap_or_default(),
                    name.as_str(),
                    arguments.as_ref().expect("the streamed input must parse"),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            live_calls, drain_calls,
            "the drain merger and the typed parser must report the same call"
        );
        assert_eq!(drain_calls.len(), 1, "the call must not be duplicated");
    }

    /// A replayed conversation keeps the assistant's provider-side calls and
    /// the results they produced. The typed parser aliases those spellings;
    /// this reader matched only `tool_use` / `tool_result`, so the replayed
    /// turn extracted to an empty message and the tool interaction vanished
    /// from the request event.
    #[test]
    fn test_parse_request_body_keeps_replayed_server_tool_blocks() {
        let body = r#"{
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "search rust"},
                {"role": "assistant", "content": [
                    {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
                     "input": {"query": "rust"}},
                    {"type": "mcp_tool_use", "id": "mcptoolu_1", "name": "github_search",
                     "input": {"q": "drain"}}
                ]},
                {"role": "user", "content": [
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1",
                     "content": [{"type": "web_search_result", "url": "https://example.com"}]},
                    {"type": "mcp_tool_result", "tool_use_id": "mcptoolu_1",
                     "content": [{"type": "text", "text": "no hits"}]}
                ]}
            ]
        }"#;
        let req = GenAIBuilder::parse_request_body(body).expect("body parses");

        let calls: Vec<(&str, &str, &serde_json::Value)> = req.messages[1]
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some((
                    id.as_deref().unwrap_or_default(),
                    name.as_str(),
                    arguments.as_ref().expect("the replayed input must survive"),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                (
                    "srvtoolu_1",
                    "web_search",
                    &serde_json::json!({"query": "rust"})
                ),
                (
                    "mcptoolu_1",
                    "github_search",
                    &serde_json::json!({"q": "drain"})
                ),
            ],
            "both provider-side calls must survive the replay: {:?}",
            req.messages[1].parts
        );

        let results: Vec<(&str, &serde_json::Value)> = req.messages[2]
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::ToolCallResponse { id, response } => {
                    Some((id.as_deref().unwrap_or_default(), response))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            results.len(),
            2,
            "both results must survive: {:?}",
            req.messages[2].parts
        );
        assert_eq!(results[0].0, "srvtoolu_1");
        assert_eq!(results[1].0, "mcptoolu_1");
    }

    /// A Gemini `streamGenerateContent` body carries no `choices` and no
    /// sentinel frame: each chunk is a standalone generation payload. The
    /// merger must merge text parts across chunks, map whole `functionCall`
    /// parts to tool calls, and take the terminal chunk's finishReason —
    /// previously the body fell through every merger and the call persisted
    /// with no output at all.
    #[test]
    fn test_merge_gemini_sse_chunks_text_calls_and_finish() {
        let body = r#"[
            {"candidates":[{"content":{"role":"model","parts":[{"text":"hel"}]},"index":0}]},
            {"candidates":[{"content":{"role":"model","parts":[{"text":"lo"},{"functionCall":{"name":"read_file","args":{"path":"/tmp/a.md"}}}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":2,"totalTokenCount":11}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "hello"));
        assert!(matches!(
            &parts[1],
            MessagePart::ToolCall { name, arguments, .. }
                if name == "read_file" && arguments == &Some(serde_json::json!({"path": "/tmp/a.md"}))
        ));
        assert_eq!(finish.as_deref(), Some("STOP"));
    }

    /// Gemini thinking models mark reasoning parts with `thought: true`;
    /// those must become Reasoning parts, not body text. An explicit null
    /// finishReason on a middle chunk must not be mistaken for a terminator.
    #[test]
    fn test_merge_gemini_sse_chunks_thought_parts_become_reasoning() {
        let body = r#"[
            {"candidates":[{"content":{"role":"model","parts":[{"text":"thinking aloud","thought":true}]},"finishReason":null,"index":0}]},
            {"candidates":[{"content":{"role":"model","parts":[{"text":"answer"}]},"finishReason":"STOP","index":0}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert!(
            matches!(&parts[0], MessagePart::Reasoning { content } if content == "thinking aloud")
        );
        assert!(matches!(&parts[1], MessagePart::Text { content } if content == "answer"));
        assert_eq!(finish.as_deref(), Some("STOP"));
    }

    /// Non-Gemini shapes must not be claimed by the Gemini merger.
    #[test]
    fn test_merge_gemini_sse_chunks_ignores_foreign_shapes() {
        let chunks = vec![serde_json::json!({"output": {"text": "dashscope"}})];
        assert!(GenAIBuilder::merge_gemini_sse_chunks(&chunks).is_none());
    }

    /// Mixed protocol bodies cannot happen (one stream is one protocol), but a
    /// body whose chunks have `choices` must keep taking the OpenAI walk even
    /// when an Anthropic-shaped event sneaks in, and an Anthropic stream with
    /// no content blocks must degrade to today's behavior (no parts) rather
    /// than inventing any.
    #[test]
    fn test_merge_sse_chunks_anthropic_without_blocks_stays_empty() {
        let chunks: Vec<serde_json::Value> = vec![
            serde_json::json!({"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":1}}}),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        ];
        let (parts, finish) = GenAIBuilder::merge_sse_chunks(&chunks);
        assert!(parts.is_empty(), "no content blocks means no parts");
        assert_eq!(finish.as_deref(), Some("end_turn"));
    }

    /// The Responses protocol reports a refusal through its own streaming
    /// events (`response.refusal.delta`/`done`), which the merger ignored the
    /// same way the chat-completions path ignored `delta.refusal`.
    #[test]
    fn test_extract_parts_from_sse_body_responses_refusal() {
        let body = r#"[
            {"type":"response.created","response":{"id":"resp_r1","model":"gpt-5"}},
            {"type":"response.refusal.delta","delta":"I can't help"},
            {"type":"response.refusal.delta","delta":" with that."},
            {"type":"response.refusal.done","refusal":"I can't help with that."},
            {"type":"response.completed","response":{"id":"resp_r1","status":"completed"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a refusal must still yield output");
        assert_eq!(parts.len(), 1);
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "I can't help with that.")
        );
    }

    /// Responses-API SSE bodies (codex 0.137+ via /v1/responses) carry no
    /// `choices` array either; the merger must aggregate their `response.*`
    /// events instead of yielding no parts. Same merger the drain path
    /// persists through.
    #[test]
    fn test_extract_parts_from_sse_body_responses_events() {
        let body = r#"[
            {"type":"response.created","response":{"id":"resp_1","model":"qwen3-coder-plus"}},
            {"type":"response.output_text.delta","delta":"Hel"},
            {"type":"response.output_text.delta","delta":"lo"},
            {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"read_file"}},
            {"type":"response.function_call_arguments.delta","delta":"{\"path\":"},
            {"type":"response.function_call_arguments.delta","delta":"\"/tmp/a.md\"}"},
            {"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":100,"output_tokens":7,"total_tokens":107}}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(matches!(
            &parts[0],
            MessagePart::Text { content } if content == "Hello"
        ));
        match &parts[1] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name, "read_file");
                assert_eq!(arguments, &Some(serde_json::json!({"path": "/tmp/a.md"})));
            }
            other => panic!("expected ToolCall part, got {other:?}"),
        }
        assert_eq!(finish.as_deref(), Some("tool_calls"));
    }

    /// A capture that attached after the text deltas has only the closing
    /// events. The live parser reads them when no non-empty delta arrived
    /// (`closing_text` in `analyzer/message/openai.rs`), but the drain merger
    /// ignored both closing shapes, so a drained Responses answer persisted
    /// `output_messages = None` while the same capture kept its text live.
    #[test]
    fn test_responses_sse_closing_text_keeps_answer() {
        let body = r#"[
            {"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}},
            {"type":"response.output_text.done","text":"hello there"},
            {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a closing text event must yield an answer");
        assert_eq!(parts.len(), 1);
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "hello there"),
            "got {parts:?}"
        );
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    /// A capture that attached after the item headers has only the finished
    /// `message` item, whose `content[]` carries the whole answer. The token
    /// extractor's `read_output_item` and the live parser both read it; the
    /// drain merger must not drop it.
    #[test]
    fn test_responses_sse_closing_message_item_keeps_answer() {
        let body = r#"[
            {"type":"response.output_item.done","item":{"type":"message","content":[
                {"type":"output_text","text":"hel"},
                {"type":"output_text","text":"lo"}
            ]}},
            {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a finished message item must yield an answer");
        assert_eq!(parts.len(), 1);
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "hello"),
            "got {parts:?}"
        );
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    /// A refusal is the model's answer too, but it rides a `refusal` content
    /// part instead of a `text` part. A capture that attached after the
    /// refusal deltas has only the finished item, so the drain merger must
    /// read that part the way the live parser's `closing_message_item_refusal`
    /// and the token extractor's `read_output_item` do.
    #[test]
    fn test_responses_sse_closing_message_item_keeps_refusal() {
        let body = r#"[
            {"type":"response.output_item.done","item":{"type":"message","content":[
                {"type":"refusal","refusal":"I can't help with that."}
            ]}},
            {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a closing refusal part must yield an answer");
        assert_eq!(parts.len(), 1);
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "I can't help with that."),
            "got {parts:?}"
        );
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    /// The refusal deltas repeat the closing part, so a capture that saw them
    /// must not append the copy on top.
    #[test]
    fn test_responses_sse_refusal_delta_wins_over_the_closing_copy() {
        let body = r#"[
            {"type":"response.refusal.delta","delta":"I can't help"},
            {"type":"response.refusal.delta","delta":" with that."},
            {"type":"response.output_item.done","item":{"type":"message","content":[
                {"type":"refusal","refusal":"I can't help with that."}
            ]}},
            {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "I can't help with that."),
            "deltas must win over their closing copies, got {parts:?}"
        );
    }

    /// A capture that attached after the reasoning deltas has only the
    /// finished `reasoning` items. Both the live parser
    /// (`closing_reasoning_item_text`) and the token extractor's
    /// `read_output_item` read them; the drain merger read only the deltas and
    /// dropped the thinking entirely.
    ///
    /// An agentic turn interleaves one reasoning item per call, and each item
    /// holds different thinking, so every item must be kept in arrival order —
    /// not replaced by the last one.
    #[test]
    fn test_responses_sse_closing_reasoning_items_keep_thinking() {
        let body = r#"[
            {"type":"response.output_item.done","item":{"type":"reasoning","content":[
                {"type":"reasoning_text","text":"first thought"}
            ]}},
            {"type":"response.output_item.done","item":{"type":"reasoning","summary":[
                {"type":"summary_text","text":"second thought"}
            ]}},
            {"type":"response.output_text.done","text":"answer"},
            {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("closing reasoning items must yield output");
        assert!(
            matches!(&parts[0], MessagePart::Reasoning { content }
                if content == "first thoughtsecond thought"),
            "got {parts:?}"
        );
        assert!(matches!(&parts[1], MessagePart::Text { content } if content == "answer"));
    }

    /// The reasoning deltas repeat the closing items, so a capture that saw
    /// them must not append the copies on top.
    #[test]
    fn test_responses_sse_reasoning_delta_wins_over_the_closing_item() {
        let body = r#"[
            {"type":"response.reasoning_summary_text.delta","delta":"think"},
            {"type":"response.output_item.done","item":{"type":"reasoning","summary":[
                {"type":"summary_text","text":"think"}
            ]}},
            {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert!(
            matches!(&parts[0], MessagePart::Reasoning { content } if content == "think"),
            "deltas must win over their closing copies, got {parts:?}"
        );
    }

    /// Closing events repeat the deltas. When the capture saw a non-empty
    /// delta, appending the closing copy would double the answer.
    #[test]
    fn test_responses_sse_text_delta_wins_over_the_closing_copy() {
        let body = r#"[
            {"type":"response.created","response":{"id":"resp_1"}},
            {"type":"response.output_text.delta","delta":"hello"},
            {"type":"response.output_text.done","text":"hello"},
            {"type":"response.output_item.done","item":{"type":"message","content":[
                {"type":"output_text","text":"hello"}
            ]}},
            {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "hello"),
            "deltas must win over their closing copies, got {parts:?}"
        );
    }

    /// A drained Responses stream must keep reasoning deltas the same way the
    /// live parser does (`aggregate_responses_sse_chunks` in
    /// `analyzer/message/openai.rs`); the merger ignored both reasoning event
    /// types, so the persisted `output_messages` lost the thinking while the
    /// same stream captured live kept it as a `Reasoning` part.
    #[test]
    fn test_responses_sse_reasoning_deltas_keep_reasoning_part() {
        let body = r#"[
            {"type":"response.created","response":{"id":"resp_1","model":"qwen3-coder-plus"}},
            {"type":"response.reasoning_summary_text.delta","delta":"think "},
            {"type":"response.reasoning_text.delta","delta":"harder"},
            {"type":"response.output_text.delta","delta":"answer"},
            {"type":"response.completed","response":{"id":"resp_1"}}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 2, "reasoning part + text part");
        assert!(matches!(
            &parts[0],
            MessagePart::Reasoning { content } if content == "think harder"
        ));
        assert!(matches!(
            &parts[1],
            MessagePart::Text { content } if content == "answer"
        ));
    }

    /// A token-capped Responses stream terminates with `response.incomplete`
    /// and `incomplete_details.reason = "max_output_tokens"`. The live parser
    /// maps that to the chat-completions "length"; the merger hardcoded "stop",
    /// so a drained row under-reported truncation.
    #[test]
    fn test_responses_sse_incomplete_max_output_tokens_is_length() {
        let body = r#"[
            {"type":"response.output_text.delta","delta":"partial"},
            {"type":"response.incomplete","response":{"id":"resp_2","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":100,"output_tokens":7,"total_tokens":107}}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(
            &parts[0],
            MessagePart::Text { content } if content == "partial"
        ));
        assert_eq!(finish.as_deref(), Some("length"));
    }

    /// A Responses stream the safety policy cut terminates with
    /// `response.incomplete` and `incomplete_details.reason =
    /// "content_filter"`, and never sends `response.completed`. The merger
    /// read only the output-cap reason, so the finish reason stayed None and
    /// the detector reported `sse_truncated` (high) — a transport failure —
    /// for a stop the provider itself chose. The live parser's aggregator
    /// already maps the reason to the chat-completions "content_filter".
    #[test]
    fn test_responses_sse_incomplete_content_filter_is_filter() {
        let body = r#"[
            {"type":"response.output_text.delta","delta":"partial"},
            {"type":"response.incomplete","response":{"id":"resp_f","status":"incomplete","incomplete_details":{"reason":"content_filter"}}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(
            parts.len(),
            1,
            "the deltas delivered before the cut persist"
        );
        assert!(matches!(
            &parts[0],
            MessagePart::Text { content } if content == "partial"
        ));
        assert_eq!(finish.as_deref(), Some("content_filter"));
    }

    /// OpenAI documents `max_messages` and `steered` as `response.incomplete`
    /// reasons too. They are terminal like the cap/filter reasons, but they
    /// set neither flag; because the arm never set `completed`, the finish
    /// fell through to None and the detector reported a transport truncation
    /// for a turn the provider had ended.
    #[test]
    fn test_responses_sse_incomplete_other_reasons_keep_a_finish() {
        for reason in ["max_messages", "steered"] {
            let body = format!(
                r#"[
                    {{"type":"response.output_text.delta","delta":"partial"}},
                    {{"type":"response.incomplete","response":{{"id":"resp_o","status":"incomplete","incomplete_details":{{"reason":"{reason}"}}}}}}
                ]"#
            );
            let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(&body).unwrap();
            assert_eq!(parts.len(), 1, "{reason}: the answer must survive");
            assert_eq!(
                finish.as_deref(),
                Some("stop"),
                "{reason}: a provider-terminated turn must not look truncated"
            );
        }
    }

    /// The cap reason keeps its "length" mapping, and a stream that ends
    /// with `response.completed` is untouched by the incomplete arms.
    #[test]
    fn test_responses_sse_incomplete_reasons_stay_distinct() {
        let body = r#"[
            {"type":"response.output_text.delta","delta":"partial"},
            {"type":"response.incomplete","response":{"id":"resp_c","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}
        ]"#;
        let (_, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(finish.as_deref(), Some("length"));

        let body = r#"[
            {"type":"response.output_text.delta","delta":"done"},
            {"type":"response.completed","response":{"id":"resp_c","status":"completed"}}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(matches!(
            &parts[0],
            MessagePart::Text { content } if content == "done"
        ));
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    /// A stream whose capture never saw a terminal event (dead-pid drain of
    /// an interrupted call) keeps an unknown finish instead of a fabricated
    /// clean stop, and an in-flight tool call still flushes with its partial
    /// arguments.
    #[test]
    fn test_extract_parts_from_sse_body_responses_truncated_flushes_call() {
        let body = r#"[
            {"type":"response.created","response":{"id":"resp_3"}},
            {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_t","name":"read_file"}},
            {"type":"response.function_call_arguments.delta","delta":"{\"path\":"}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_t"));
                assert_eq!(name, "read_file");
                // Arguments cut mid-JSON stay a None payload, not a lossy
                // partial parse.
                assert!(arguments.is_none());
            }
            other => panic!("expected ToolCall part, got {other:?}"),
        }
        assert!(finish.is_none(), "no terminal event means no finish reason");
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

    /// Compatible providers stream `tool_calls` deltas without an `index` and
    /// mark each call with its name, the same shape the token-side
    /// reconstruction and the analyzer's message parser already route into
    /// separate calls. Keying every index-less delta to slot 0 overwrote the
    /// first call's id and name with the second's and concatenated both
    /// argument fragments into one invalid JSON string.
    #[test]
    fn test_extract_parts_from_sse_body_unindexed_tool_calls() {
        let body = r#"[
            {"choices":[{"delta":{"tool_calls":[{"id":"call_a","function":{"name":"read_file","arguments":"{\"path\":"}}]},"index":0}]},
            {"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"\"/tmp/a.md\"}"}}]},"index":0}]},
            {"choices":[{"delta":{"tool_calls":[{"id":"call_b","function":{"name":"write_file","arguments":"{\"path\":\"/tmp/b.md\"}"}}]},"index":0,"finish_reason":"tool_calls"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(
            parts.len(),
            2,
            "a named fragment starts a new call; the second must not overwrite the first: {parts:?}"
        );
        match &parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_a"));
                assert_eq!(name, "read_file");
                assert_eq!(arguments.as_ref().unwrap()["path"], "/tmp/a.md");
            }
            _ => panic!("expected ToolCall"),
        }
        match &parts[1] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_b"));
                assert_eq!(name, "write_file");
                assert_eq!(arguments.as_ref().unwrap()["path"], "/tmp/b.md");
            }
            _ => panic!("expected ToolCall"),
        }
        assert_eq!(finish, Some("tool_calls".to_string()));
    }

    /// OpenAI scopes `tool_calls[].index` to its choice: every choice starts
    /// its calls at index 0. Keying the merger by the wire index alone folded
    /// two choices' first calls into one slot, so a multi-choice stream
    /// reported one call with the later choice's id/name and both fragments
    /// spliced into unparseable arguments.
    #[test]
    fn test_extract_parts_from_sse_body_multi_choice_tool_calls() {
        let body = r#"[
            {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"first","arguments":"{\"a\":1}"}}]}}]},
            {"choices":[{"index":1,"delta":{"tool_calls":[{"index":0,"id":"call_b","function":{"name":"second","arguments":"{\"b\":2}"}}]},"finish_reason":"tool_calls"}]}
        ]"#;
        let (parts, _) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        let calls: Vec<(&str, Option<&serde_json::Value>)> = parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::ToolCall {
                    name, arguments, ..
                } => Some((name.as_str(), arguments.as_ref())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2, "one call per choice, got {parts:?}");
        assert_eq!(calls[0].0, "first");
        assert_eq!(calls[0].1, Some(&serde_json::json!({"a": 1})));
        assert_eq!(calls[1].0, "second");
        assert_eq!(calls[1].1, Some(&serde_json::json!({"b": 2})));
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

    /// `index` is wire input on the delta path too: a value outside the slot
    /// range must not be truncated into another slot, which would overwrite a
    /// valid tool call's id, name and arguments in the reconstructed message.
    #[test]
    fn test_extract_parts_from_sse_body_rejects_absurd_tool_call_index() {
        let body = r#"[
            {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"alpha","arguments":"{\"x\":1}"}}]}}]},
            {"choices":[{"delta":{"tool_calls":[{"index":4294967296,"id":"call_b","type":"function","function":{"name":"beta","arguments":"{\"y\":2}"}}]}}]},
            {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();

        assert_eq!(
            parts.len(),
            1,
            "the out-of-range index must not be merged into slot 0: {parts:?}"
        );
        match &parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_a"));
                assert_eq!(name, "alpha");
                assert_eq!(arguments.as_ref().unwrap()["x"], 1);
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
    /// A choice may carry a cumulative `message` snapshot instead of a `delta`.
    /// SysOM's Copilot stream always does, and some OpenAI-compatible gateways
    /// send one as the final frame; reading only `delta` dropped those frames
    /// entirely — their text, and the `finish_reason` beside them.
    #[test]
    fn test_merge_sse_chunks_reads_message_snapshots() {
        let body = r#"[
            {"choices":[{"message":{"content":"Hello"}}]},
            {"choices":[{"message":{"content":"Hello there!"}}]},
            {"choices":[{"message":{"content":"Hello there!"},"finish_reason":"stop"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(finish, Some("stop".to_string()));
        assert_eq!(parts.len(), 1);
        // The snapshot repeats the answer so far: the last one wins instead of
        // the three being concatenated.
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "Hello there!"),
            "{parts:?}"
        );
    }

    /// The final OpenAI-compatible frame can be a whole `message` after a run of
    /// `delta` frames; the snapshot completes the text and carries the finish
    /// reason the deltas never did.
    #[test]
    fn test_merge_sse_chunks_finishes_a_delta_stream_with_a_message_snapshot() {
        let body = r#"[
            {"choices":[{"delta":{"content":"Do"},"finish_reason":null}]},
            {"choices":[{"message":{"content":"Done"},"finish_reason":"stop"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(finish, Some("stop".to_string()));
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "Done"),
            "{parts:?}"
        );
    }

    /// A choice that carries neither member still reports its finish reason
    /// instead of being skipped whole.
    #[test]
    fn test_merge_sse_chunks_reads_a_payloadless_finish_reason() {
        let body = r#"[
            {"choices":[{"delta":{"content":"hi"},"finish_reason":null}]},
            {"choices":[{"index":0,"finish_reason":"length"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        assert_eq!(finish, Some("length".to_string()));
        assert!(matches!(&parts[0], MessagePart::Text { content } if content == "hi"));
    }

    /// Legacy `/v1/completions` streams carry each fragment on the choice
    /// itself, with no `delta` or `message` object. `merge_sse_chunks` skipped
    /// every such chunk, so a drained legacy stream produced no output
    /// message at all while the token extractor counted its text.
    #[test]
    fn test_merge_sse_chunks_reads_legacy_completion_text() {
        let body = r#"[
            {"id":"cmpl-1","object":"text_completion","choices":[{"index":0,"text":"The capital of "}]},
            {"id":"cmpl-1","object":"text_completion","choices":[{"index":0,"text":"France is Paris.","finish_reason":"stop"}]}
        ]"#;
        let (parts, finish) = GenAIBuilder::extract_parts_from_sse_body(body)
            .expect("a drained legacy stream must produce output parts");
        assert_eq!(finish, Some("stop".to_string()));
        assert!(
            matches!(&parts[0], MessagePart::Text { content } if content == "The capital of France is Paris."),
            "{parts:?}"
        );
    }

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

    /// The non-streaming body carries the refusal in `message.refusal`; the
    /// conversion read only content/reasoning/tool_calls, so the refusal text
    /// was dropped from the recorded output.
    #[test]
    fn test_openai_msg_to_output_refusal() {
        let msg = OpenAIChatMessage {
            role: crate::analyzer::message::types::MessageRole::Assistant,
            content: None,
            reasoning_content: None,
            refusal: Some("I can't help with that.".to_string()),
            function_call: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            annotations: None,
            audio: None,
        };
        let output = GenAIBuilder::openai_msg_to_output(&msg, Some("stop"));
        assert_eq!(output.parts.len(), 1);
        assert!(
            matches!(&output.parts[0], MessagePart::Text { content } if content == "I can't help with that.")
        );
    }
}
