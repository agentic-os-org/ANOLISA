//! GenAI LLM-call construction
//!
//! Builds `LLMCall` / `LLMRequest` / `LLMResponse` from `AnalysisResult`,
//! including OpenAI-specific SSE aggregation and tool-call delta merging.
//! Logic preserved verbatim from the original `builder.rs`; only visibility
//! widened to `pub(super)` so `builder.rs` can call `build_llm_call`.

use super::GenAIBuilder;
use super::helpers::PidAgentNameCache;
use super::semantic::{
    InputMessage, LLMCall, LLMRequest, LLMResponse, MessagePart, OutputMessage, TokenUsage,
};
use crate::analyzer::{AnalysisResult, HttpRecord, ParsedApiMessage, TokenRecord};
use crate::response_map::ResponseSessionMapper;
use std::collections::HashMap;

impl GenAIBuilder {
    /// Build LLMCall from analysis results
    ///
    /// Combines data from TokenRecord, HttpRecord, and ParsedApiMessage
    pub(super) fn build_llm_call(
        &self,
        results: &[AnalysisResult],
        response_mapper: &ResponseSessionMapper,
        pid_agent_name_cache: &impl PidAgentNameCache,
    ) -> Option<LLMCall> {
        // Extract components from analysis results
        let token_record = results.iter().find_map(|r| match r {
            AnalysisResult::Token(t) => Some(t.clone()),
            _ => None,
        });

        let http_record = results.iter().find_map(|r| match r {
            AnalysisResult::Http(h) => Some(h.clone()),
            _ => None,
        });

        let parsed_message = results.iter().find_map(|r| match r {
            AnalysisResult::Message(m) => Some(m.clone()),
            _ => None,
        });

        // Need at least HttpRecord to build LLMCall
        let http = http_record?;
        let pending_match_key = Self::pending_match_key(
            http.pid,
            http.timestamp_ns,
            &http.method,
            &http.path,
            http.request_body.as_deref(),
        );

        // Check if this is an LLM API call (path-based or body-based for SysOM POP API)
        let path_match = self.is_llm_api_path(&http.path);
        let body_match = !path_match && Self::is_sysom_pop_request(&http.request_body);
        let is_llm = path_match || body_match;
        if !is_llm && !http.is_sse {
            return None;
        }

        let internal_id = self.generate_id();

        // Build request from parsed message or HTTP record
        let request = self.build_request(&parsed_message, &http);
        // Build response from parsed message or HTTP record
        let mut response = self.build_response(&parsed_message, &http, &token_record);

        // Recover empty tool_call names from request tools definitions.
        // DashScope/Qwen gateways sometimes omit function.name entirely from
        // all SSE delta chunks, leaving the merged name as "". When the
        // request's tools array is available, back-fill from it.
        Self::recover_empty_tool_call_names(&request, &mut response);

        // Build token usage from TokenRecord
        let token_usage = token_record.as_ref().map(|t| {
            TokenUsage {
                input_tokens: t.input_tokens as u32,
                output_tokens: t.output_tokens as u32,
                // Whether the cache counters belong in the total depends on the
                // provider, so let TokenRecord own that rule.
                total_tokens: t.total_tokens() as u32,
                cache_creation_input_tokens: t.cache_creation_tokens.map(|v| v as u32),
                cache_read_input_tokens: t.cache_read_tokens.map(|v| v as u32),
            }
        });

        // Determine provider and model
        // Priority: path-based (most specific) > body-based > parsed_message > token_record
        let provider = self
            .extract_provider_from_path(&http.path)
            .or_else(|| Self::extract_provider_from_body(&http.request_body))
            .or_else(|| parsed_message.as_ref().map(|m| m.provider().to_string()))
            .or_else(|| token_record.as_ref().map(|t| t.provider.clone()))
            .unwrap_or_else(|| "unknown".to_string());

        // Model priority: parsed_message (most accurate) > token_record > body extraction
        let model = self
            .extract_model_from_message(&parsed_message)
            .or_else(|| {
                token_record
                    .as_ref()
                    .and_then(|t| t.model.as_ref().filter(|m| !m.is_empty()).cloned())
            })
            .or_else(|| Self::extract_model_from_body(&http.request_body, &http.response_body))
            .unwrap_or_else(|| "unknown".to_string());

        // 在 request move 之前提取用户查询 / first&last user message 原文
        let user_query = Self::extract_last_user_query(&request);
        let first_user_raw = Self::extract_first_user_raw(&request).unwrap_or_default();
        let last_user_raw = Self::extract_last_user_raw(&request).unwrap_or_default();
        let user_message_count = Self::count_real_user_messages(&request);

        // Classify call kind (main / recap / web_search) from request content
        let call_kind = super::helpers::classify_call_kind(&request).as_str();

        // 提取 LLM API 的 response_id（如 chatcmpl-xxx），用作 trace_id
        // 同时作为 call_id 的首选值：trace_id 有值时直接复用，避免两套 ID；
        // SysOM / 解析失败等无 response_id 的场景 fallback 到内部生成的 internal_id。
        let response_id = Self::extract_response_id(&parsed_message, &http);
        let call_id = response_id.clone().unwrap_or_else(|| internal_id.clone());
        let response_id = response_id.unwrap_or_else(|| call_id.clone());

        // 提前解析 agent_name，后面调用 IdResolver 需要它作为 LRU key 维度，
        // 避免同机不同 agent 在同一 user query 下撞同一 session_id。
        let agent_name = Self::resolve_agent_name(&http.comm, http.pid, pid_agent_name_cache)
            .unwrap_or_else(|| http.comm.clone());
        let pid_i32 = http.pid as i32;

        // session_id: 优先从 agent 自身的 session 获取（通过 response ID → .jsonl UUID 映射），
        // 未命中时再尝试 pid → .jsonl UUID 映射（Codex CLI 等 rollout 文件不内嵌 response_id 的场景），
        // 仍未命中则 fallback 到 `SHA256("session" + 该 session 内最早 response_id)`。
        let metadata_session = parsed_message
            .as_ref()
            .and_then(|m| m.request_metadata_session_id());
        let parsed_response_id = parsed_message
            .as_ref()
            .and_then(|m| m.response_id())
            .map(|s| s.to_string());
        let mapper_session = parsed_response_id
            .as_deref()
            .and_then(|rid| response_mapper.get_session_by_response_id(rid))
            .or_else(|| response_mapper.get_session_by_pid(http.pid))
            .map(|s| s.to_string());
        let session_id = metadata_session.or(mapper_session).or_else(|| {
            self.id_resolver
                .resolve_session_id(&agent_name, pid_i32, &first_user_raw, &response_id)
        });

        // conversation_id: SHA256("conversation" + 该 conversation 内最早 response_id)
        let conversation_id = self.id_resolver.resolve_conversation_id(
            &agent_name,
            pid_i32,
            &last_user_raw,
            &response_id,
            user_message_count,
        );

        // 若本次响应的 finish_reason 表明本轮对话已经结束（非 tool_calls/tool_use 等
        // 表示流程尚在继续的取值），显式驱逐 (agent_name, pid, last_user_raw) 对应的
        // conversation anchor。避免固定模板文本（如系统 recap nudge）被复用于不同真实
        // 对话轮次时，因永久锚定而得到相同的 conversation_id（即下游的 turn.id），
        // 进而导致下游按 turn.id 分组的 step 计数逻辑误将新轮对话归入旧轮。
        if let Some(reason) = response
            .messages
            .last()
            .and_then(|m| m.finish_reason.as_deref())
        {
            if Self::is_turn_terminal_finish_reason(reason) {
                // Some providers (e.g. DashScope with forced tool_choice) report
                // finish_reason "stop" even when the response carries tool calls.
                // A response that still asks for tool execution means the turn is
                // not over, so keep the conversation anchor alive.
                // Both finish_reason and the tool-call probe read the last output
                // message: for multi-choice responses the last choice is decisive,
                // matching the SSE aggregation semantics (finish_reason comes from
                // the last chunk).
                let has_tool_call = response
                    .messages
                    .last()
                    .map(|m| {
                        m.parts
                            .iter()
                            .any(|p| matches!(p, MessagePart::ToolCall { .. }))
                    })
                    .unwrap_or(false);
                if !has_tool_call {
                    self.id_resolver.finish_conversation(
                        &agent_name,
                        pid_i32,
                        &last_user_raw,
                        user_message_count,
                    );
                }
            }
        }

        // Extract error message from response body when status_code >= 400
        let error = if http.status_code >= 400 {
            http.response_body.as_ref().and_then(|body| {
                /// Strip HTTP chunked transfer encoding (e.g. "b6\r\n{json}\r\n0\r\n\r\n")
                /// and return the JSON object substring.
                fn strip_chunked(body: &str) -> &str {
                    // Find the first '{' — everything before it may be chunk-size hex + CRLF
                    let start = match body.find('{') {
                        Some(idx) => idx,
                        None => return body,
                    };
                    // Find the last '}' — everything after it is chunked trailer
                    let end = match body.rfind('}') {
                        Some(idx) => idx + 1,
                        None => return &body[start..],
                    };
                    &body[start..end]
                }

                /// Try to extract `message` from a JSON value (handles nested / escaped JSON)
                fn extract_message(v: &serde_json::Value) -> Option<String> {
                    if let Some(e) = v.get("error") {
                        if e.is_object() {
                            // {"error":{"message":"..."}}
                            if let Some(msg) = e.get("message").and_then(|m| m.as_str()) {
                                return Some(msg.to_string());
                            }
                        } else if let Some(s) = e.as_str() {
                            // {"error": "{\"error\":{\"message\":\"...\"}}"}  — escaped JSON string
                            if let Ok(inner) = serde_json::from_str::<serde_json::Value>(s) {
                                if let Some(msg) = inner.get("message").and_then(|m| m.as_str()) {
                                    return Some(msg.to_string());
                                }
                                if let Some(inner_e) = inner.get("error") {
                                    if let Some(msg) =
                                        inner_e.get("message").and_then(|m| m.as_str())
                                    {
                                        return Some(msg.to_string());
                                    }
                                }
                            }
                            return Some(s.to_string());
                        }
                    }
                    // Top-level {"message":"..."}
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                }

                let json_str = strip_chunked(body);
                serde_json::from_str::<serde_json::Value>(json_str)
                    .ok()
                    .and_then(|v| extract_message(&v))
                    .or_else(|| Some(body.clone()))
            })
        } else {
            None
        };

        Some(LLMCall {
            call_id,
            start_timestamp_ns: http.timestamp_ns,
            end_timestamp_ns: http.timestamp_ns + http.duration_ns,
            duration_ns: http.duration_ns,
            provider,
            model,
            request,
            response,
            token_usage,
            error,
            pid: pid_i32,
            // Process name = the *process* comm (`<procfs root>/<pid>/comm`), not
            // the SSL event's per-event thread comm (which may be a library
            // worker-thread name such as "HTTP client"). Falls back to the event
            // comm only when the entry is unreadable (process already gone).
            process_name: crate::discovery::scanner::read_comm(http.pid)
                .unwrap_or_else(|| http.comm.clone()),
            agent_name: Some(agent_name.clone()),
            metadata: {
                let mut meta = HashMap::new();
                meta.insert("method".to_string(), http.method);
                meta.insert("path".to_string(), http.path.clone());
                meta.insert("status_code".to_string(), http.status_code.to_string());
                meta.insert("is_sse".to_string(), http.is_sse.to_string());
                if let Some(timestamp_ns) = http.first_output_timestamp_ns {
                    meta.insert(
                        "first_output_timestamp_ns".to_string(),
                        timestamp_ns.to_string(),
                    );
                }
                meta.insert(
                    "sse_event_count".to_string(),
                    http.sse_event_count.to_string(),
                );
                // Extract server.address and server.port from Host header
                if let Ok(headers) =
                    serde_json::from_str::<HashMap<String, String>>(&http.request_headers)
                {
                    if let Some(host) = headers.get("host").or_else(|| headers.get("Host")) {
                        if let Some((addr, port)) = host.rsplit_once(':') {
                            meta.insert("server.address".to_string(), addr.to_string());
                            meta.insert("server.port".to_string(), port.to_string());
                        } else {
                            meta.insert("server.address".to_string(), host.clone());
                        }
                    }
                }
                if let Some(addr) = meta.get("server.address").cloned() {
                    meta.insert("http.domain".to_string(), addr);
                }
                // Derive gen_ai.operation.name from path
                if http.path.contains("/chat/completions") || http.path.contains("/v1/messages") {
                    meta.insert("operation_name".to_string(), "chat".to_string());
                } else if http.path.contains("/completions") {
                    meta.insert("operation_name".to_string(), "text_completion".to_string());
                } else if http.path.contains("/api/v1/copilot/generate_copilot") {
                    meta.insert("operation_name".to_string(), "chat".to_string());
                }
                // conversation_id: 对话ID，同一 user query 触发的所有调用共享
                if let Some(ref cid) = conversation_id {
                    meta.insert("conversation_id".to_string(), cid.clone());
                }
                // response_id: LLM API 返回的响应 ID，用作 trace_id
                meta.insert("response_id".to_string(), response_id);
                // user_query: 用户实际输入的原文
                if let Some(ref q) = user_query {
                    meta.insert("user_query".to_string(), q.clone());
                }
                // session_id: 同一 agent 进程的完整会话标识
                if let Some(ref sid) = session_id {
                    meta.insert("session_id".to_string(), sid.clone());
                }
                meta.insert("pending_match_key".to_string(), pending_match_key);
                // call_kind: main / recap / web_search
                meta.insert("call_kind".to_string(), call_kind.to_string());
                meta
            },
        })
    }

