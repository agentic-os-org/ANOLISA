//! GenAI Semantic Builder
//!
//! This module builds GenAI semantic events from AnalysisResult.
//! It reuses already-extracted data to avoid redundant parsing.

use super::helpers::PidAgentNameCache;
use super::id_resolver::IdResolver;
use super::semantic::{GenAISemanticEvent, OutputMessage};
use crate::aggregator::{ConnectionId, ParsedRequest};
use crate::analyzer::AnalysisResult;
use crate::analyzer::token::{TokenParser, merge_usage};
use crate::parser::sse::ParsedSseEvent;
use crate::response_map::ResponseSessionMapper;
use crate::runtime_metrics::StageTimer;
use crate::storage::sqlite::{PendingCallInfo, PendingOrigin, SseEnrichment};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Output from `GenAIBuilder::build()`, containing built events and deferred resolution info.
pub struct BuildOutput {
    /// Built GenAI semantic events (ready to export, may have fallback session_id)
    pub events: Vec<GenAISemanticEvent>,
    /// If set, the session_id was NOT resolved from the ResponseSessionMapper and
    /// the caller should retry the lookup later using this response ID.
    /// When the lookup succeeds, update the `session_id` metadata of all events.
    pub pending_response_id: Option<String>,
}

/// Builder that constructs GenAI semantic events from AnalysisResult
pub struct GenAIBuilder {
    /// Session ID prefix (timestamp-based, unique per agentsight run)
    session_prefix: String,
    /// Counter for generating unique IDs within a session
    call_counter: AtomicU64,
    /// Resolver for `session_id` fallback / `conversation_id` based on the
    /// earliest `response_id` observed within a session / conversation.
    pub(super) id_resolver: IdResolver,
}