    /// Returns whether a `finish_reason` value *by itself* marks the end of a turn.
    ///
    /// `tool_calls` (OpenAI) / `tool_use` (Anthropic) / `function_call` (legacy
    /// OpenAI) mean the model requested tool execution; `pause_turn` (Anthropic)
    /// means a long-running turn was paused and will resume. Anything else
    /// (`stop`, `end_turn`, `length`, ...) is treated as terminal.
    ///
    /// Note: the eviction site additionally checks whether the last assistant
    /// message still carries a `MessagePart::ToolCall`; if so, the conversation
    /// anchor is kept alive even for a terminal `finish_reason` (some providers,
    /// e.g. DashScope with forced `tool_choice`, report `"stop"` for tool calls).
    fn is_turn_terminal_finish_reason(reason: &str) -> bool {
        !matches!(
            reason.to_ascii_lowercase().as_str(),
            "tool_calls" | "tool_use" | "function_call" | "pause_turn"
        )
    }

    /// Back-fill empty tool_call names in the response from the request's
    /// `tools` definitions.
    ///
    /// Some OpenAI-compatible gateways (DashScope/Qwen) omit `function.name`
    /// from all SSE delta chunks for a tool_call, leaving the merged name as
    /// `""`. When the request carries a `tools` array we can recover:
    ///   - single tool defined → use its name unconditionally
    ///   - multiple tools → match by comparing the tool_call's argument keys
    ///     against each tool's `parameters.properties` keys
    fn recover_empty_tool_call_names(request: &LLMRequest, response: &mut LLMResponse) {
        let tools = match request.tools.as_ref() {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };

        // Extract (name, property_keys) from each tool definition
        let tool_info: Vec<(String, Vec<String>)> = tools
            .iter()
            .filter_map(|t| {
                let func = t.get("function")?;
                let name = func.get("name")?.as_str()?.to_string();
                let prop_keys: Vec<String> = func
                    .get("parameters")
                    .and_then(|p| p.get("properties"))
                    .and_then(|p| p.as_object())
                    .map(|obj| obj.keys().cloned().collect())
                    .unwrap_or_default();
                Some((name, prop_keys))
            })
            .collect();

        if tool_info.is_empty() {
            return;
        }

        for msg in &mut response.messages {
            for part in &mut msg.parts {
                let MessagePart::ToolCall {
                    name, arguments, ..
                } = part
                else {
                    continue;
                };
                if !name.is_empty() {
                    continue;
                }

                if tool_info.len() == 1 {
                    *name = tool_info[0].0.clone();
                    continue;
                }

                // Multiple tools: match by argument keys
                let call_keys: Vec<String> = arguments
                    .as_ref()
                    .and_then(|a| a.as_object())
                    .map(|obj| obj.keys().cloned().collect())
                    .unwrap_or_default();

                if call_keys.is_empty() {
                    continue;
                }

                // Pick the tool whose property keys are a superset of the call's keys
                let matched: Vec<&str> = tool_info
                    .iter()
                    .filter(|(_, prop_keys)| {
                        !prop_keys.is_empty() && call_keys.iter().all(|k| prop_keys.contains(k))
                    })
                    .map(|(n, _)| n.as_str())
                    .collect();

                if matched.len() == 1 {
                    *name = matched[0].to_string();
                }
                // 0 or >1 matches: leave empty (abstain, don't guess)
            }
        }
    }

    /// Build LLMRequest from parsed message or HTTP record
    fn build_request(&self, message: &Option<ParsedApiMessage>, http: &HttpRecord) -> LLMRequest {
        match message {
            Some(ParsedApiMessage::OpenAICompletion { request, .. }) => {
                if let Some(req) = request.as_ref() {
                    let msgs = req.messages.iter().map(Self::openai_msg_to_input).collect();
                    return LLMRequest {
                        messages: msgs,
                        temperature: req.temperature,
                        max_tokens: req.max_tokens,
                        frequency_penalty: req.frequency_penalty,
                        presence_penalty: req.presence_penalty,
                        top_p: req.top_p,
                        top_k: None,
                        seed: req.seed,
                        stop_sequences: req.stop.clone(),
                        stream: req.stream.unwrap_or(false),
                        tools: req.tools.clone(),
                        raw_body: http.request_body.clone(),
                    };
                }
            }
            Some(ParsedApiMessage::AnthropicMessage { request, .. }) => {
                if let Some(req) = request.as_ref() {
                    let mut msgs: Vec<InputMessage> = Vec::new();
                    // Anthropic carries the system prompt at the top-level
                    // "system" field, not in the messages array. Inject it
                    // as a synthetic system-role message so downstream
                    // system_instructions extraction (SQLite storage, SLS
                    // upload, call classification) picks it up.
                    if let Some(system) = req.system.as_ref() {
                        let text = system.as_text();
                        if !text.is_empty() {
                            msgs.push(InputMessage {
                                role: "system".to_string(),
                                parts: vec![MessagePart::Text { content: text }],
                                name: None,
                            });
                        }
                    }
                    msgs.extend(req.messages.iter().map(|m| {
                        let role = format!("{:?}", m.role).to_lowercase();
                        InputMessage {
                            role,
                            parts: Self::anthropic_message_content_to_parts(&m.content),
                            name: None,
                        }
                    }));
                    return LLMRequest {
                        messages: msgs,
                        temperature: req.temperature,
                        max_tokens: Some(req.max_tokens),
                        frequency_penalty: None,
                        presence_penalty: None,
                        top_p: req.top_p,
                        top_k: req.top_k.map(|v| v as f64),
                        seed: None,
                        stop_sequences: req.stop_sequences.clone(),
                        stream: req.stream.unwrap_or(false),
                        tools: req.tools.clone(),
                        raw_body: http.request_body.clone(),
                    };
                }
            }
            Some(ParsedApiMessage::SysomMessage { request, .. }) => {
                if let Some(req) = request.as_ref() {
                    let msgs = req
                        .params
                        .messages
                        .iter()
                        .map(|m| {
                            let role = m.role.clone();
                            let mut parts = Vec::new();
                            if role == "tool" {
                                let response_val =
                                    serde_json::from_str::<serde_json::Value>(&m.content)
                                        .unwrap_or_else(|_| {
                                            serde_json::Value::String(m.content.clone())
                                        });
                                parts.push(MessagePart::ToolCallResponse {
                                    id: m.tool_call_id.clone(),
                                    response: response_val,
                                });
                            } else if !m.content.is_empty() {
                                parts.push(MessagePart::Text {
                                    content: m.content.clone(),
                                });
                            }
                            if let Some(ref tool_calls) = m.tool_calls {
                                for tc in tool_calls {
                                    let arguments = serde_json::from_str::<serde_json::Value>(
                                        &tc.function.arguments,
                                    )
                                    .ok();
                                    parts.push(MessagePart::ToolCall {
                                        id: Some(tc.id.clone()),
                                        name: tc.function.name.clone(),
                                        arguments,
                                    });
                                }
                            }
                            InputMessage {
                                role,
                                parts,
                                name: m.name.clone(),
                            }
                        })
                        .collect();
                    return LLMRequest {
                        messages: msgs,
                        temperature: req.params.temperature,
                        max_tokens: req.params.max_tokens,
                        frequency_penalty: None,
                        presence_penalty: None,
                        top_p: req.params.top_p,
                        top_k: None,
                        seed: None,
                        stop_sequences: None,
                        stream: req.params.stream,
                        tools: req.params.tools.clone(),
                        raw_body: http.request_body.clone(),
                    };
                }
            }
            _ => {}
        }

        // Fallback: no parsed message — parse request_body directly
        if let Some(ref body) = http.request_body {
            if let Some(req) = Self::parse_request_body(body) {
                return req;
            }
        }
        LLMRequest {
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
            raw_body: http.request_body.clone(),
        }
    }

    // parse_request_body / extract_response_id / openai_msg_to_input /
    // openai_msg_to_output / parse_openai_tool_call_value / parse_sse_response_body /
    // extract_parts_from_sse_body live in `openai_parse.rs` (same impl block).