impl Default for GenAIBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GenAIBuilder {
    /// Create a new GenAI builder
    pub fn new() -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let pid = std::process::id();
        GenAIBuilder {
            session_prefix: format!("{ts:x}_{pid:x}"),
            call_counter: AtomicU64::new(0),
            id_resolver: IdResolver::new(),
        }
    }

    pub(super) fn pending_match_key(
        pid: u32,
        start_timestamp_ns: u64,
        method: &str,
        path: &str,
        request_body: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"agentsight-pending-v1\0");
        hasher.update(pid.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(start_timestamp_ns.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(method.as_bytes());
        hasher.update(b"\0");
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        if let Some(body) = request_body {
            hasher.update(body.as_bytes());
        }
        format!("pmk-{:x}", hasher.finalize())
    }

    fn parsed_request_match_body(
        request: &ParsedRequest,
        body: Option<&serde_json::Value>,
    ) -> Option<String> {
        body.map(|v| serde_json::to_string(v).unwrap_or_default())
            .or_else(|| {
                let raw = request.body();
                if raw.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(raw).to_string())
                }
            })
    }

    /// Build GenAI semantic events AND a `PendingCallInfo` to be written to DB
    /// before the response arrives.
    ///
    /// Returns `(output, Some(pending_info))` where `pending_info.call_id` matches
    /// the `call_id` embedded inside the returned `LLMCall` event, so the caller can
    /// first `insert_pending(pending_info)` and later `complete_pending(event)`.
    ///
    /// The `BuildOutput` also carries `pending_response_id` when the session_id
    /// could not be resolved from the `ResponseSessionMapper` so the caller can
    /// queue the events for deferred resolution.
    ///
    /// Returns `(output, None)` when no LLM API call was detected in `results`.
    pub fn build_with_pending(
        &self,
        results: &[AnalysisResult],
        response_mapper: &ResponseSessionMapper,
        pid_agent_name_cache: &impl PidAgentNameCache,
    ) -> (BuildOutput, Option<PendingCallInfo>) {
        let timer = StageTimer::start("genai");
        let mut events = Vec::new();
        let mut pending: Option<PendingCallInfo> = None;
        let mut pending_response_id = None;

        if let Some(llm_call) = self.build_llm_call(results, response_mapper, pid_agent_name_cache)
        {
            // Build PendingCallInfo from the same LLMCall before moving it
            let http_record = results.iter().find_map(|r| match r {
                AnalysisResult::Http(h) => Some(h.clone()),
                _ => None,
            });

            // Extract input messages for the pending record
            let (input_messages_json, system_instructions_json) = {
                let sys: Vec<_> = llm_call
                    .request
                    .messages
                    .iter()
                    .filter(|m| m.role == "system")
                    .collect();
                let latest =
                    crate::genai::semantic::latest_round_input_messages(&llm_call.request.messages);
                (
                    if latest.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&latest).ok()
                    },
                    if sys.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&sys).ok()
                    },
                )
            };

            // Determine response_id from call metadata (may come from parsed_message
            // or SSE body fallback), and check if mapper resolved it (either via
            // response_id mapping, or via pid → session fallback for agents like
            // Codex CLI whose rollout file does not embed a response_id).
            let response_id = llm_call.metadata.get("response_id").cloned();
            let mapper_hit = response_id
                .as_deref()
                .and_then(|rid| response_mapper.get_session_by_response_id(rid))
                .is_some()
                || response_mapper
                    .get_session_by_pid(llm_call.pid as u32)
                    .is_some();

            // If response_id exists but mapper didn't resolve session_id, queue
            // for deferred resolution so the next FileWrite event can fix it.
            if response_id.is_some() && !mapper_hit {
                pending_response_id = response_id;
                log::debug!(
                    "GenAI response_id {} not yet in mapper, will defer session_id resolution",
                    pending_response_id.as_deref().unwrap_or_default()
                );
            }

            pending = Some(PendingCallInfo {
                call_id: llm_call.call_id.clone(),
                trace_id: llm_call.metadata.get("response_id").cloned(),
                conversation_id: llm_call.metadata.get("conversation_id").cloned(),
                session_id: llm_call.metadata.get("session_id").cloned(),
                start_timestamp_ns: llm_call.start_timestamp_ns,
                pid: llm_call.pid,
                process_name: llm_call.process_name.clone(),
                agent_name: llm_call.agent_name.clone(),
                http_method: http_record.as_ref().map(|h| h.method.clone()),
                http_path: http_record.as_ref().map(|h| h.path.clone()),
                input_messages: input_messages_json,
                system_instructions: system_instructions_json,
                user_query: llm_call.metadata.get("user_query").cloned(),
                is_sse: llm_call.request.stream,
                model: Some(llm_call.model.clone()),
                provider: Some(llm_call.provider.clone()),
                call_kind: llm_call
                    .metadata
                    .get("call_kind")
                    .cloned()
                    .unwrap_or_else(|| "main".to_string()),
                pending_origin: PendingOrigin::RequestCapture,
                pending_match_key: llm_call.metadata.get("pending_match_key").cloned(),
            });

            events.push(GenAISemanticEvent::LLMCall(llm_call));
        }

        let output = BuildOutput {
            events,
            pending_response_id,
        };
        timer.record_outputs(output.events.len());
        (output, pending)
    }

    /// Build a `PendingCallInfo` directly from a raw `ParsedRequest` and
    /// `ConnectionId`, without needing a full `AnalysisResult`.
    ///
    /// This is used when the event loop detects that a PID has died while its
    /// connection was still in `RequestPending` or `SseActive` state.  By
    /// writing a pending record to `genai_events`, the HealthChecker can later
    /// find it via `list_pending_for_pid` and create a properly correlated
    /// `InterruptionEvent`.
    ///
    /// Returns `None` if the request path is not a known LLM API endpoint or
    /// the body cannot be parsed at all.
    ///
    /// 本函数只会在调用方已经判定"这次追踪的调用不会再正常收到 `finish_reason`"
    /// 的场景下被调用，覆盖两类情况：
    /// 1. 进程已确认退出（`ProcMon::Exit` 触发的即时崩溃检测、定期扫描发现的
    ///    死 PID 清理）；
    /// 2. 进程仍存活，但连接/SSE 流已超过空闲超时（默认 60s 无数据）被判定为
    ///    手动中断或流已放弃（`snapshot_idle_connections`）。
    ///
    /// 因此本函数总是会额外驱逐 `(agent_name, pid, last_user_text)` 对应的
    /// conversation anchor，给这两类中断场景提供与 `call_builder.rs` 中
    /// `finish_reason` 终止态同等的轮结束信号：避免未来该 PID 复用相同固定
    /// 文案（如系统 recap nudge，或用户重发一模一样的 prompt）时，被误判成
    /// 同一轮尚未结束的旧对话。
    ///
    /// 即使原连接后续意外恢复并正常完成（空闲超时属于启发式判断，可能误判），
    /// 驱逐锚点也不会产生错误结果：该调用完成时仍会通过正常路径重新调用
    /// `resolve_conversation_id`，用它自己的 `response_id` 重新锚定，效果与
    /// 未驱逐时一致——只要驱逐后、真正完成前没有其他调用抢占了这个 key。
    pub fn build_pending_from_request(
        &self,
        request: &ParsedRequest,
        conn_id: &ConnectionId,
        response_mapper: &ResponseSessionMapper,
        pid_agent_name_cache: &impl PidAgentNameCache,
    ) -> Option<PendingCallInfo> {
        // Only process known LLM API paths
        let path_match = crate::parser::llm::is_llm_api_path(&request.path);
        let body_str = if request.body_len > 0 {
            Some(request.body_str().to_string())
        } else {
            None
        };
        let body_match = !path_match && Self::is_sysom_pop_request(&body_str);
        if !path_match && !body_match {
            return None;
        }

        let call_id = self.generate_id();
        let body = request.json_body();
        let match_body = Self::parsed_request_match_body(request, body.as_ref());
        let pending_match_key = Self::pending_match_key(
            conn_id.pid,
            request.source_event.timestamp_ns,
            &request.method,
            &request.path,
            match_body.as_deref(),
        );

        // Determine if streaming
        let is_sse = body
            .as_ref()
            .and_then(|v| v.get("stream"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Parse messages from body to extract user_query / input_messages /
        // system_instructions / first_user_text / last_user_text。session_id 与
        // conversation_id 在 request 阶段采用双层兑底：
        //   1. 优先走 IdResolver::peek_*（同 PID 之前有过正常完成的调用 →
        //      LRU 已 anchor 首个 response_id，复用后与正常路径完全对齐）。
        //   2. 未命中时 → `crash_fallback_id`以 (agent_name, pid, user_text) 作为
        //      兑底 ID 输入，保证 crash-drain 路径同 PID 同 user_query 的
        //      crash 记录归一桶，不同 user_query 分桶。
        //
        // 正常响应到达后 `complete_pending` 仍会用 `IdResolver::resolve_*`
        // 重新计算并 UPDATE 正常 ID，只有 crash 路径才会保留这里写入的
        // peek/fallback 值。
        let (
            user_query,
            input_messages,
            system_instructions,
            first_user_text,
            last_user_text,
            user_message_count,
        ) = if let Some(view) = body
            .as_ref()
            .and_then(crate::parser::llm::extract_messages_view)
        {
            let (messages, instructions_text) = view;

            // First user message raw text — used as `session_key` material
            // for IdResolver peek / crash fallback.
            let first_user_text = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .find_map(Self::extract_message_text)
                .unwrap_or_default();

            // Last user message raw text — used for user_query (display text)
            // 以及 conversation_key (peek / crash fallback)。
            let last_user_raw = messages
                .iter()
                .rev()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .find_map(Self::extract_message_text);
            let last_user_text = last_user_raw.clone().unwrap_or_default();

            let user_message_count = Self::count_real_user_messages_from_json(&messages);

            // user_query: last user message text, stripped of metadata prefix
            let user_query = last_user_raw.as_deref().map(Self::strip_user_query_prefix);

            // Serialise message subsets for the pending record
            let sys: Vec<_> = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                .collect();
            let non_sys: Vec<_> = messages
                .iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
                .collect();

            let input_messages = if non_sys.is_empty() {
                None
            } else {
                serde_json::to_string(&non_sys).ok()
            };
            let system_instructions = if sys.is_empty() {
                // Responses API carries the system prompt at the top level
                // via "instructions". Fall back to that when the messages
                // array has no system role.
                instructions_text.map(|s| serde_json::to_string(&s).unwrap_or(s))
            } else {
                serde_json::to_string(&sys).ok()
            };

            (
                user_query,
                input_messages,
                system_instructions,
                first_user_text,
                last_user_text,
                user_message_count,
            )
        } else {
            (None, None, None, String::new(), String::new(), 0)
        };

        // Classify call_kind from request content
        let call_kind =
            super::helpers::classify_call_kind_from_raw(&system_instructions, &first_user_text);

        // Extract model from request body JSON "model" field
        let model = body
            .as_ref()
            .and_then(|v| v.get("model"))
            .and_then(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // Extract provider from request path
        let provider = self.extract_provider_from_path(&request.path);

        // Resolve agent_name: cache → cmdline rule → *process* comm
        // (`<procfs root>/<pid>/comm`). Only if the entry is unreadable do we
        // fall back to the SSL event's per-event thread comm, which may be a
        // library worker-thread name such as "HTTP client".
        let agent_name = Self::resolve_agent_name_from_comm(
            &request.source_event.comm,
            conn_id.pid,
            pid_agent_name_cache,
        )
        .or_else(|| crate::discovery::scanner::read_comm(conn_id.pid))
        .or_else(|| Some(request.source_event.comm_str()));

        // 从 request body.metadata 提取 session_id（复用 types.rs 共享函数）
        let metadata_session: Option<String> = body
            .as_ref()
            .and_then(|b| b.get("metadata"))
            .and_then(crate::analyzer::message::types::session_id_from_metadata);

        // 双层兜底计算 session_id / conversation_id（详见上方注释）。
        // 这里不使用 unwrap_or_else(|| "") 是为了让“同 PID 同 agent”上下文下
        // crash_fallback_id 输入始终相同。
        let agent_name_str = agent_name.as_deref().unwrap_or("");
        let pid_i32 = conn_id.pid as i32;

        let session_id = metadata_session
            .or_else(|| {
                // Mapper first (same order as call_builder.rs): a FileWrite
                // from this pid already revealed the real session UUID, which
                // groups the crash-drain record with the normal calls of the
                // same session instead of an isolated crash bucket (#2059).
                //
                // Known limitation: pid_map has no lifecycle bound, so a
                // recycled pid whose new process has not yet written a session
                // file can surface the previous process's mapping here. This
                // mis-groups only orphan calls that would otherwise get a
                // synthetic crash hash, and needs pid reuse + zero FileWrite +
                // drain to coincide — accepted instead of lifecycle tracking.
                response_mapper
                    .get_session_by_pid(conn_id.pid)
                    .map(str::to_string)
            })
            .or_else(|| {
                self.id_resolver
                    .peek_session_id(agent_name_str, pid_i32, &first_user_text)
            })
            .or_else(|| {
                Some(super::id_resolver::crash_fallback_id(
                    "session",
                    agent_name_str,
                    pid_i32,
                    &first_user_text,
                    0,
                ))
            });
        let conversation_id = self
            .id_resolver
            .peek_conversation_id(agent_name_str, pid_i32, &last_user_text, user_message_count)
            .or_else(|| {
                Some(super::id_resolver::crash_fallback_id(
                    "conversation",
                    agent_name_str,
                    pid_i32,
                    &last_user_text,
                    user_message_count,
                ))
            });

        // 若调用方已确认本次调用不会再有后续 finish_reason（进程崩溃 /
        // 本函数只在中断场景下被调用（进程崩溃/异常退出，或连接空闲超时被
        // 判定为中断），此轮对话不会再有后续 finish_reason 信号。在这里显式
        // 驱逐锚点，给这两类中断场景提供与 call_builder.rs 中 finish_reason
        // 终止态同等的轮结束信号，避免未来同 PID 复用相同固定文案（如系统
        // recap nudge、或用户重发一模一样的 prompt）时被归入同一轮。
        self.id_resolver.finish_conversation(
            agent_name_str,
            pid_i32,
            &last_user_text,
            user_message_count,
        );

        Some(PendingCallInfo {
            call_id,
            trace_id: None, // LLM API response_id, not available until response
            // session_id / conversation_id 在请求阶段采用双层兑底：
            // 1) IdResolver::peek_* 复用同 PID 之前正常完成调用的 anchor，
            //    响应到达后 `complete_pending` 会用同样的值覆盖；
            // 2) LRU miss 时走 `crash_fallback_id`（`crash-` 前缀与正常 ID 隔离），
            //    进程崩溃不会走到 complete_pending 时该值会保留，供
            //    handle_agent_crash_detection 按 (sid, cid) 分组。
            conversation_id,
            session_id,
            start_timestamp_ns: request.source_event.timestamp_ns,
            pid: pid_i32,
            // Process name = the *process* comm, not the per-event thread comm.
            process_name: crate::discovery::scanner::read_comm(conn_id.pid)
                .unwrap_or_else(|| request.source_event.comm.clone()),
            agent_name,
            http_method: Some(request.method.clone()),
            http_path: Some(request.path.clone()),
            input_messages,
            system_instructions,
            user_query,
            is_sse,
            model,
            provider,
            call_kind: call_kind.to_string(),
            pending_origin: PendingOrigin::DeadPidDrain,
            pending_match_key: Some(pending_match_key),
        })
    }

    /// Extract enrichment data from SSE events captured before the process died.
    ///
    /// Parses sse_events for:
    /// - model name (from first chunk's "model" field)
    /// - trace_id / response_id (from first chunk's "id" field)
    /// - token usage (via TokenParser, from DashScope-style usage chunks)
    /// - output content (merged content deltas)
    ///
    /// Returns `None` if sse_events is empty.
    pub fn extract_sse_enrichment(sse_events: &[ParsedSseEvent]) -> Option<SseEnrichment> {
        if sse_events.is_empty() {
            return None;
        }

        let token_parser = TokenParser::new();
        let mut model: Option<String> = None;
        let mut trace_id: Option<String> = None;
        let mut chunks: Vec<serde_json::Value> = Vec::new();

        // Forward scan for model, trace_id; collect the JSON bodies for the
        // shared part merger below.
        for event in sse_events {
            if let Some(json) = event.json_body() {
                // Extract model from first chunk that has it
                if model.is_none() {
                    if let Some(m) = json.get("model").and_then(|v| v.as_str()) {
                        if !m.is_empty() {
                            model = Some(m.to_string());
                        }
                    }
                }
                // Anthropic nests the model inside message_start's message
                // object; none of its events carries a top-level "model" key,
                // so without this lookup a drained Anthropic stream records
                // no model from the response at all.
                if model.is_none()
                    && json.get("type").and_then(|v| v.as_str()) == Some("message_start")
                {
                    if let Some(m) = json.pointer("/message/model").and_then(|v| v.as_str()) {
                        if !m.is_empty() {
                            model = Some(m.to_string());
                        }
                    }
                }
                // The Responses protocol nests both the model and the response
                // id inside the `response` object of response.created /
                // response.completed; its chunks carry no top-level keys for
                // either.
                if model.is_none() {
                    if let Some(m) = json.pointer("/response/model").and_then(|v| v.as_str()) {
                        if !m.is_empty() {
                            model = Some(m.to_string());
                        }
                    }
                }
                // Extract response id (trace_id) from first chunk that has it
                if trace_id.is_none() {
                    if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            trace_id = Some(id.to_string());
                        }
                    }
                }
                if trace_id.is_none() {
                    if let Some(id) = json.pointer("/response/id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            trace_id = Some(id.to_string());
                        }
                    }
                }
                chunks.push(json);
            }
        }

        // Merge token usage across every event. Anthropic splits it: the
        // `message_start` event carries input_tokens plus the cache counters
        // while the terminal `message_delta` carries only output_tokens, so
        // keeping the last usage-bearing event would record input as 0.
        let usage = sse_events
            .iter()
            .filter_map(|e| token_parser.parse_event(e))
            .fold(None, merge_usage);

        let (input_tokens, output_tokens) = match &usage {
            Some(u) => (Some(u.input_tokens as i64), Some(u.output_tokens as i64)),
            None => (None, None),
        };

        // Use model from usage if not found in content chunks
        if model.is_none() {
            if let Some(ref u) = usage {
                model = u.model.clone();
            }
        }

        // Persist the output through the SAME part merger the live response
        // path uses, so drained rows carry the internally-tagged
        // (`"type": "text" | …`) shape every typed consumer deserializes —
        // and keep tool-call and reasoning deltas instead of dropping them.
        // The old hand-built `[{"Text": …}]` externally-tagged payload failed
        // `Vec<OutputMessage>` parsing ("missing field `type`"), silently
        // losing the row in skill metrics and ATIF export.
        let (parts, finish_reason) = Self::merge_sse_chunks(&chunks);
        let output_messages = if parts.is_empty() {
            None
        } else {
            serde_json::to_string(&vec![OutputMessage {
                role: "assistant".to_string(),
                parts,
                name: None,
                finish_reason,
            }])
            .ok()
        };

        let event_count = sse_events.len() as i64;

        Some(SseEnrichment {
            model,
            trace_id,
            provider: None, // provider already set from request path in insert_pending
            output_messages,
            sse_event_count: Some(event_count),
            input_tokens,
            output_tokens,
        })
    }

    /// Count "real" user messages from a raw JSON messages array.
    ///
    /// A real user message has `role="user"` AND content that is either a
    /// non-empty string, or an array containing at least one item with type
    /// `"text"`, `"input_text"`, or `"output_text"` and non-empty text.
    /// Mirrors the text detection logic from `extract_message_text`.
    fn count_real_user_messages_from_json(messages: &[serde_json::Value]) -> usize {
        messages
            .iter()
            .filter(|m| {
                m.get("role").and_then(|r| r.as_str()) == Some("user")
                    && Self::extract_message_text(m).is_some()
            })
            .count()
    }

    /// Generate globally unique ID (unique across restarts)
    pub(super) fn generate_id(&self) -> String {
        let seq = self.call_counter.fetch_add(1, Ordering::Relaxed);
        format!("{}_{}", self.session_prefix, seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genai::semantic::MessagePart;
    use crate::probes::sslsniff::SslEvent;
    use std::rc::Rc;

    fn make_request(path: &str, body: &str) -> ParsedRequest {
        let buf = body.as_bytes().to_vec();
        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1000,
            delta_ns: 0,
            pid: 1234,
            tid: 1,
            uid: 0,
            len: buf.len() as u32,
            rw: 1,
            comm: "test".to_string(),
            buf,
            is_handshake: false,
            ssl_ptr: 0x1,
        });
        ParsedRequest {
            method: "POST".to_string(),
            path: path.to_string(),
            version: 11,
            headers: std::collections::HashMap::new(),
            body_offset: 0,
            body_len: body.len(),
            source_event: ssl_event,
            reassembled_body: None,
        }
    }

    /// Build a zero-copy `ParsedSseEvent` whose single `data:` line is `data`.
    fn make_sse_event(data: &str) -> ParsedSseEvent {
        let buf = data.as_bytes().to_vec();
        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1000,
            delta_ns: 0,
            pid: 1234,
            tid: 1,
            uid: 0,
            len: buf.len() as u32,
            rw: 0,
            comm: "test".to_string(),
            buf,
            is_handshake: false,
            ssl_ptr: 0x1,
        });
        ParsedSseEvent::new(None, None, None, 0, data.len(), ssl_event)
    }

    /// Anthropic splits usage across SSE events: `message_start` carries
    /// input_tokens plus the cache counters, while the terminal
    /// `message_delta` carries only output_tokens. The drain path must merge
    /// both, exactly like the analyzer's SSE token extractor, instead of
    /// letting the last usage-bearing event win and recording input as 0.
    #[test]
    fn test_extract_sse_enrichment_output_messages_round_trips() {
        // The persisted output_messages must deserialize as
        // Vec<OutputMessage> — the old hand-built `[{"Text": …}]` shape
        // failed with "missing field `type`" and silently dropped the row
        // in every typed consumer (skill metrics, ATIF export).
        let events = vec![
            make_sse_event(
                r#"{"model":"qwen-max","id":"resp_1","choices":[{"delta":{"content":"Hel"}}]}"#,
            ),
            make_sse_event(
                r#"{"choices":[{"delta":{"content":"lo world"},"finish_reason":null}]}"#,
            ),
            make_sse_event(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
        ];
        let enrichment = GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment");
        let json = enrichment
            .output_messages
            .expect("output_messages must be set");
        let parsed: Vec<OutputMessage> =
            serde_json::from_str(&json).expect("must round-trip as Vec<OutputMessage>");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].role, "assistant");
        assert_eq!(parsed[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(parsed[0].parts.len(), 1);
        match &parsed[0].parts[0] {
            MessagePart::Text { content } => assert_eq!(content, "Hello world"),
            other => panic!("expected Text part, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_sse_enrichment_captures_streamed_tool_calls() {
        // A pure tool-calling turn: the old walker only read delta.content,
        // so output_messages was None; the shared merger now keeps the
        // index-merged tool call.
        let events = vec![
            make_sse_event(
                r#"{"model":"qwen-max","id":"resp_2","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}"#,
            ),
            make_sse_event(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Beijing\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            ),
        ];
        let enrichment = GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment");
        let json = enrichment
            .output_messages
            .expect("tool calls must be persisted");
        let parsed: Vec<OutputMessage> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0].finish_reason.as_deref(), Some("tool_calls"));
        match &parsed[0].parts[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name, "get_weather");
                assert_eq!(
                    arguments
                        .as_ref()
                        .and_then(|a| a.get("city"))
                        .and_then(|v| v.as_str()),
                    Some("Beijing")
                );
            }
            other => panic!("expected ToolCall part, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_sse_enrichment_captures_reasoning_deltas() {
        let events = vec![
            make_sse_event(
                r#"{"model":"deepseek-r","id":"resp_3","choices":[{"delta":{"reasoning_content":"think "}}]}"#,
            ),
            make_sse_event(
                r#"{"choices":[{"delta":{"reasoning_content":"hard","content":"answer"},"finish_reason":"stop"}]}"#,
            ),
        ];
        let enrichment = GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment");
        let json = enrichment
            .output_messages
            .expect("reasoning must be persisted");
        let parsed: Vec<OutputMessage> = serde_json::from_str(&json).unwrap();
        // Reasoning precedes text (live-path merger ordering).
        assert_eq!(parsed[0].parts.len(), 2);
        assert!(
            matches!(&parsed[0].parts[0], MessagePart::Reasoning { content } if content == "think hard")
        );
        assert!(
            matches!(&parsed[0].parts[1], MessagePart::Text { content } if content == "answer")
        );
    }

    #[test]
    fn test_extract_sse_enrichment_matches_live_path_shape() {
        // Guard against future shape drift: serializing the same stream
        // through the live-path types must produce the identical JSON the
        // enrichment persists.
        let events = vec![make_sse_event(
            r#"{"model":"m","id":"i","choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}"#,
        )];
        let enrichment = GenAIBuilder::extract_sse_enrichment(&events).unwrap();
        let drain_json = enrichment.output_messages.unwrap();

        let body = r#"[{"model":"m","id":"i","choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}]"#;
        let (parts, finish_reason) = GenAIBuilder::extract_parts_from_sse_body(body).unwrap();
        let live_json = serde_json::to_string(&vec![OutputMessage {
            role: "assistant".to_string(),
            parts,
            name: None,
            finish_reason,
        }])
        .unwrap();
        assert_eq!(drain_json, live_json);
    }

    #[test]
    fn test_extract_sse_enrichment_empty_stream_yields_none() {
        // A stream with no output deltas keeps output_messages = None
        // (existing behavior for usage-only events).
        let events = vec![make_sse_event(
            r#"{"model":"m","id":"i","usage":{"prompt_tokens":5,"completion_tokens":0}}"#,
        )];
        let enrichment = GenAIBuilder::extract_sse_enrichment(&events).unwrap();
        assert!(enrichment.output_messages.is_none());
    }

    #[test]
    fn test_extract_sse_enrichment_merges_anthropic_split_usage() {
        let events = vec![
            make_sse_event(
                r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":1234,"output_tokens":1,"cache_creation_input_tokens":5678,"cache_read_input_tokens":90}}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
            ),
            make_sse_event(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":42}}"#,
            ),
        ];

        let enrichment =
            GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment from anthropic SSE");
        assert_eq!(
            enrichment.input_tokens,
            Some(1234),
            "input_tokens from message_start must survive the message_delta"
        );
        assert_eq!(enrichment.output_tokens, Some(42));
    }

    /// A drained Anthropic stream must keep its output content and model, not
    /// just the usage. The live path reconstructs Anthropic content through the
    /// analyzer's message parser, but the drain path has only
    /// `extract_sse_enrichment`, whose merger previously understood OpenAI
    /// `choices[].delta` alone — so the persisted row lost `output_messages`
    /// (and the model, which Anthropic nests inside `message_start.message`).
    #[test]
    fn test_extract_sse_enrichment_anthropic_stream_keeps_content() {
        let events = vec![
            make_sse_event(
                r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":1234,"output_tokens":1}}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo world"}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"Paris\"}"}}"#,
            ),
            make_sse_event(
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42}}"#,
            ),
        ];

        let enrichment =
            GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment from anthropic SSE");

        // The model is nested in message_start.message, not top-level.
        assert_eq!(
            enrichment.model.as_deref(),
            Some("claude-sonnet-4-5"),
            "model must be read from message_start.message.model"
        );

        let json = enrichment
            .output_messages
            .expect("drained anthropic output must be persisted");
        let parsed: Vec<OutputMessage> =
            serde_json::from_str(&json).expect("must round-trip as Vec<OutputMessage>");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].finish_reason.as_deref(), Some("tool_use"));
        assert_eq!(parsed[0].parts.len(), 2, "text part + tool_use part");
        match &parsed[0].parts[0] {
            MessagePart::Text { content } => assert_eq!(content, "Hello world"),
            other => panic!("expected Text part, got {other:?}"),
        }
        match &parsed[0].parts[1] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("toolu_1"));
                assert_eq!(name, "get_weather");
                assert_eq!(
                    arguments,
                    &Some(serde_json::json!({"city": "Paris"})),
                    "input_json_delta fragments must concatenate into the arguments"
                );
            }
            other => panic!("expected ToolCall part, got {other:?}"),
        }
    }

    /// A thinking block must survive the drain path too, in block order before
    /// the text that follows it.
    #[test]
    fn test_extract_sse_enrichment_anthropic_stream_keeps_reasoning() {
        let events = vec![
            make_sse_event(
                r#"{"type":"message_start","message":{"id":"msg_2","role":"assistant","model":"claude-sonnet-4-5","content":[],"usage":{"input_tokens":10,"output_tokens":1}}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"pondering"}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            ),
            make_sse_event(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}"#,
            ),
            make_sse_event(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
            ),
        ];

        let enrichment =
            GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment from anthropic SSE");
        let json = enrichment
            .output_messages
            .expect("drained anthropic output must be persisted");
        let parsed: Vec<OutputMessage> =
            serde_json::from_str(&json).expect("must round-trip as Vec<OutputMessage>");
        assert_eq!(parsed[0].parts.len(), 2);
        assert!(matches!(
            &parsed[0].parts[0],
            MessagePart::Reasoning { content } if content == "pondering"
        ));
        assert!(matches!(
            &parsed[0].parts[1],
            MessagePart::Text { content } if content == "answer"
        ));
    }

    /// A drained OpenAI **Responses** stream (codex 0.137+ via /v1/responses)
    /// must keep its output content, model and trace_id. The live path
    /// reconstructs `response.*` events through the analyzer's message parser,
    /// but the drain path had no Responses aggregation at all — the persisted
    /// row lost `output_messages`, the model (nested in `response.model`) and
    /// the trace_id (nested in `response.id`).
    #[test]
    fn test_extract_sse_enrichment_responses_stream_keeps_content() {
        let events = vec![
            make_sse_event(
                r#"{"type":"response.created","response":{"id":"resp_1","model":"qwen3-coder-plus"}}"#,
            ),
            make_sse_event(r#"{"type":"response.output_text.delta","delta":"Hel"}"#),
            make_sse_event(r#"{"type":"response.output_text.delta","delta":"lo"}"#),
            make_sse_event(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"read_file"}}"#,
            ),
            make_sse_event(
                r#"{"type":"response.function_call_arguments.delta","delta":"{\"path\":"}"#,
            ),
            make_sse_event(
                r#"{"type":"response.function_call_arguments.delta","delta":"\"/tmp/a.md\"}"}"#,
            ),
            make_sse_event(
                r#"{"type":"response.completed","response":{"id":"resp_1","model":"qwen3-coder-plus","usage":{"input_tokens":100,"output_tokens":7,"total_tokens":107}}}"#,
            ),
        ];

        let enrichment =
            GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment from responses SSE");

        assert_eq!(
            enrichment.model.as_deref(),
            Some("qwen3-coder-plus"),
            "model must be read from response.model"
        );
        assert_eq!(
            enrichment.trace_id.as_deref(),
            Some("resp_1"),
            "trace_id must be read from response.id"
        );
        assert_eq!(enrichment.input_tokens, Some(100));
        assert_eq!(enrichment.output_tokens, Some(7));

        let json = enrichment
            .output_messages
            .expect("drained responses output must be persisted");
        let parsed: Vec<OutputMessage> =
            serde_json::from_str(&json).expect("must round-trip as Vec<OutputMessage>");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(parsed[0].parts.len(), 2, "text part + tool_call part");
        assert!(matches!(
            &parsed[0].parts[0],
            MessagePart::Text { content } if content == "Hello"
        ));
        match &parsed[0].parts[1] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name, "read_file");
                assert_eq!(
                    arguments,
                    &Some(serde_json::json!({"path": "/tmp/a.md"})),
                    "argument deltas must concatenate"
                );
            }
            other => panic!("expected ToolCall part, got {other:?}"),
        }
    }

    /// Parallel Responses tool calls without per-call `done` events must all
    /// survive the drain path (same invariant the analyzer aggregator holds).
    #[test]
    fn test_extract_sse_enrichment_responses_parallel_tool_calls() {
        let events = vec![
            make_sse_event(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"read_file"}}"#,
            ),
            make_sse_event(
                r#"{"type":"response.function_call_arguments.delta","delta":"{\"a\": 1}"}"#,
            ),
            make_sse_event(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_2","name":"list_dir"}}"#,
            ),
            make_sse_event(
                r#"{"type":"response.function_call_arguments.delta","delta":"{\"b\": 2}"}"#,
            ),
            make_sse_event(
                r#"{"type":"response.completed","response":{"id":"resp_2","model":"m","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
            ),
        ];

        let enrichment =
            GenAIBuilder::extract_sse_enrichment(&events).expect("enrichment from responses SSE");
        let json = enrichment
            .output_messages
            .expect("drained responses output must be persisted");
        let parsed: Vec<OutputMessage> =
            serde_json::from_str(&json).expect("must round-trip as Vec<OutputMessage>");
        let tool_calls: Vec<&MessagePart> = parsed[0]
            .parts
            .iter()
            .filter(|p| matches!(p, MessagePart::ToolCall { .. }))
            .collect();
        assert_eq!(
            tool_calls.len(),
            2,
            "both in-flight calls must survive without done events"
        );
        match tool_calls[0] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name, "read_file");
                assert_eq!(arguments, &Some(serde_json::json!({"a": 1})));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match tool_calls[1] {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_2"));
                assert_eq!(name, "list_dir");
                assert_eq!(arguments, &Some(serde_json::json!({"b": 2})));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn test_generate_id_unique() {
        let builder = GenAIBuilder::new();
        let id1 = builder.generate_id();
        let id2 = builder.generate_id();
        assert_ne!(id1, id2);
        assert!(id1.contains('_'));
    }

    #[test]
    fn test_default_builder() {
        let b1 = GenAIBuilder::default();
        let b2 = GenAIBuilder::new();
        // Both should have different session prefixes (different timestamps)
        // But both should generate valid IDs
        let id1 = b1.generate_id();
        let id2 = b2.generate_id();
        assert!(id1.contains('_'));
        assert!(id2.contains('_'));
    }

    #[test]
    fn test_build_pending_from_request_chat_completions() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","messages":[{"role":"system","content":"sys"},{"role":"user","content":"hello"}]}"#;
        let req = make_request("/v1/chat/completions", body);
        let mapper = ResponseSessionMapper::new();
        let cache = std::collections::HashMap::new();
        let pending = builder
            .build_pending_from_request(&req, &ConnectionId { pid: 1, ssl_ptr: 2 }, &mapper, &cache)
            .unwrap();
        assert_eq!(pending.model.as_deref(), Some("gpt-4"));
        assert_eq!(pending.provider.as_deref(), Some("openai"));
        assert!(pending.system_instructions.is_some());
        assert!(pending.user_query.as_deref() == Some("hello"));
        assert_eq!(pending.call_kind, "main");
    }

    #[test]
    fn test_build_pending_from_request_responses_api() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","input":[{"role":"user","content":"hello"}],"instructions":"sys prompt"}"#;
        let req = make_request("/v1/responses", body);
        let mapper = ResponseSessionMapper::new();
        let cache = std::collections::HashMap::new();
        let pending = builder
            .build_pending_from_request(&req, &ConnectionId { pid: 1, ssl_ptr: 2 }, &mapper, &cache)
            .unwrap();
        assert_eq!(pending.model.as_deref(), Some("gpt-4"));
        assert_eq!(pending.provider.as_deref(), Some("openai"));
        assert!(pending.system_instructions.is_some());
        assert!(pending.user_query.as_deref() == Some("hello"));
    }

    #[test]
    fn test_build_pending_from_request_non_llm_path() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","messages":[]}"#;
        let req = make_request("/api/health", body);
        let mapper = ResponseSessionMapper::new();
        let cache = std::collections::HashMap::new();
        assert!(
            builder
                .build_pending_from_request(
                    &req,
                    &ConnectionId { pid: 1, ssl_ptr: 2 },
                    &mapper,
                    &cache
                )
                .is_none()
        );
    }

    #[test]
    fn test_build_pending_from_request_llm_path_no_messages_view() {
        let builder = GenAIBuilder::new();
        // LLM path but body lacks both "messages" and "input".
        let body = r#"{"model":"gpt-4","stream":true}"#;
        let req = make_request("/v1/chat/completions", body);
        let mapper = ResponseSessionMapper::new();
        let cache = std::collections::HashMap::new();
        let pending = builder
            .build_pending_from_request(&req, &ConnectionId { pid: 1, ssl_ptr: 2 }, &mapper, &cache)
            .expect("LLM path should still create pending even without messages");
        assert_eq!(pending.model.as_deref(), Some("gpt-4"));
        assert!(pending.user_query.is_none());
        assert!(pending.input_messages.is_none());
        assert!(pending.system_instructions.is_none());
    }

    #[test]
    fn test_build_pending_from_request_count_tokens_is_skipped() {
        // The drain path must not persist a pending row for an interrupted
        // count-tokens call either: same conversation as the real turn, no
        // max_tokens, no usage — a phantom interrupted llm_call.
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"Long document"}]}"#;
        let req = make_request("/v1/messages/count_tokens", body);
        let mapper = ResponseSessionMapper::new();
        let cache = std::collections::HashMap::new();
        assert!(
            builder
                .build_pending_from_request(
                    &req,
                    &ConnectionId { pid: 1, ssl_ptr: 2 },
                    &mapper,
                    &cache
                )
                .is_none(),
            "count_tokens must not create a pending row"
        );
    }

    #[test]
    fn test_build_pending_from_request_responses_retrieval_is_skipped() {
        // The drain path must not persist a pending row for an interrupted
        // retrieval poll either: a GET /v1/responses/{id} has no request
        // body to anchor a conversation — a phantom interrupted llm_call
        // with no user input whose response would land on the next drain.
        let builder = GenAIBuilder::new();
        let mut req = make_request("/v1/responses/resp_abc123", "");
        req.method = "GET".to_string();
        let mapper = ResponseSessionMapper::new();
        let cache = std::collections::HashMap::new();
        assert!(
            builder
                .build_pending_from_request(
                    &req,
                    &ConnectionId { pid: 1, ssl_ptr: 2 },
                    &mapper,
                    &cache
                )
                .is_none(),
            "a retrieval poll must not create a pending row"
        );
    }

    #[test]
    fn test_build_pending_from_request_evicts_conversation_anchor() {
        // build_pending_from_request 只在中断场景（进程崩溃或空闲超时）下被
        // 调用，因此总是会驱逐 conversation 锚点：固定文本先通过正常路径
        // 锁定一个 conversation_id，然后该轮调用确认不会再有 finish_reason，
        // 再次调用 `build_pending_from_request` 应驱逐该锚点，使同 PID
        // 同固定文本的下一轮对话重新锚定。
        let builder = GenAIBuilder::new();
        let pid = 1i32;
        let text = "hello";
        let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"hello"}]}"#;
        let req = make_request("/v1/chat/completions", body);
        let mapper = ResponseSessionMapper::new();
        let cache = std::collections::HashMap::new();

        // 先跑一次拿到 build_pending_from_request 实际解析出的 agent_name
        // （测试环境下可能因为 `/proc/1/comm` 真实存在而解析成实际进程名，
        // 不一定是 make_request 里设置的 "test"，所以直接从返回值里读，
        // 保证后面手动调用 `resolve_conversation_id` 时用的 key 与它一致）。
        let pending = builder
            .build_pending_from_request(&req, &ConnectionId { pid: 1, ssl_ptr: 2 }, &mapper, &cache)
            .expect("LLM path should create pending");
        let agent_name = pending.agent_name.clone().unwrap_or_default();

        // 1. 模拟同轮内一次正常完成的 LLM 调用，锚定一个 conversation_id。
        let turn1 = builder
            .id_resolver
            .resolve_conversation_id(&agent_name, pid, text, "resp-1", 1)
            .unwrap();

        // 2. 该轮后续调用超时/进程崩溃，确认不会再有 finish_reason。
        builder
            .build_pending_from_request(&req, &ConnectionId { pid: 1, ssl_ptr: 2 }, &mapper, &cache)
            .expect("LLM path should create pending");

        // 3. 数十分钟后同一段固定文本触发了一轮全新的真实对话，应得到不同的 conversation_id。
        let turn2 = builder
            .id_resolver
            .resolve_conversation_id(&agent_name, pid, text, "resp-2", 1)
            .unwrap();
        assert_ne!(
            turn1, turn2,
            "build_pending_from_request 应驱逐锚点，让相同文本开启新的一轮对话"
        );
    }

    /// A pid → session UUID mapping registered by a FileWrite event must win
    /// over the peek/crash-fallback chain in the crash-drain path (#2059).
    /// Reverting the mapper lookup in `build_pending_from_request` makes this
    /// test fail (session_id would become the 32-hex crash fallback).
    #[test]
    fn test_build_pending_from_request_uses_mapper_pid_session() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"hello"}]}"#;
        let req = make_request("/v1/chat/completions", body);
        let cache = std::collections::HashMap::new();

        // Pre-seed pid 4242 → session UUID via a cosh-core atomic-write temp
        // file, the same way the filewrite probe feeds the mapper at runtime.
        let mut mapper = ResponseSessionMapper::new();
        mapper.process_filewrite(&crate::probes::FileWriteEvent {
            pid: 4242,
            tid: 4242,
            uid: 0,
            timestamp_ns: 0,
            write_size: 0,
            comm: "cosh-core".to_string(),
            filename:
                ".550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp"
                    .to_string(),
            cgroup_id: 0,
            buf: Vec::new(),
        });

        let pending = builder
            .build_pending_from_request(
                &req,
                &ConnectionId {
                    pid: 4242,
                    ssl_ptr: 2,
                },
                &mapper,
                &cache,
            )
            .expect("LLM path should create pending");
        assert_eq!(
            pending.session_id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000"),
            "mapper pid → session UUID must win over the crash fallback"
        );
    }

    /// Without a mapper hit the crash-drain path must keep its previous
    /// behavior: fall back to the 32-hex `crash_fallback_id` (peek misses on
    /// a fresh builder).
    #[test]
    fn test_build_pending_from_request_falls_back_without_mapper_hit() {
        let builder = GenAIBuilder::new();
        let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"hello"}]}"#;
        let req = make_request("/v1/chat/completions", body);
        let cache = std::collections::HashMap::new();
        // Mapper knows a different pid only.
        let mut mapper = ResponseSessionMapper::new();
        mapper.process_filewrite(&crate::probes::FileWriteEvent {
            pid: 9999,
            tid: 9999,
            uid: 0,
            timestamp_ns: 0,
            write_size: 0,
            comm: "cosh-core".to_string(),
            filename:
                ".550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp"
                    .to_string(),
            cgroup_id: 0,
            buf: Vec::new(),
        });

        let pending = builder
            .build_pending_from_request(
                &req,
                &ConnectionId {
                    pid: 4242,
                    ssl_ptr: 2,
                },
                &mapper,
                &cache,
            )
            .expect("LLM path should create pending");
        let session_id = pending.session_id.expect("fallback session_id");
        assert_eq!(
            session_id.len(),
            32,
            "unmapped pid must keep the 32-hex crash fallback, got {session_id}"
        );
    }
}