    fn anthropic_message_content_to_parts(
        content: &crate::analyzer::message::AnthropicMessageContent,
    ) -> Vec<MessagePart> {
        match content {
            crate::analyzer::message::AnthropicMessageContent::Text(text) => {
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![MessagePart::Text {
                        content: text.clone(),
                    }]
                }
            }
            crate::analyzer::message::AnthropicMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(Self::anthropic_content_block_to_part)
                .collect(),
        }
    }

    fn anthropic_content_block_to_part(
        block: &crate::analyzer::message::AnthropicContentBlock,
    ) -> Option<MessagePart> {
        match block {
            crate::analyzer::message::AnthropicContentBlock::Text { text, .. } => {
                if text.is_empty() {
                    None
                } else {
                    Some(MessagePart::Text {
                        content: text.clone(),
                    })
                }
            }
            crate::analyzer::message::AnthropicContentBlock::ToolUse { id, name, input } => {
                Some(MessagePart::ToolCall {
                    id: Some(id.clone()),
                    name: name.clone(),
                    arguments: Some(input.clone()),
                })
            }
            crate::analyzer::message::AnthropicContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let response = match (content.clone(), *is_error) {
                    (Some(value), Some(is_error)) => {
                        serde_json::json!({"content": value, "is_error": is_error})
                    }
                    (Some(value), None) => value,
                    (None, Some(is_error)) => serde_json::json!({"is_error": is_error}),
                    (None, None) => serde_json::Value::Null,
                };
                Some(MessagePart::ToolCallResponse {
                    id: Some(tool_use_id.clone()),
                    response,
                })
            }
            crate::analyzer::message::AnthropicContentBlock::Thinking { thinking, .. } => {
                if thinking.is_empty() {
                    None
                } else {
                    Some(MessagePart::Reasoning {
                        content: thinking.clone(),
                    })
                }
            }
            _ => None,
        }
    }

    /// Build LLMResponse from parsed message or HTTP record
    fn build_response(
        &self,
        message: &Option<ParsedApiMessage>,
        http: &HttpRecord,
        _token_record: &Option<TokenRecord>,
    ) -> LLMResponse {
        // Try to extract from parsed message first
        let (messages, finish_reason): (Vec<OutputMessage>, Option<String>) = match message {
            Some(ParsedApiMessage::OpenAICompletion { response, .. }) => response
                .as_ref()
                .map(|resp| {
                    let msgs: Vec<OutputMessage> = resp
                        .choices
                        .iter()
                        .map(|c| Self::openai_msg_to_output(&c.message, c.finish_reason.as_deref()))
                        .collect();
                    let finish = resp.choices.first().and_then(|c| c.finish_reason.clone());
                    (msgs, finish)
                })
                .unwrap_or_else(|| (vec![], None)),
            Some(ParsedApiMessage::AnthropicMessage { response, .. }) => {
                response
                    .as_ref()
                    .map(|resp| {
                        let mut parts = Vec::new();
                        for block in &resp.content {
                            match block {
                                crate::analyzer::message::AnthropicContentBlock::Text {
                                    text,
                                    ..
                                } => {
                                    if !text.is_empty() {
                                        parts.push(MessagePart::Text {
                                            content: text.clone(),
                                        });
                                    }
                                }
                                crate::analyzer::message::AnthropicContentBlock::ToolUse {
                                    id,
                                    name,
                                    input,
                                } => {
                                    // Anthropic tool_use: convert to MessagePart::ToolCall
                                    parts.push(MessagePart::ToolCall {
                                        id: Some(id.clone()),
                                        name: name.clone(),
                                        arguments: Some(input.clone()),
                                    });
                                }
                                crate::analyzer::message::AnthropicContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                    is_error,
                                } => {
                                    // Anthropic tool_result: convert to MessagePart::ToolCallResponse.
                                    // `is_error` must ride along inside the wrapper so the ATIF
                                    // converter can preserve it as a structured failure signal.
                                    let response_val = match (content.clone(), *is_error) {
                                        (Some(value), Some(flag)) => {
                                            serde_json::json!({"content": value, "is_error": flag})
                                        }
                                        (Some(value), None) => value,
                                        (None, Some(flag)) => serde_json::json!({"is_error": flag}),
                                        (None, None) => serde_json::Value::Null,
                                    };
                                    parts.push(MessagePart::ToolCallResponse {
                                        id: Some(tool_use_id.clone()),
                                        response: response_val,
                                    });
                                }
                                crate::analyzer::message::AnthropicContentBlock::Thinking {
                                    thinking,
                                    ..
                                } if !thinking.is_empty() => {
                                    parts.push(MessagePart::Reasoning {
                                        content: thinking.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }
                        let msgs = vec![OutputMessage {
                            role: "assistant".to_string(),
                            parts,
                            name: None,
                            finish_reason: resp.stop_reason.clone(),
                        }];
                        let finish = resp.stop_reason.clone();
                        (msgs, finish)
                    })
                    .unwrap_or_else(|| (vec![], None))
            }
            _ => (vec![], None),
        };

        // SysOM response handling
        let (messages, finish_reason) = if messages.is_empty() {
            match message {
                Some(ParsedApiMessage::SysomMessage { response, .. }) => response
                    .as_ref()
                    .map(|resp| {
                        let choice = resp.choices.first();
                        let mut parts = Vec::new();
                        if let Some(choice) = choice {
                            if !choice.message.content.is_empty() {
                                parts.push(MessagePart::Text {
                                    content: choice.message.content.clone(),
                                });
                            }
                            if let Some(ref tool_use) = choice.message.tool_use {
                                for item in tool_use {
                                    let arguments = serde_json::from_str::<serde_json::Value>(
                                        &item.function.arguments,
                                    )
                                    .ok();
                                    parts.push(MessagePart::ToolCall {
                                        id: Some(item.id.clone()),
                                        name: item.function.name.clone(),
                                        arguments,
                                    });
                                }
                            }
                        }
                        let msgs = if parts.is_empty() {
                            vec![]
                        } else {
                            vec![OutputMessage {
                                role: "assistant".to_string(),
                                parts,
                                name: None,
                                finish_reason: Some("stop".to_string()),
                            }]
                        };
                        (msgs, Some("stop".to_string()))
                    })
                    .unwrap_or_else(|| (vec![], None)),
                _ => (messages, finish_reason),
            }
        } else {
            (messages, finish_reason)
        };

        // For SSE responses, extract from response_body when no parsed message
        let messages = if messages.is_empty() && http.is_sse {
            // No parsed response — reconstruct from SSE response body directly
            if let Some(ref body) = http.response_body {
                Self::parse_sse_response_body(body, finish_reason.as_deref()).unwrap_or(messages)
            } else {
                messages
            }
        } else if http.is_sse {
            // Has parsed response but may be missing reasoning/tool_calls — enrich from SSE body
            let mut msgs = messages;
            if let Some(ref body) = http.response_body {
                if let Some(msg) = msgs.first_mut() {
                    if msg.role == "assistant" {
                        let has_reasoning = msg
                            .parts
                            .iter()
                            .any(|p| matches!(p, MessagePart::Reasoning { .. }));
                        // Check if any tool_call is missing id
                        let has_tool_calls_without_id = msg
                            .parts
                            .iter()
                            .any(|p| matches!(p, MessagePart::ToolCall { id, .. } if id.is_none()));
                        let has_tool_calls = msg
                            .parts
                            .iter()
                            .any(|p| matches!(p, MessagePart::ToolCall { .. }));

                        if let Some((extra, sse_finish)) = Self::extract_parts_from_sse_body(body) {
                            if !has_reasoning {
                                if let Some(r) = extra
                                    .iter()
                                    .find(|p| matches!(p, MessagePart::Reasoning { .. }))
                                {
                                    msg.parts.insert(0, r.clone());
                                }
                            }
                            // Always try to enrich tool_calls if missing id or no tool_calls
                            if !has_tool_calls || has_tool_calls_without_id {
                                // Remove existing tool_calls without id, replace with SSE ones
                                if has_tool_calls_without_id {
                                    msg.parts.retain(|p| !matches!(p, MessagePart::ToolCall { id, .. } if id.is_none()));
                                }
                                for p in extra
                                    .into_iter()
                                    .filter(|p| matches!(p, MessagePart::ToolCall { .. }))
                                {
                                    msg.parts.push(p);
                                }
                            }
                            // Enrich finish_reason if missing
                            if msg.finish_reason.is_none() {
                                msg.finish_reason = sse_finish;
                            }
                        }
                    }
                }
            }
            msgs
        } else if messages.is_empty() && Self::is_dashscope_native_path(&http.path) {
            // Non-streaming DashScope/Bailian native protocol: no typed parser
            // claims `/aigc/*-generation/generation`, so the `output` envelope
            // has to be reconstructed from the raw response body.
            http.response_body
                .as_ref()
                .and_then(|body| Self::parse_sse_response_body(body, finish_reason.as_deref()))
                .unwrap_or(messages)
        } else {
            messages
        };

        LLMResponse {
            messages,
            streamed: http.is_sse,
            raw_body: http.response_body.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::message::sysom::{
        SysomFunction, SysomLlmParams, SysomMessage as SysMsg, SysomRequest, SysomResponse,
        SysomResponseChoice, SysomResponseMessage, SysomToolCall, SysomToolUseItem,
    };
    use crate::analyzer::message::types::{
        AnthropicContentBlock, AnthropicMessage as AnthMsg, AnthropicMessageContent,
        AnthropicRequest, AnthropicResponse, AnthropicSystemBlock, AnthropicSystemPrompt,
        AnthropicUsage, MessageRole, OpenAIChatMessage, OpenAIChoice, OpenAIContent, OpenAIRequest,
        OpenAIResponse,
    };
    use crate::analyzer::{AnalysisResult, HttpRecord, ParsedApiMessage, TokenRecord};
    use crate::response_map::ResponseSessionMapper;
    use std::collections::HashMap;

    fn make_http(
        path: &str,
        request_body: Option<String>,
        response_body: Option<String>,
    ) -> HttpRecord {
        HttpRecord {
            timestamp_ns: 1_000_000_000,
            pid: 100,
            comm: "test_agent".to_string(),
            method: "POST".to_string(),
            path: path.to_string(),
            status_code: 200,
            request_headers: "{}".to_string(),
            request_body,
            response_headers: "{}".to_string(),
            response_body,
            duration_ns: 1_000_000,
            first_output_timestamp_ns: None,
            is_sse: false,
            sse_event_count: 0,
        }
    }

    fn build_call(builder: &GenAIBuilder, results: &[AnalysisResult]) -> Option<LLMCall> {
        let mapper = ResponseSessionMapper::new();
        let cache = HashMap::new();
        builder.build_llm_call(results, &mapper, &cache)
    }

    fn empty_chat_msg(role: MessageRole, content: Option<&str>) -> OpenAIChatMessage {
        OpenAIChatMessage {
            role,
            content: content.map(|s| OpenAIContent::Text(s.to_string())),
            reasoning_content: None,
            refusal: None,
            function_call: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            annotations: None,
            audio: None,
        }
    }

    #[test]
    fn test_build_llm_call_returns_none_for_non_llm() {
        let builder = GenAIBuilder::new();
        let http = make_http("/api/health", None, None);
        assert!(build_call(&builder, &[AnalysisResult::Http(http)]).is_none());
    }

    // ── Verification: HTTPS-fallback trigger for unparsable LLM traffic ──
    //
    // An LLM API path whose body cannot be parsed into semantic messages must
    // still build an LLMCall (path matched), but that call carries no messages,
    // so `is_semantically_empty()` is true — this is exactly what drives the FFI
    // divert to AgentsightHttpsData. A parsable exchange must NOT be empty.
    #[test]
    fn test_unparsable_llm_traffic_is_semantically_empty() {
        let builder = GenAIBuilder::new();
        let http = make_http(
            "/v1/chat/completions",
            Some("this is not a valid chat-completions body".to_string()),
            Some("neither is this a parseable response".to_string()),
        );
        let call = build_call(&builder, &[AnalysisResult::Http(http)])
            .expect("LLM path still builds an LLMCall");
        assert!(
            call.is_semantically_empty(),
            "unparsable LLM body must yield a semantically-empty call (→ HTTPS fallback)"
        );
    }

    #[test]
    fn test_parsable_llm_traffic_is_not_semantically_empty() {
        let builder = GenAIBuilder::new();
        let anth_req = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![AnthMsg {
                role: MessageRole::User,
                content: AnthropicMessageContent::Text("Hi".to_string()),
            }],
            max_tokens: 200,
            system: None,
            stream: Some(false),
            temperature: Some(0.5),
            top_p: None,
            top_k: None,
            stop_sequences: None,
            metadata: None,
            tools: None,
            tool_choice: None,
        };
        let parsed = ParsedApiMessage::AnthropicMessage {
            request: Some(anth_req),
            response: None,
        };
        let http = make_http("/v1/messages", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .expect("valid LLM call builds");
        assert!(
            !call.is_semantically_empty(),
            "a parsable LLM exchange must carry messages (no HTTPS fallback)"
        );
    }

    #[test]
    fn test_build_llm_call_returns_none_when_no_http() {
        let builder = GenAIBuilder::new();
        let token = TokenRecord::new(1, "x".to_string(), "openai".to_string(), 1, 2);
        assert!(build_call(&builder, &[AnalysisResult::Token(token)]).is_none());
    }

    /// DashScope native path for reference in the tests below.
    const DASHSCOPE_TEXT_GEN_PATH: &str = "/api/v1/services/aigc/text-generation/generation";

    /// Non-streaming native calls used to be dropped entirely by the
    /// `!is_llm && !is_sse` gate, so nothing reached the Dashboard.
    #[test]
    fn test_build_llm_call_dashscope_native_non_streaming() {
        let builder = GenAIBuilder::new();
        let request_body = serde_json::json!({
            "model": "qwen-plus",
            "input": {"messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Count to 3"}
            ]},
            "parameters": {"result_format": "message"}
        })
        .to_string();
        // Native responses have no top-level `model`, only output/usage/request_id.
        let response_body = serde_json::json!({
            "output": {"choices": [{
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "1, 2, 3."}
            }]},
            "usage": {"input_tokens": 22, "output_tokens": 8, "total_tokens": 30},
            "request_id": "req-native-1"
        })
        .to_string();

        let http = make_http(
            DASHSCOPE_TEXT_GEN_PATH,
            Some(request_body),
            Some(response_body),
        );
        let call = build_call(&builder, &[AnalysisResult::Http(http)])
            .expect("native non-streaming call must build an LLMCall");

        assert_eq!(call.provider, "dashscope");
        assert_eq!(call.model, "qwen-plus", "model comes from the request body");

        // Request side: `input` is an object, so extract_messages_view must
        // reach into `input.messages`.
        assert_eq!(call.request.messages.len(), 2);
        assert_eq!(call.request.messages[0].role, "system");
        assert_eq!(call.request.messages[1].role, "user");

        // Response side: non-streaming native has no typed parser, so the
        // envelope must be reconstructed from the raw body.
        assert_eq!(call.response.messages.len(), 1);
        assert!(matches!(
            &call.response.messages[0].parts[0],
            MessagePart::Text { content } if content == "1, 2, 3."
        ));
        assert_eq!(
            call.response.messages[0].finish_reason.as_deref(),
            Some("stop")
        );
        assert!(!call.is_semantically_empty());
    }

    /// The token record provider must not leak `anthropic` into the call, and a
    /// model-less token record must not shadow the request-body model.
    #[test]
    fn test_build_llm_call_dashscope_native_provider_beats_token_record() {
        let builder = GenAIBuilder::new();
        let request_body =
            serde_json::json!({"model": "qwen3-vl-plus", "input": {"messages": []}}).to_string();
        let http = make_http(
            "/api/v1/services/aigc/multimodal-generation/generation",
            Some(request_body),
            None,
        );
        // Simulates the pre-fix mislabelling reaching build_llm_call.
        let token = TokenRecord::new(100, "python3".to_string(), "anthropic".to_string(), 30, 12)
            .with_model(String::new());
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Token(token)],
        )
        .expect("native call builds");
        assert_eq!(call.provider, "dashscope");
        assert_eq!(call.model, "qwen3-vl-plus");
    }

    #[test]
    fn test_build_llm_call_chat_completions_with_host() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#.to_string();
        let mut http = make_http("/v1/chat/completions", Some(body), None);
        http.request_headers = r#"{"host":"api.openai.com:443"}"#.to_string();
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.provider, "openai");
        assert_eq!(call.model, "gpt-4");
        assert_eq!(
            call.metadata.get("server.address").unwrap(),
            "api.openai.com"
        );
        assert_eq!(call.metadata.get("server.port").unwrap(), "443");
        assert_eq!(call.metadata.get("http.domain").unwrap(), "api.openai.com");
        assert_eq!(call.metadata.get("operation_name").unwrap(), "chat");
        assert!(call.metadata.contains_key("user_query"));
    }

    #[test]
    fn test_build_llm_call_host_without_port() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#.to_string();
        let mut http = make_http("/v1/chat/completions", Some(body), None);
        http.request_headers = r#"{"Host":"example.com"}"#.to_string();
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.metadata.get("server.address").unwrap(), "example.com");
        assert!(!call.metadata.contains_key("server.port"));
        assert_eq!(call.metadata.get("http.domain").unwrap(), "example.com");
    }

    #[test]
    fn test_build_llm_call_completions_path() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}]}"#.to_string();
        let http = make_http("/v1/completions", Some(body), None);
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(
            call.metadata.get("operation_name").unwrap(),
            "text_completion"
        );
    }

    #[test]
    fn test_build_llm_call_copilot_path_operation_name() {
        let builder = GenAIBuilder::new();
        let body = r#"{"llmParamString":"{\"model\":\"qwen\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"}"#.to_string();
        let http = make_http(
            "/api/v1/copilot/generate_copilot_response",
            Some(body),
            None,
        );
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.metadata.get("operation_name").unwrap(), "chat");
        assert_eq!(call.provider, "sysom");
    }

    #[test]
    fn test_build_llm_call_sysom_body_match_only() {
        let builder = GenAIBuilder::new();
        let body = r#"{"llmParamString":"{\"model\":\"qwen\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"}"#.to_string();
        let http = make_http("/some/proxy/path", Some(body), None);
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.provider, "sysom");
    }

    #[test]
    fn test_build_llm_call_error_object_message() {
        let builder = GenAIBuilder::new();
        let mut http = make_http(
            "/v1/chat/completions",
            None,
            Some(r#"{"error":{"message":"rate limit"}}"#.to_string()),
        );
        http.status_code = 429;
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.error.as_deref(), Some("rate limit"));
    }

    #[test]
    fn test_build_llm_call_error_chunked_encoding() {
        let builder = GenAIBuilder::new();
        let body = "b6\r\n{\"error\":{\"message\":\"oops\"}}\r\n0\r\n\r\n";
        let mut http = make_http("/v1/messages", None, Some(body.to_string()));
        http.status_code = 500;
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.error.as_deref(), Some("oops"));
    }

    #[test]
    fn test_build_llm_call_error_escaped_json_string() {
        let builder = GenAIBuilder::new();
        let body = r#"{"error":"{\"error\":{\"message\":\"inner_msg\"}}"}"#;
        let mut http = make_http("/v1/chat/completions", None, Some(body.to_string()));
        http.status_code = 400;
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.error.as_deref(), Some("inner_msg"));
    }

    #[test]
    fn test_build_llm_call_error_top_level_message() {
        let builder = GenAIBuilder::new();
        let body = r#"{"message":"top msg"}"#;
        let mut http = make_http("/v1/chat/completions", None, Some(body.to_string()));
        http.status_code = 503;
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.error.as_deref(), Some("top msg"));
    }

    #[test]
    fn test_build_llm_call_error_plain_text_fallback() {
        let builder = GenAIBuilder::new();
        let body = "Internal Server Error";
        let mut http = make_http("/v1/messages", None, Some(body.to_string()));
        http.status_code = 502;
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(call.error.as_deref(), Some("Internal Server Error"));
    }

    #[test]
    fn test_build_llm_call_error_status_below_400_no_error() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"x","messages":[{"role":"user","content":"hi"}]}"#.to_string();
        let http = make_http("/v1/chat/completions", Some(body), None);
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert!(call.error.is_none());
    }

    #[test]
    fn test_build_llm_call_with_token_record() {
        let builder = GenAIBuilder::new();
        let http = make_http("/v1/chat/completions", None, None);
        let token = TokenRecord {
            id: 0,
            timestamp_ns: 0,
            pid: 100,
            comm: "x".to_string(),
            agent: None,
            model: Some("token-model".to_string()),
            provider: "token-provider".to_string(),
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_tokens: Some(5),
            cache_read_tokens: Some(3),
            request_id: None,
            endpoint: None,
            tool_calls: vec![],
            reasoning_content: None,
        };
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Token(token)],
        )
        .unwrap();
        assert_eq!(call.model, "token-model");
        let tu = call.token_usage.unwrap();
        assert_eq!(tu.input_tokens, 10);
        assert_eq!(tu.output_tokens, 20);
        // "token-provider" is not Anthropic, so the cache counters are assumed
        // to sit inside the reported input already.
        assert_eq!(tu.total_tokens, 30);
        assert_eq!(tu.cache_creation_input_tokens, Some(5));
        assert_eq!(tu.cache_read_input_tokens, Some(3));
    }

    #[test]
    fn test_build_request_openai_with_data() {
        let builder = GenAIBuilder::new();
        let openai_req = OpenAIRequest {
            model: "gpt-4".to_string(),
            messages: vec![empty_chat_msg(MessageRole::User, Some("hi"))],
            temperature: Some(0.7),
            max_tokens: Some(100),
            stream: Some(true),
            top_p: Some(0.9),
            n: None,
            stop: Some(vec!["END".to_string()]),
            presence_penalty: Some(0.1),
            frequency_penalty: Some(0.2),
            user: None,
            tools: Some(vec![serde_json::json!({"name": "t"})]),
            tool_choice: None,
            response_format: None,
            seed: Some(42),
            logprobs: None,
            top_logprobs: None,
            parallel_tool_calls: None,
        };
        let parsed = ParsedApiMessage::OpenAICompletion {
            request: Some(openai_req),
            response: None,
        };
        let http = make_http("/v1/chat/completions", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert_eq!(call.request.temperature, Some(0.7));
        assert_eq!(call.request.max_tokens, Some(100));
        assert!(call.request.stream);
        assert_eq!(call.request.seed, Some(42));
        assert_eq!(call.request.frequency_penalty, Some(0.2));
        assert_eq!(call.request.presence_penalty, Some(0.1));
        assert!(call.request.tools.is_some());
        assert_eq!(
            call.request.stop_sequences.as_deref().unwrap(),
            &["END".to_string()]
        );
    }

    #[test]
    fn test_build_request_anthropic_with_data() {
        let builder = GenAIBuilder::new();
        let anth_req = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![AnthMsg {
                role: MessageRole::User,
                content: AnthropicMessageContent::Text("Hi".to_string()),
            }],
            max_tokens: 200,
            system: None,
            stream: Some(false),
            temperature: Some(0.5),
            top_p: Some(0.8),
            top_k: Some(50),
            stop_sequences: Some(vec!["STOP".to_string()]),
            metadata: None,
            tools: Some(vec![serde_json::json!({"name": "t"})]),
            tool_choice: None,
        };
        let parsed = ParsedApiMessage::AnthropicMessage {
            request: Some(anth_req),
            response: None,
        };
        let http = make_http("/v1/messages", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert_eq!(call.provider, "anthropic");
        assert_eq!(call.request.temperature, Some(0.5));
        assert_eq!(call.request.max_tokens, Some(200));
        assert_eq!(call.request.top_k, Some(50.0));
        assert_eq!(
            call.request.stop_sequences.as_deref().unwrap(),
            &["STOP".to_string()]
        );
        assert!(!call.request.stream);
        assert!(call.request.tools.is_some());
    }

    #[test]
    fn test_build_request_anthropic_with_system_text() {
        let builder = GenAIBuilder::new();
        let anth_req = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![AnthMsg {
                role: MessageRole::User,
                content: AnthropicMessageContent::Text("Hi".to_string()),
            }],
            max_tokens: 200,
            system: Some(AnthropicSystemPrompt::Text("You are helpful".to_string())),
            stream: Some(false),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            metadata: None,
            tools: None,
            tool_choice: None,
        };
        let parsed = ParsedApiMessage::AnthropicMessage {
            request: Some(anth_req),
            response: None,
        };
        let http = make_http("/v1/messages", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert_eq!(call.request.messages.len(), 2);
        assert_eq!(call.request.messages[0].role, "system");
        assert_eq!(call.request.messages[1].role, "user");
    }

    #[test]
    fn test_build_request_anthropic_with_system_blocks() {
        let builder = GenAIBuilder::new();
        let anth_req = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![],
            max_tokens: 200,
            system: Some(AnthropicSystemPrompt::Blocks(vec![
                AnthropicSystemBlock {
                    type_: "text".to_string(),
                    text: Some("Part 1".to_string()),
                    cache_control: None,
                },
                AnthropicSystemBlock {
                    type_: "text".to_string(),
                    text: Some("Part 2".to_string()),
                    cache_control: None,
                },
            ])),
            stream: Some(false),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            metadata: None,
            tools: None,
            tool_choice: None,
        };
        let parsed = ParsedApiMessage::AnthropicMessage {
            request: Some(anth_req),
            response: None,
        };
        let http = make_http("/v1/messages", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert_eq!(call.request.messages.len(), 1);
        assert_eq!(call.request.messages[0].role, "system");
    }

    #[test]
    fn test_build_request_anthropic_preserves_tool_result_blocks() {
        let builder = GenAIBuilder::new();
        let anth_req = AnthropicRequest {
            model: "claude-3".to_string(),
            messages: vec![
                AnthMsg {
                    role: MessageRole::Assistant,
                    content: AnthropicMessageContent::Blocks(vec![
                        AnthropicContentBlock::ToolUse {
                            id: "toolu_1".to_string(),
                            name: "Bash".to_string(),
                            input: serde_json::json!({"command": "cat /tmp/missing.txt"}),
                        },
                    ]),
                },
                AnthMsg {
                    role: MessageRole::User,
                    content: AnthropicMessageContent::Blocks(vec![
                        AnthropicContentBlock::ToolResult {
                            tool_use_id: "toolu_1".to_string(),
                            content: Some(serde_json::json!(
                                "Exit code 1\ncat: /tmp/missing.txt: No such file or directory"
                            )),
                            is_error: Some(true),
                        },
                    ]),
                },
            ],
            max_tokens: 200,
            system: None,
            stream: Some(false),
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: None,
            metadata: None,
            tools: None,
            tool_choice: None,
        };
        let parsed = ParsedApiMessage::AnthropicMessage {
            request: Some(anth_req),
            response: None,
        };
        let http = make_http("/v1/messages", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();

        assert!(call.request.messages.iter().any(|message| {
            message.parts.iter().any(|part| {
                matches!(
                    part,
                    MessagePart::ToolCallResponse { id, response }
                        if id.as_deref() == Some("toolu_1")
                            && response.get("is_error").and_then(|v| v.as_bool()) == Some(true)
                )
            })
        }));
    }

    #[test]
    fn test_build_request_sysom_full() {
        let builder = GenAIBuilder::new();
        let sysom_req = SysomRequest {
            params: SysomLlmParams {
                model: "qwen".to_string(),
                messages: vec![
                    SysMsg {
                        role: "system".to_string(),
                        content: "instr".to_string(),
                        tool_call_id: None,
                        name: None,
                        tool_calls: None,
                    },
                    SysMsg {
                        role: "user".to_string(),
                        content: "hello".to_string(),
                        tool_call_id: None,
                        name: None,
                        tool_calls: None,
                    },
                    SysMsg {
                        role: "assistant".to_string(),
                        content: String::new(),
                        tool_call_id: None,
                        name: None,
                        tool_calls: Some(vec![SysomToolCall {
                            id: "call_1".to_string(),
                            call_type: "function".to_string(),
                            function: SysomFunction {
                                name: "search".to_string(),
                                arguments: r#"{"q":"rust"}"#.to_string(),
                            },
                        }]),
                    },
                    SysMsg {
                        role: "tool".to_string(),
                        content: r#"{"result":"ok"}"#.to_string(),
                        tool_call_id: Some("call_1".to_string()),
                        name: Some("search".to_string()),
                        tool_calls: None,
                    },
                    SysMsg {
                        role: "tool".to_string(),
                        content: "plaintext result".to_string(),
                        tool_call_id: Some("call_2".to_string()),
                        name: None,
                        tool_calls: None,
                    },
                ],
                stream: true,
                temperature: Some(0.3),
                max_tokens: Some(500),
                top_p: Some(0.9),
                tools: Some(vec![serde_json::json!({"name": "t"})]),
                use_dashscope: None,
            },
        };
        let parsed = ParsedApiMessage::SysomMessage {
            request: Some(sysom_req),
            response: None,
        };
        let http = make_http("/api/v1/copilot/generate_copilot", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert_eq!(call.provider, "sysom");
        assert_eq!(call.model, "qwen");
        assert_eq!(call.request.messages.len(), 5);
        assert_eq!(call.request.temperature, Some(0.3));
        assert_eq!(call.request.max_tokens, Some(500));
        assert!(call.request.stream);
        let assistant = call
            .request
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap();
        assert!(
            assistant
                .parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCall { name, .. } if name == "search"))
        );
        let tool_msgs: Vec<_> = call
            .request
            .messages
            .iter()
            .filter(|m| m.role == "tool")
            .collect();
        assert_eq!(tool_msgs.len(), 2);
        assert!(
            tool_msgs[0]
                .parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCallResponse { .. }))
        );
    }

    #[test]
    fn test_build_response_anthropic_all_blocks() {
        let builder = GenAIBuilder::new();
        let anth_resp = AnthropicResponse {
            id: "msg_123".to_string(),
            type_: "message".to_string(),
            role: MessageRole::Assistant,
            content: vec![
                AnthropicContentBlock::Thinking {
                    thinking: "let me think".to_string(),
                    signature: None,
                },
                AnthropicContentBlock::Text {
                    text: "answer".to_string(),
                    cache_control: None,
                },
                AnthropicContentBlock::ToolUse {
                    id: "tu_1".to_string(),
                    name: "calc".to_string(),
                    input: serde_json::json!({"a": 1}),
                },
                AnthropicContentBlock::ToolResult {
                    tool_use_id: "tu_0".to_string(),
                    content: Some(serde_json::json!("ok")),
                    is_error: None,
                },
                AnthropicContentBlock::Text {
                    text: String::new(),
                    cache_control: None,
                },
                AnthropicContentBlock::Thinking {
                    thinking: String::new(),
                    signature: None,
                },
            ],
            model: "claude-3".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };
        let parsed = ParsedApiMessage::AnthropicMessage {
            request: None,
            response: Some(anth_resp),
        };
        let http = make_http("/v1/messages", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert_eq!(call.response.messages.len(), 1);
        let msg = &call.response.messages[0];
        assert_eq!(msg.role, "assistant");
        assert!(
            msg.parts.iter().any(
                |p| matches!(p, MessagePart::Reasoning { content } if content == "let me think")
            )
        );
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { content } if content == "answer"))
        );
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCall { name, .. } if name == "calc"))
        );
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCallResponse { .. }))
        );
        assert_eq!(msg.finish_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn test_build_response_openai_with_choice() {
        let builder = GenAIBuilder::new();
        let openai_resp = OpenAIResponse {
            id: "chatcmpl-1".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-4".to_string(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: empty_chat_msg(MessageRole::Assistant, Some("answer")),
                finish_reason: Some("stop".to_string()),
                logprobs: None,
            }],
            usage: None,
            system_fingerprint: None,
        };
        let parsed = ParsedApiMessage::OpenAICompletion {
            request: None,
            response: Some(openai_resp),
        };
        let http = make_http("/v1/chat/completions", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert_eq!(call.response.messages.len(), 1);
        let msg = &call.response.messages[0];
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { content } if content == "answer"))
        );
        assert_eq!(msg.finish_reason.as_deref(), Some("stop"));
        assert_eq!(call.metadata.get("response_id").unwrap(), "chatcmpl-1");
    }

    #[test]
    fn test_build_response_sysom_with_content_and_tool_use() {
        let builder = GenAIBuilder::new();
        let sysom_resp = SysomResponse {
            id: Some("chatcmpl-xx".to_string()),
            choices: vec![SysomResponseChoice {
                message: SysomResponseMessage {
                    content: "answer".to_string(),
                    tool_use: Some(vec![SysomToolUseItem {
                        index: 0,
                        id: "tu_1".to_string(),
                        item_type: "function".to_string(),
                        function: SysomFunction {
                            name: "calc".to_string(),
                            arguments: r#"{"x":1}"#.to_string(),
                        },
                    }]),
                },
            }],
        };
        let parsed = ParsedApiMessage::SysomMessage {
            request: None,
            response: Some(sysom_resp),
        };
        let http = make_http("/api/v1/copilot/generate_copilot", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        let msg = &call.response.messages[0];
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { content } if content == "answer"))
        );
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCall { name, .. } if name == "calc"))
        );
        assert_eq!(msg.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn test_build_response_sysom_empty_message() {
        let builder = GenAIBuilder::new();
        let sysom_resp = SysomResponse {
            id: None,
            choices: vec![SysomResponseChoice {
                message: SysomResponseMessage {
                    content: String::new(),
                    tool_use: None,
                },
            }],
        };
        let parsed = ParsedApiMessage::SysomMessage {
            request: None,
            response: Some(sysom_resp),
        };
        let http = make_http("/api/v1/copilot/generate_copilot", None, None);
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        assert!(call.response.messages.is_empty());
    }

    #[test]
    fn test_build_response_sse_no_parsed_message() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"q"}]}"#;
        let sse_body = r#"[{"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}]"#;
        let mut http = make_http(
            "/v1/chat/completions",
            Some(body.to_string()),
            Some(sse_body.to_string()),
        );
        http.is_sse = true;
        http.sse_event_count = 1;
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert!(call.response.streamed);
        assert!(!call.response.messages.is_empty());
        let msg = &call.response.messages[0];
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { content } if content == "hi"))
        );
    }

    #[test]
    fn test_build_response_sse_enrichment_with_reasoning() {
        let builder = GenAIBuilder::new();
        let openai_resp = OpenAIResponse {
            id: "chatcmpl-2".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-4".to_string(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: empty_chat_msg(MessageRole::Assistant, Some("answer")),
                finish_reason: None,
                logprobs: None,
            }],
            usage: None,
            system_fingerprint: None,
        };
        let parsed = ParsedApiMessage::OpenAICompletion {
            request: None,
            response: Some(openai_resp),
        };
        let sse_body = r#"[{"choices":[{"delta":{"reasoning_content":"thinking"}}]},{"choices":[{"delta":{"content":"answer"},"finish_reason":"stop"}]}]"#;
        let mut http = make_http("/v1/chat/completions", None, Some(sse_body.to_string()));
        http.is_sse = true;
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        let msg = &call.response.messages[0];
        assert!(
            msg.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Reasoning { content } if content == "thinking"))
        );
        assert_eq!(msg.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn test_build_response_sse_enrichment_with_tool_calls() {
        let builder = GenAIBuilder::new();
        // Parsed response has a tool_call without id; SSE body has tool_calls with id
        let mut assistant_msg = empty_chat_msg(MessageRole::Assistant, Some(""));
        assistant_msg.tool_calls = Some(vec![serde_json::json!({
            "function": {"name": "x", "arguments": "{}"}
        })]);
        let openai_resp = OpenAIResponse {
            id: "chatcmpl-3".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-4".to_string(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: assistant_msg,
                finish_reason: None,
                logprobs: None,
            }],
            usage: None,
            system_fingerprint: None,
        };
        let parsed = ParsedApiMessage::OpenAICompletion {
            request: None,
            response: Some(openai_resp),
        };
        let sse_body = r#"[{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc_1","function":{"name":"search","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}]"#;
        let mut http = make_http("/v1/chat/completions", None, Some(sse_body.to_string()));
        http.is_sse = true;
        let call = build_call(
            &builder,
            &[AnalysisResult::Http(http), AnalysisResult::Message(parsed)],
        )
        .unwrap();
        let msg = &call.response.messages[0];
        assert!(msg.parts.iter().any(
            |p| matches!(p, MessagePart::ToolCall { id, .. } if id.as_deref() == Some("tc_1"))
        ));
    }

    #[test]
    fn test_build_request_fallback_empty_when_no_body() {
        let builder = GenAIBuilder::new();
        let mut http = make_http("/v1/chat/completions", None, None);
        http.is_sse = true;
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert!(call.request.messages.is_empty());
        assert!(!call.request.stream);
    }

    #[test]
    fn first_output_timestamp_reaches_latency_query() {
        let mut http = make_http(
            "/v1/chat/completions",
            Some(
                r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#
                    .to_string(),
            ),
            Some(
                r#"{"id":"chatcmpl-test","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"world"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#
                    .to_string(),
            ),
        );
        http.duration_ns = 500_000_000;
        http.first_output_timestamp_ns = Some(1_100_000_000);
        http.is_sse = true;
        http.sse_event_count = 4;

        let builder = GenAIBuilder::new();
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(
            call.metadata.get("first_output_timestamp_ns"),
            Some(&"1100000000".to_string())
        );
        assert_eq!(call.metadata.get("is_sse"), Some(&"true".to_string()));

        let path = std::env::temp_dir().join(format!(
            "agentsight_timing_pipeline_{}_{}.db",
            std::process::id(),
            1_100_000_000u64
        ));
        let _ = std::fs::remove_file(&path);
        let store = crate::storage::sqlite::genai::GenAISqliteStore::new_with_path(&path).unwrap();
        let event = crate::genai::semantic::GenAISemanticEvent::LLMCall(call);
        store.complete_pending(&event).unwrap();

        let metrics = store.get_latency_metrics(0, 2_000_000_000, None).unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].streaming_call_count, 1);
        assert_eq!(
            metrics[0].ttft_ms.as_ref().map(|metric| metric.p50),
            Some(100.0)
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_build_llm_call_call_kind_main() {
        let builder = GenAIBuilder::new();
        let body = serde_json::json!({
            "model": "qwen3.6-plus",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "hello"}
            ]
        })
        .to_string();
        let http = make_http("/v1/chat/completions", Some(body), None);
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(
            call.metadata.get("call_kind").map(|s| s.as_str()),
            Some("main")
        );
    }

    #[test]
    fn test_build_llm_call_call_kind_recap_memory_subagent() {
        let builder = GenAIBuilder::new();
        let body = serde_json::json!({
            "model": "qwen3.6-plus",
            "messages": [
                {"role": "system", "content": "You are now acting as the managed memory extraction subagent."},
                {"role": "user", "content": "Managed memory has TWO directories. Choose which one to write each memory into."}
            ]
        })
        .to_string();
        let http = make_http("/v1/chat/completions", Some(body), None);
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(
            call.metadata.get("call_kind").map(|s| s.as_str()),
            Some("recap")
        );
    }

    #[test]
    fn test_build_llm_call_call_kind_recap_suggestion_subagent() {
        let builder = GenAIBuilder::new();
        let body = serde_json::json!({
            "model": "qwen3.6-plus",
            "messages": [
                {"role": "user", "content": "[SUGGESTION MODE: Suggest what the user might naturally type next.]"}
            ]
        })
        .to_string();
        let http = make_http("/v1/chat/completions", Some(body), None);
        let call = build_call(&builder, &[AnalysisResult::Http(http)]).unwrap();
        assert_eq!(
            call.metadata.get("call_kind").map(|s| s.as_str()),
            Some("recap")
        );
    }

    // ── Turn eviction vs. tool calls (issue #2262) ──────────────────────
    //
    // The conversation anchor must survive a response that still requests
    // tool execution, even when the provider mislabels it with a terminal
    // finish_reason such as "stop" (e.g. DashScope with forced tool_choice).

    /// Builds an OpenAI-style request/response pair: a single user message
    /// plus an assistant reply with the given finish_reason and tool_calls.
    fn openai_turn_results(
        user_text: &str,
        response_id: &str,
        finish_reason: &str,
        tool_calls: Option<Vec<serde_json::Value>>,
    ) -> Vec<AnalysisResult> {
        let request = OpenAIRequest {
            model: "gpt-4".to_string(),
            messages: vec![empty_chat_msg(MessageRole::User, Some(user_text))],
            temperature: None,
            max_tokens: None,
            stream: None,
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
        };
        let mut assistant_msg = empty_chat_msg(MessageRole::Assistant, Some("ok"));
        assistant_msg.tool_calls = tool_calls;
        let response = OpenAIResponse {
            id: response_id.to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "gpt-4".to_string(),
            choices: vec![OpenAIChoice {
                index: 0,
                message: assistant_msg,
                finish_reason: Some(finish_reason.to_string()),
                logprobs: None,
            }],
            usage: None,
            system_fingerprint: None,
        };
        let parsed = ParsedApiMessage::OpenAICompletion {
            request: Some(request),
            response: Some(response),
        };
        let http = make_http("/v1/chat/completions", None, None);
        vec![AnalysisResult::Http(http), AnalysisResult::Message(parsed)]
    }

    fn conv_id(builder: &GenAIBuilder, results: &[AnalysisResult]) -> String {
        build_call(builder, results)
            .unwrap()
            .metadata
            .get("conversation_id")
            .unwrap()
            .clone()
    }

    fn sample_tool_calls() -> Vec<serde_json::Value> {
        vec![serde_json::json!({
            "id": "tc_1",
            "type": "function",
            "function": {"name": "run_cmd", "arguments": "{}"}
        })]
    }

    #[test]
    fn test_tool_call_with_stop_finish_reason_keeps_turn_open() {
        let builder = GenAIBuilder::new();
        let user = "same user text";
        let first = conv_id(
            &builder,
            &openai_turn_results(user, "resp-1", "stop", Some(sample_tool_calls())),
        );
        let second = conv_id(
            &builder,
            &openai_turn_results(user, "resp-2", "stop", Some(sample_tool_calls())),
        );
        assert_eq!(
            first, second,
            "a response carrying tool calls must not evict the conversation anchor"
        );
    }

    #[test]
    fn test_pure_text_stop_finish_reason_ends_turn() {
        let builder = GenAIBuilder::new();
        let user = "same user text";
        let first = conv_id(&builder, &openai_turn_results(user, "resp-1", "stop", None));
        let second = conv_id(&builder, &openai_turn_results(user, "resp-2", "stop", None));
        assert_ne!(
            first, second,
            "a pure-text terminal response must still evict the conversation anchor"
        );
    }

    #[test]
    fn test_empty_tool_calls_with_stop_ends_turn() {
        let builder = GenAIBuilder::new();
        let user = "same user text";
        let first = conv_id(
            &builder,
            &openai_turn_results(user, "resp-1", "stop", Some(vec![])),
        );
        let second = conv_id(
            &builder,
            &openai_turn_results(user, "resp-2", "stop", Some(vec![])),
        );
        assert_ne!(
            first, second,
            "an empty tool_calls array yields no ToolCall part and must still end the turn"
        );
    }

    #[test]
    fn test_pause_turn_finish_reason_keeps_turn_open() {
        let builder = GenAIBuilder::new();
        let user = "same user text";
        let first = conv_id(
            &builder,
            &openai_turn_results(user, "resp-1", "pause_turn", None),
        );
        let second = conv_id(
            &builder,
            &openai_turn_results(user, "resp-2", "pause_turn", None),
        );
        assert_eq!(
            first, second,
            "pause_turn means the turn will resume, so the anchor must survive"
        );
    }

    /// Builds an OpenAI SSE exchange: request body with a single user message
    /// and a streamed response whose tool_call arrives as fragmented deltas,
    /// with the last chunk reporting finish_reason "stop".
    fn sse_stop_tool_call_results(user_text: &str, response_id: &str) -> Vec<AnalysisResult> {
        let request_body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": user_text}]
        })
        .to_string();
        let sse_body = format!(
            concat!(
                r#"[{{"id":"{rid}","choices":[{{"delta":{{"tool_calls":"#,
                r#"[{{"index":0,"id":"tc_1","function":{{"name":"run_cmd","arguments":"{{\"c"}}}}]}},"#,
                r#""finish_reason":null}}]}},"#,
                r#"{{"id":"{rid}","choices":[{{"delta":{{"tool_calls":"#,
                r#"[{{"index":0,"function":{{"arguments":"md\":1}}"}}}}]}},"finish_reason":"stop"}}]}}]"#
            ),
            rid = response_id
        );
        let mut http = make_http("/v1/chat/completions", Some(request_body), Some(sse_body));
        http.is_sse = true;
        vec![AnalysisResult::Http(http)]
    }

    #[test]
    fn test_sse_tool_call_with_stop_keeps_turn_open() {
        let builder = GenAIBuilder::new();
        let user = "same user text";
        let first = conv_id(&builder, &sse_stop_tool_call_results(user, "sse-resp-1"));
        let second = conv_id(&builder, &sse_stop_tool_call_results(user, "sse-resp-2"));
        assert_eq!(
            first, second,
            "an SSE-aggregated tool call with finish_reason stop must not evict the anchor"
        );
    }

    /// Builds a SysOM exchange with a single user message and a tool_use
    /// response; the SysOM path hardcodes finish_reason "stop".
    fn sysom_tool_use_results(user_text: &str) -> Vec<AnalysisResult> {
        let request = SysomRequest {
            params: SysomLlmParams {
                model: "qwen".to_string(),
                messages: vec![SysMsg {
                    role: "user".to_string(),
                    content: user_text.to_string(),
                    tool_call_id: None,
                    name: None,
                    tool_calls: None,
                }],
                stream: false,
                temperature: None,
                max_tokens: None,
                top_p: None,
                tools: None,
                use_dashscope: None,
            },
        };
        let response = SysomResponse {
            id: Some("chatcmpl-xx".to_string()),
            choices: vec![SysomResponseChoice {
                message: SysomResponseMessage {
                    content: "answer".to_string(),
                    tool_use: Some(vec![SysomToolUseItem {
                        index: 0,
                        id: "tu_1".to_string(),
                        item_type: "function".to_string(),
                        function: SysomFunction {
                            name: "calc".to_string(),
                            arguments: r#"{"x":1}"#.to_string(),
                        },
                    }]),
                },
            }],
        };
        let parsed = ParsedApiMessage::SysomMessage {
            request: Some(request),
            response: Some(response),
        };
        let http = make_http("/api/v1/copilot/generate_copilot", None, None);
        vec![AnalysisResult::Http(http), AnalysisResult::Message(parsed)]
    }

    #[test]
    fn test_sysom_tool_use_with_stop_keeps_turn_open() {
        let builder = GenAIBuilder::new();
        let user = "same user text";
        let first = conv_id(&builder, &sysom_tool_use_results(user));
        let second = conv_id(&builder, &sysom_tool_use_results(user));
        assert_eq!(
            first, second,
            "a SysOM tool_use response (hardcoded finish_reason stop) must not evict the anchor"
        );
    }

    // --- recover_empty_tool_call_names tests ---

    fn make_request_with_tools(tools: Vec<serde_json::Value>) -> LLMRequest {
        LLMRequest {
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
            tools: Some(tools),
            raw_body: None,
        }
    }

    fn make_response_with_tool_calls(parts: Vec<MessagePart>) -> LLMResponse {
        LLMResponse {
            messages: vec![OutputMessage {
                role: "assistant".to_string(),
                parts,
                name: None,
                finish_reason: None,
            }],
            streamed: true,
            raw_body: None,
        }
    }

    fn tool_def(name: &str, props: &[&str]) -> serde_json::Value {
        let properties: serde_json::Map<String, serde_json::Value> = props
            .iter()
            .map(|p| (p.to_string(), serde_json::json!({"type": "string"})))
            .collect();
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "parameters": { "type": "object", "properties": properties }
            }
        })
    }

    /// Single tool defined: empty name gets back-filled unconditionally.
    /// Discriminating: removing the single-tool fast path makes this fail.
    #[test]
    fn test_recover_name_single_tool() {
        let request = make_request_with_tools(vec![tool_def("run_shell_command", &["command"])]);
        let mut response = make_response_with_tool_calls(vec![MessagePart::ToolCall {
            id: Some("call_1".to_string()),
            name: String::new(),
            arguments: Some(serde_json::json!({"command": "whoami"})),
        }]);

        GenAIBuilder::recover_empty_tool_call_names(&request, &mut response);

        match &response.messages[0].parts[0] {
            MessagePart::ToolCall { name, .. } => assert_eq!(name, "run_shell_command"),
            _ => panic!("expected ToolCall"),
        }
    }

    /// Non-empty name is never overwritten by recovery.
    #[test]
    fn test_recover_name_skips_nonempty() {
        let request = make_request_with_tools(vec![tool_def("run_shell_command", &["command"])]);
        let mut response = make_response_with_tool_calls(vec![MessagePart::ToolCall {
            id: Some("call_1".to_string()),
            name: "original_name".to_string(),
            arguments: Some(serde_json::json!({"command": "whoami"})),
        }]);

        GenAIBuilder::recover_empty_tool_call_names(&request, &mut response);

        match &response.messages[0].parts[0] {
            MessagePart::ToolCall { name, .. } => assert_eq!(name, "original_name"),
            _ => panic!("expected ToolCall"),
        }
    }

    /// Multiple tools: match by argument keys.
    #[test]
    fn test_recover_name_multi_tool_by_arg_keys() {
        let request = make_request_with_tools(vec![
            tool_def("run_shell_command", &["command", "is_background"]),
            tool_def("read_file", &["file_path", "offset", "limit"]),
        ]);
        let mut response = make_response_with_tool_calls(vec![
            MessagePart::ToolCall {
                id: Some("call_1".to_string()),
                name: String::new(),
                arguments: Some(serde_json::json!({"command": "whoami", "is_background": false})),
            },
            MessagePart::ToolCall {
                id: Some("call_2".to_string()),
                name: String::new(),
                arguments: Some(serde_json::json!({"file_path": "/etc/hostname"})),
            },
        ]);

        GenAIBuilder::recover_empty_tool_call_names(&request, &mut response);

        match &response.messages[0].parts[0] {
            MessagePart::ToolCall { name, .. } => assert_eq!(name, "run_shell_command"),
            _ => panic!("expected ToolCall"),
        }
        match &response.messages[0].parts[1] {
            MessagePart::ToolCall { name, .. } => assert_eq!(name, "read_file"),
            _ => panic!("expected ToolCall"),
        }
    }

    /// No tools in request: nothing to recover from, name stays empty.
    #[test]
    fn test_recover_name_no_tools_in_request() {
        let request = LLMRequest {
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
        };
        let mut response = make_response_with_tool_calls(vec![MessagePart::ToolCall {
            id: Some("call_1".to_string()),
            name: String::new(),
            arguments: Some(serde_json::json!({"command": "whoami"})),
        }]);

        GenAIBuilder::recover_empty_tool_call_names(&request, &mut response);

        match &response.messages[0].parts[0] {
            MessagePart::ToolCall { name, .. } => assert_eq!(name, ""),
            _ => panic!("expected ToolCall"),
        }
    }
}
