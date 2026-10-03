//! Interruption detection rules applied to individual LLMCall events.
//!
//! # Online path (called immediately after each LLMCall is built)
//! `InterruptionDetector::detect(call)` checks a single call against all
//! single-call rules and returns any detected interruption events.
//!
//! # Tool-use path
//! `InterruptionDetector::detect_tool_use(tool)` inspects a `ToolUse` event
//! and returns a `tool_failure` interruption if the tool execution failed.

use super::types::{InterruptionEvent, InterruptionType};
use crate::genai::semantic::{LLMCall, MessagePart, ToolUse};

/// Whether the finish reason indicates a normal (non-truncated) end of generation.
///
/// `pause_turn` (Anthropic) means a long-running turn was paused by the server
/// and will be resumed by the client; the SSE stream itself completed normally,
/// so it must not be treated as truncation.
fn is_normal_finish(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some("stop" | "tool_calls" | "end_turn" | "tool_use" | "stop_sequence" | "pause_turn")
    )
}

/// Whether the finish reason indicates a token-limit stop (handled by rules 9/10).
fn is_token_limit_finish(reason: Option<&str>) -> bool {
    matches!(reason, Some("length" | "max_tokens"))
}

fn structured_stream_error_text(raw_body: &str) -> Option<String> {
    if let Some(error) = structured_error_json_text(raw_body.trim()) {
        return Some(error);
    }

    raw_body.lines().find_map(|line| {
        let payload = line
            .trim()
            .strip_prefix("data:")
            .map(str::trim)
            .unwrap_or_else(|| line.trim());
        if payload.is_empty() || payload == "[DONE]" || payload.starts_with("event:") {
            return None;
        }
        structured_error_json_text(payload)
    })
}

fn structured_error_json_text(candidate: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(candidate)
        .ok()
        .and_then(|value| structured_error_value_text(&value))
}

fn structured_error_value_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Array(items) => items.iter().find_map(structured_error_value_text),
        serde_json::Value::Object(map) => {
            let event_type = map.get("type").and_then(|value| value.as_str());
            let is_error_event = event_type.is_some_and(|kind| {
                kind == "error" || kind.ends_with(".failed") || kind.ends_with(".error")
            });

            if let Some(error) = map.get("error") {
                return summarize_error_value(error);
            }

            let response_error = map
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(summarize_error_value);
            if let Some(error) = response_error {
                return Some(error);
            }

            if is_error_event {
                return summarize_error_value(value);
            }

            None
        }
        _ => None,
    }
}

fn summarize_error_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        serde_json::Value::Object(map) => {
            let text = ["type", "code", "message"]
                .iter()
                .filter_map(|key| map.get(*key).and_then(|value| value.as_str()))
                .filter(|text| !text.trim().is_empty())
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

fn is_unauthorized_error_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("permission denied")
        || lower.contains("permission_denied")
        || lower.contains("forbidden")
        || lower.contains("sandbox")
        || lower.contains("not allowed")
        || lower.contains("access denied")
        || lower.contains("operation not permitted")
        || lower.contains("eperm")
        || lower.contains("eacces")
}

/// Checks for structured/specific tool-failure signals in free-form text.
///
/// Deliberately excludes generic words like "exception" or "failed" that
/// commonly appear in normal, non-error tool output (e.g. "handled the
/// exception gracefully"), which would otherwise cause false positives.
fn text_has_tool_error_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("traceback")
        || lower.contains("exit code")
        || lower.contains("no such file or directory")
        || lower.contains("permission denied")
        || lower.contains("command not found")
        || lower.contains("eacces")
        || lower.contains("eperm")
        || lower.contains("\"status\": \"error\"")
        || lower.contains("\"status\":") && lower.contains("\"error\"")
}

fn tool_response_failure_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(error) = tool_response_failure_text(&parsed) {
                    return Some(error);
                }
            }
            if text_has_tool_error_signal(trimmed) {
                Some(trimmed.to_string())
            } else {
                None
            }
        }
        serde_json::Value::Array(items) => items.iter().find_map(tool_response_failure_text),
        serde_json::Value::Object(map) => {
            let explicit_error = map
                .get("is_error")
                .or_else(|| map.get("isError"))
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
                || map
                    .get("success")
                    .and_then(|value| value.as_bool())
                    .is_some_and(|success| !success)
                || map
                    .get("status")
                    .and_then(|value| value.as_str())
                    .is_some_and(|status| status.eq_ignore_ascii_case("error"));

            let nested_error = ["error", "message", "content", "response", "details"]
                .iter()
                .filter_map(|key| map.get(*key))
                .find_map(tool_response_failure_text);

            if let Some(error) = nested_error {
                return Some(error);
            }

            if explicit_error {
                return Some(value.to_string());
            }

            map.values().find_map(tool_response_failure_text)
        }
        _ => None,
    }
}

fn traced_tool_failure(call: &LLMCall) -> Option<(&'static str, Option<String>, String)> {
    let request_parts = call
        .request
        .messages
        .iter()
        .flat_map(|message| message.parts.iter());
    let response_parts = call
        .response
        .messages
        .iter()
        .flat_map(|message| message.parts.iter());

    request_parts.chain(response_parts).find_map(|part| {
        if let MessagePart::ToolCallResponse { id, response } = part {
            tool_response_failure_text(response)
                .map(|error| ("tool_call_response", id.clone(), error))
        } else {
            None
        }
    })
}

/// Configuration for the interruption detector
pub struct DetectorConfig {
    /// Ratio of output_tokens / max_tokens that triggers token_limit (default: 0.95)
    pub token_limit_ratio: f64,
    /// Minimum call duration to consider sse_truncated (avoid fast-fail false positives)
    pub sse_min_duration_ns: u64,
    /// Call duration threshold that triggers slow_response (default: 120 seconds)
    pub slow_response_ns_threshold: u64,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        DetectorConfig {
            token_limit_ratio: 0.95,
            sse_min_duration_ns: 1_000_000_000, // 1 second
            slow_response_ns_threshold: 120_000_000_000, // 120 seconds
        }
    }
}

pub struct InterruptionDetector {
    pub config: DetectorConfig,
    /// When false, `detect` returns an empty vector without doing any work.
    /// Used to gate the interruption-detection feature via `agentsight.json`.
    enabled: bool,
}

impl Default for InterruptionDetector {
    fn default() -> Self {
        Self::new(DetectorConfig::default())
    }
}

impl InterruptionDetector {
    pub fn new(config: DetectorConfig) -> Self {
        InterruptionDetector {
            config,
            enabled: true,
        }
    }

    /// Create a disabled detector.
    ///
    /// `detect` returns an empty vector and consumes no meaningful memory.
    pub fn disabled() -> Self {
        InterruptionDetector {
            config: DetectorConfig::default(),
            enabled: false,
        }
    }

    /// Online detection: inspect a single completed LLMCall.
    ///
    /// Detection priority (higher = checked first):
    ///   1. AuthError       — 401/403
    ///   2. RateLimit       — 429
    ///   3. NetworkTimeout  — 408/504
    ///   4. ServiceUnavailable — 502/503
    ///   5. ContextOverflow — keywords in error body
    ///   5.5. ResourceExhaustion — quota/billing keywords
    ///   6. SafetyFilter    — finish_reason == "content_filter"
    ///   6.5. StateMachineError — protocol/state keywords
    ///   7. LlmError        — generic HTTP >= 400 fallback
    ///   8. SseTruncated    — SSE stream ended prematurely
    ///   9. TokenLimit      — finish_reason == "length" + ratio
    ///  10. ContextOverflow via finish_reason heuristic
    ///  11. EmptyResponse   — HTTP 200 but no output messages
    ///  12. SlowResponse    — successful but duration > threshold
    pub fn detect(&self, call: &LLMCall) -> Vec<InterruptionEvent> {
        if !self.enabled {
            return Vec::new();
        }

        let mut events = Vec::new();

        let session_id = call.metadata.get("session_id").cloned();
        let trace_id = call.metadata.get("response_id").cloned();
        let conversation_id = call.metadata.get("conversation_id").cloned();
        let call_id = Some(call.call_id.clone());
        let pid = Some(call.pid);
        let agent_name = call.agent_name.clone();

        let status_code: u16 = call
            .metadata
            .get("status_code")
            .and_then(|s| s.parse().ok())
            .unwrap_or(200);

        let is_sse = call
            .metadata
            .get("is_sse")
            .map(|s| s == "true")
            .unwrap_or(false);
        let response_body = call.response.raw_body.as_deref().unwrap_or("");
        let stream_error = if status_code < 400 && is_sse {
            structured_stream_error_text(response_body)
        } else {
            None
        };
        let effective_error = call.error.as_deref().or(stream_error.as_deref());
        let error_text = effective_error.unwrap_or("");
        let structured_error = error_text.to_ascii_lowercase();
        let response_error_body = if status_code >= 400 {
            response_body
        } else {
            ""
        };
        let combined_error = format!("{error_text} {response_error_body}").to_ascii_lowercase();

        let is_context_overflow = combined_error.contains("context_length_exceeded")
            || combined_error.contains("maximum context length")
            || combined_error.contains("context window")
            || combined_error.contains("context_length")
            || combined_error.contains("reduce the length")
            || combined_error.contains("prompt is too long")
            || combined_error.contains("input is too long")
            || combined_error.contains("tokens_limit_reached")
            || combined_error.contains("context limit")
            || combined_error.contains("exceeds the model")
            // HTTP 413 from some gateways
            || status_code == 413;

        // ── 1. AuthError (401/403 / invalid_api_key) ──────────────────────────
        if status_code == 401
            || status_code == 403
            || combined_error.contains("invalid_api_key")
            || combined_error.contains("authentication")
            || combined_error.contains("unauthorized")
            || combined_error.contains("invalid x-api-key")
        {
            let detail = serde_json::json!({
                "model": call.model,
                "status_code": status_code,
                "error": effective_error,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::AuthError,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 2. RateLimit (429 / rate_limit) ────────────────────────────────────
        if status_code == 429
            || combined_error.contains("rate_limit")
            || combined_error.contains("rate limit")
            || combined_error.contains("too many requests")
        {
            let detail = serde_json::json!({
                "model": call.model,
                "status_code": status_code,
                "error": effective_error,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::RateLimit,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 3. NetworkTimeout (408/504 / timeout) ─────────────────────────────
        if status_code == 408
            || status_code == 504
            || structured_error.contains("timeout")
            || structured_error.contains("timed out")
            || structured_error.contains("deadline exceeded")
        {
            let detail = serde_json::json!({
                "model": call.model,
                "status_code": status_code,
                "error": effective_error,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::NetworkTimeout,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 4. ServiceUnavailable (502/503 / overloaded) ──────────────────────
        if status_code == 502
            || status_code == 503
            || combined_error.contains("overloaded")
            || combined_error.contains("service_unavailable")
            || combined_error.contains("server is overloaded")
            || combined_error.contains("model is overloaded")
        {
            let detail = serde_json::json!({
                "model": call.model,
                "status_code": status_code,
                "error": effective_error,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::ServiceUnavailable,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 5. Context overflow ───────────────────────────────────────────────
        // 必须在 LlmError 之前检查，避免 400 + context 关键字被通用规则吞掉
        if is_context_overflow {
            let detail = serde_json::json!({
                "model": call.model,
                "status_code": status_code,
                "error": effective_error,
                "input_tokens": call.token_usage.as_ref().map(|u| u.input_tokens),
            });
            events.push(InterruptionEvent::new(
                InterruptionType::ContextOverflow,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events; // context overflow supersedes all other rules
        }

        // ── 5.5 Resource exhaustion (quota / billing / spending limits) ───
        // Distinct from per-minute rate limiting (Rule 2): these are harder
        // limits that typically require user action (upgrade plan, add credits).
        let is_resource_exhaustion = status_code == 402
            || combined_error.contains("quota")
            || combined_error.contains("billing")
            || combined_error.contains("insufficient")
            || combined_error.contains("usage_limit")
            || combined_error.contains("daily limit")
            || combined_error.contains("monthly limit")
            || combined_error.contains("spending limit")
            || combined_error.contains("credit")
            || combined_error.contains("exhausted")
            || combined_error.contains("allowance");
        if is_resource_exhaustion {
            let detail = serde_json::json!({
                "model": call.model,
                "status_code": status_code,
                "error": effective_error,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::ResourceExhaustion,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 6. SafetyFilter (finish_reason == "content_filter") ───────────────
        // 必须在 LlmError 之前检查：部分厂商对 content_filter 返回 200 + finish_reason
        let finish_reason = call
            .response
            .messages
            .first()
            .and_then(|m| m.finish_reason.as_deref());
        if finish_reason == Some("content_filter") {
            let detail = serde_json::json!({
                "model": call.model,
                "finish_reason": "content_filter",
                "error": effective_error,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::SafetyFilter,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 6.5 State machine / protocol error ──────────────────────────────
        // Agent received a response that violates expected protocol state,
        // e.g. malformed response structure or invalid state transition.
        let is_state_error = combined_error.contains("invalid state")
            || combined_error.contains("protocol error")
            || combined_error.contains("malformed")
            || combined_error.contains("unexpected response")
            || combined_error.contains("invalid transition")
            || combined_error.contains("parse error")
            || combined_error.contains("deserialization");
        if is_state_error {
            let detail = serde_json::json!({
                "model": call.model,
                "status_code": status_code,
                "error": effective_error,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::StateMachineError,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 7. LLM error (non-context HTTP/API errors) ────────────────────────
        // 通用兜底：所有 HTTP >= 400 且未被上述规则匹配的错误
        if status_code >= 400 || call.error.is_some() || stream_error.is_some() {
            let detail = serde_json::json!({
                "status_code": status_code,
                "error": effective_error,
                "model": call.model,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::LlmError,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
            return events;
        }

        // ── 8. SSE truncated ──────────────────────────────────────────────────
        // 严格条件：SSE 流 + 持续时间 >= 阈值 + 无正常终止标志 + 非 token-limit
        // 正常终止标志：finish_reason 为 stop/tool_calls/end_turn/tool_use/stop_sequence/pause_turn
        // token-limit (length/max_tokens) 由 rule 9/10 单独处理
        if is_sse
            && !is_normal_finish(finish_reason)
            && !is_token_limit_finish(finish_reason)
            && call.duration_ns >= self.config.sse_min_duration_ns
        {
            let detail = serde_json::json!({
                "model": call.model,
                "duration_ms": call.duration_ns / 1_000_000,
                "sse_event_count": call.metadata.get("sse_event_count"),
            });
            events.push(InterruptionEvent::new(
                InterruptionType::SseTruncated,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
        }

        // ── 9. Token limit (output capped by max_tokens) ──────────────────────
        if finish_reason == Some("length") {
            if let Some(max_tokens) = call.request.max_tokens {
                if let Some(usage) = &call.token_usage {
                    let ratio = usage.output_tokens as f64 / max_tokens as f64;
                    if ratio >= self.config.token_limit_ratio {
                        let detail = serde_json::json!({
                            "model": call.model,
                            "output_tokens": usage.output_tokens,
                            "max_tokens": max_tokens,
                            "ratio": ratio,
                        });
                        events.push(InterruptionEvent::new(
                            InterruptionType::TokenLimit,
                            session_id.clone(),
                            trace_id.clone(),
                            conversation_id.clone(),
                            call_id.clone(),
                            pid,
                            agent_name.clone(),
                            call.end_timestamp_ns as i64,
                            Some(detail),
                        ));
                    }
                }
            }
        }

        // ── 10. Context overflow via finish_reason (200 response, input overflow)
        // 有些模型在输入超出上下文窗口时仍返回 200 + finish_reason="length"。
        // 通过 input_tokens >> max_tokens 启发式判定（input > max_tokens * 4）
        if finish_reason == Some("length") {
            if let Some(usage) = &call.token_usage {
                if let Some(max_tokens) = call.request.max_tokens {
                    // If input tokens are much larger than the output cap, this
                    // is almost certainly a context-length issue, not output truncation.
                    if usage.input_tokens > max_tokens * 4 {
                        let detail = serde_json::json!({
                            "model": call.model,
                            "input_tokens": usage.input_tokens,
                            "max_tokens": max_tokens,
                            "finish_reason": "length",
                            "note": "input_tokens >> max_tokens suggests context overflow",
                        });
                        events.push(InterruptionEvent::new(
                            InterruptionType::ContextOverflow,
                            session_id.clone(),
                            trace_id.clone(),
                            conversation_id.clone(),
                            call_id.clone(),
                            pid,
                            agent_name.clone(),
                            call.end_timestamp_ns as i64,
                            Some(detail),
                        ));
                    }
                }
            }
        }

        // ── 11. Tool failure from traced tool_result/tool_call_response ─────────
        // Some agents include actual tool execution results in the next LLMCall
        // request history.  Detect failed tool results directly from that trace
        // data even when no standalone ToolUse event is emitted.
        if events.is_empty() {
            if let Some((source, tool_use_id, error)) = traced_tool_failure(call) {
                let itype = if is_unauthorized_error_text(&error) {
                    InterruptionType::UnauthorizedAction
                } else {
                    InterruptionType::ToolFailure
                };
                let detail = serde_json::json!({
                    "model": call.model,
                    "source": source,
                    "tool_use_id": tool_use_id,
                    "error": error,
                });
                events.push(InterruptionEvent::new(
                    itype,
                    session_id.clone(),
                    trace_id.clone(),
                    conversation_id.clone(),
                    call_id.clone(),
                    pid,
                    agent_name.clone(),
                    call.end_timestamp_ns as i64,
                    Some(detail),
                ));
            }
        }

        // ── 12. Slow response (successful call but duration exceeds threshold) ─
        // Catches cases where the LLM eventually succeeded but took unusually
        // long, degrading user experience.  This intentionally runs before
        // EmptyResponse so long-running calls are reported as latency issues.
        if events.is_empty()
            && status_code < 400
            && call.error.is_none()
            && stream_error.is_none()
            && call.duration_ns >= self.config.slow_response_ns_threshold
        {
            let detail = serde_json::json!({
                "model": call.model,
                "duration_ms": call.duration_ns / 1_000_000,
                "threshold_ms": self.config.slow_response_ns_threshold / 1_000_000,
            });
            events.push(InterruptionEvent::new(
                InterruptionType::SlowResponse,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
        }

        // ── 13. Empty response (200 OK but no output messages and no error) ─────
        // Catches cases where the LLM returned an empty body or zero messages
        // without any error signal — the call “succeeded” but produced nothing useful.
        if events.is_empty()
            && status_code < 400
            && call.error.is_none()
            && stream_error.is_none()
            && call.response.messages.is_empty()
        {
            let detail = serde_json::json!({
                "model": call.model,
                "duration_ms": call.duration_ns / 1_000_000,
                "note": "HTTP 200 but no output messages",
            });
            events.push(InterruptionEvent::new(
                InterruptionType::EmptyResponse,
                session_id.clone(),
                trace_id.clone(),
                conversation_id.clone(),
                call_id.clone(),
                pid,
                agent_name.clone(),
                call.end_timestamp_ns as i64,
                Some(detail),
            ));
        }

        events
    }

    /// Inspect a `ToolUse` event and return a `tool_failure` or
    /// `unauthorized_action` interruption if the tool execution failed.
    ///
    /// When the failure error message contains permission-related keywords
    /// (permission denied, sandbox, forbidden, etc.), the event is classified
    /// as `UnauthorizedAction`; otherwise it falls back to `ToolFailure`.
    pub fn detect_tool_use(
        &self,
        tool: &ToolUse,
        session_id: Option<String>,
        conversation_id: Option<String>,
    ) -> Vec<InterruptionEvent> {
        if !self.enabled || tool.success {
            return Vec::new();
        }

        // Classify permission-related tool failures as UnauthorizedAction
        let error_lower = tool.error.as_deref().unwrap_or("").to_ascii_lowercase();
        let is_unauthorized = is_unauthorized_error_text(&error_lower);

        let itype = if is_unauthorized {
            InterruptionType::UnauthorizedAction
        } else {
            InterruptionType::ToolFailure
        };

        let detail = serde_json::json!({
            "tool_name": tool.tool_name,
            "tool_use_id": tool.tool_use_id,
            "error": tool.error,
            "duration_ms": tool.duration_ns.map(|d| d / 1_000_000),
        });
        vec![InterruptionEvent::new(
            itype,
            session_id,
            None,
            conversation_id,
            None,
            Some(tool.pid),
            None,
            tool.timestamp_ns as i64,
            Some(detail),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genai::semantic::*;
    use std::collections::HashMap;

    fn make_base_call() -> LLMCall {
        LLMCall {
            call_id: "call-001".to_string(),
            start_timestamp_ns: 1_000_000_000,
            end_timestamp_ns: 2_000_000_000,
            duration_ns: 1_000_000_000,
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            request: LLMRequest {
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
            },
            response: LLMResponse {
                messages: vec![],
                streamed: false,
                raw_body: None,
            },
            token_usage: None,
            error: None,
            pid: 1234,
            process_name: "agent".to_string(),
            agent_name: Some("TestAgent".to_string()),
            metadata: HashMap::from([("status_code".to_string(), "200".to_string())]),
        }
    }

    #[test]
    fn test_no_interruption_for_normal_call() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![MessagePart::Text {
                content: "Hello".to_string(),
            }],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(events.is_empty());
    }

    #[test]
    fn test_detect_context_overflow_keyword() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("context_length_exceeded".to_string());
        call.metadata
            .insert("status_code".to_string(), "400".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ContextOverflow
        );
    }

    #[test]
    fn test_detect_context_overflow_http_413() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "413".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ContextOverflow
        );
    }

    #[test]
    fn test_detect_context_overflow_response_body() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "400".to_string());
        // 修复后从 call.response.raw_body 读取响应体
        call.response.raw_body = Some("maximum context length is 128k".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ContextOverflow
        );
    }

    #[test]
    fn test_detect_llm_error_http_500() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "500".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::LlmError);
    }

    #[test]
    fn test_detect_llm_error_with_error_field() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("internal_server_error".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::LlmError);
    }

    #[test]
    fn test_detect_sse_truncated() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000; // > 1 second min
        // response.messages is empty
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::SseTruncated);
    }

    #[test]
    fn test_no_sse_truncated_short_duration() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 500_000_000; // < 1 second min
        // Add messages to prevent EmptyResponse from firing
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(events.is_empty());
    }

    #[test]
    fn test_detect_token_limit() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.request.max_tokens = Some(4096);
        call.token_usage = Some(TokenUsage {
            input_tokens: 1000,
            output_tokens: 3900, // 3900/4096 = 0.952 >= 0.95
            total_tokens: 4900,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("length".to_string()),
        }];
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::TokenLimit);
    }

    #[test]
    fn test_no_token_limit_below_ratio() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.request.max_tokens = Some(4096);
        call.token_usage = Some(TokenUsage {
            input_tokens: 1000,
            output_tokens: 2000, // 2000/4096 = 0.488 < 0.95
            total_tokens: 3000,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("length".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(events.is_empty());
    }

    #[test]
    fn test_detect_context_overflow_via_finish_reason() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.request.max_tokens = Some(4096);
        call.token_usage = Some(TokenUsage {
            input_tokens: 20000, // >> 4096 * 4 = 16384
            output_tokens: 100,
            total_tokens: 20100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("length".to_string()),
        }];
        let events = detector.detect(&call);
        // Should have context_overflow (from rule 5)
        assert!(
            events
                .iter()
                .any(|e| e.interruption_type == InterruptionType::ContextOverflow)
        );
    }

    #[test]
    fn test_context_overflow_supersedes_llm_error() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "400".to_string());
        call.error = Some("context_length_exceeded: max 128000 tokens".to_string());
        let events = detector.detect(&call);
        // Should be context_overflow, NOT llm_error
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ContextOverflow
        );
    }

    #[test]
    fn test_event_metadata_fields() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "500".to_string());
        call.metadata
            .insert("session_id".to_string(), "sess-abc".to_string());
        call.metadata
            .insert("response_id".to_string(), "trace-xyz".to_string());
        call.metadata
            .insert("conversation_id".to_string(), "conv-123".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, Some("sess-abc".to_string()));
        assert_eq!(events[0].trace_id, Some("trace-xyz".to_string()));
        assert_eq!(events[0].conversation_id, Some("conv-123".to_string()));
        assert_eq!(events[0].call_id, Some("call-001".to_string()));
        assert_eq!(events[0].pid, Some(1234));
        assert_eq!(events[0].agent_name, Some("TestAgent".to_string()));
    }

    #[test]
    fn test_custom_config() {
        let config = DetectorConfig {
            token_limit_ratio: 0.8,
            sse_min_duration_ns: 500_000_000,
            slow_response_ns_threshold: 120_000_000_000,
        };
        let detector = InterruptionDetector::new(config);
        let mut call = make_base_call();
        call.request.max_tokens = Some(100);
        call.token_usage = Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 85, // 85/100 = 0.85 >= 0.8 (custom ratio)
            total_tokens: 95,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("length".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .any(|e| e.interruption_type == InterruptionType::TokenLimit)
        );
    }

    // ── 新增类型的测试 ──────────────────────────────────────────────────────

    #[test]
    fn test_detect_auth_error_401() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "401".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::AuthError);
    }

    #[test]
    fn test_detect_auth_error_403() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "403".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::AuthError);
    }

    #[test]
    fn test_detect_auth_error_invalid_api_key() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("invalid_api_key".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::AuthError);
    }

    #[test]
    fn test_detect_rate_limit_429() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "429".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::RateLimit);
    }

    #[test]
    fn test_detect_rate_limit_error_keyword() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("rate_limit_exceeded".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::RateLimit);
    }

    #[test]
    fn test_detect_network_timeout_504() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "504".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::NetworkTimeout
        );
    }

    #[test]
    fn test_detect_network_timeout_error_keyword() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("request timeout".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::NetworkTimeout
        );
    }

    #[test]
    fn test_ignores_timeout_text_in_successful_response_body() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.raw_body = Some(
            r#"{"content":"Run journalctl -u app | grep -i \"timeout\" to inspect timeout logs."}"#
                .to_string(),
        );
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];

        let events = detector.detect(&call);

        assert!(events.is_empty());
    }

    #[test]
    fn test_ignores_auth_text_in_successful_response_body() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.raw_body = Some(
            r#"{"content":"Use rejectUnauthorized:false only for local TLS smoke tests."}"#
                .to_string(),
        );
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];

        let events = detector.detect(&call);

        assert!(events.is_empty());
    }

    #[test]
    fn test_ignores_rate_limit_text_in_successful_response_body() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.raw_body = Some(
            r#"{"content":"Document rate limit and too many requests troubleshooting steps."}"#
                .to_string(),
        );
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];

        let events = detector.detect(&call);

        assert!(events.is_empty());
    }

    #[test]
    fn test_ignores_service_unavailable_text_in_successful_response_body() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.raw_body = Some(
            r#"{"content":"Explain service_unavailable and overloaded model symptoms."}"#
                .to_string(),
        );
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];

        let events = detector.detect(&call);

        assert!(events.is_empty());
    }

    #[test]
    fn test_ignores_context_overflow_text_in_successful_response_body() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.raw_body = Some(
            r#"{"content":"Compare context window, input is too long, and prompt is too long errors."}"#
                .to_string(),
        );
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];

        let events = detector.detect(&call);

        assert!(events.is_empty());
    }

    #[test]
    fn test_detects_anthropic_sse_error_event_in_200_stream() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 500_000_000;
        call.response.raw_body = Some(
            "event: error\n\
             data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n"
                .to_string(),
        );

        let events = detector.detect(&call);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ServiceUnavailable
        );
    }

    #[test]
    fn test_detects_structured_sse_error_object_in_200_stream() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 100_000_000;
        call.response.raw_body =
            Some(r#"data: {"error":{"message":"upstream failed before completion"}}"#.to_string());

        let events = detector.detect(&call);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::LlmError);
    }

    #[test]
    fn test_detect_service_unavailable_503() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "503".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ServiceUnavailable
        );
    }

    #[test]
    fn test_detect_service_unavailable_error_keyword() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("model is overloaded".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ServiceUnavailable
        );
    }

    #[test]
    fn test_detect_safety_filter() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("content_filter".to_string()),
        }];
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::SafetyFilter);
    }

    #[test]
    fn test_safety_filter_not_fired_on_normal_stop() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(events.is_empty());
    }

    #[test]
    fn test_sse_truncated_with_normal_finish_not_fired() {
        // SSE 流有正常终止标志（finish_reason=stop）不应被判为截断
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .all(|e| e.interruption_type != InterruptionType::SseTruncated)
        );
    }

    #[test]
    fn test_sse_truncated_with_tool_calls_finish_not_fired() {
        // SSE 流 finish_reason=tool_calls 是正常终止
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("tool_calls".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .all(|e| e.interruption_type != InterruptionType::SseTruncated)
        );
    }

    #[test]
    fn test_sse_tool_use_not_truncated() {
        // SSE + finish_reason="tool_use" → 不产生 SseTruncated
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("tool_use".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .all(|e| e.interruption_type != InterruptionType::SseTruncated),
            "tool_use should not trigger SseTruncated"
        );
    }

    #[test]
    fn test_sse_stop_sequence_not_truncated() {
        // SSE + finish_reason="stop_sequence" → 不产生 SseTruncated
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop_sequence".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .all(|e| e.interruption_type != InterruptionType::SseTruncated),
            "stop_sequence should not trigger SseTruncated"
        );
    }

    #[test]
    fn test_pause_turn_sse_not_reported_as_truncated() {
        // SSE + finish_reason="pause_turn"（Anthropic 长轮次暂停，流是完整的）
        // → 不产生 SseTruncated
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("pause_turn".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .all(|e| e.interruption_type != InterruptionType::SseTruncated),
            "pause_turn should not trigger SseTruncated"
        );
    }

    #[test]
    fn test_sse_unknown_finish_reason_still_truncated() {
        // 守护 #1023 原行为：SSE + 非白名单 finish_reason（None 场景由
        // test_detect_sse_truncated 覆盖，这里覆盖 Some(未知值)）仍报 SseTruncated
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("some_unknown_reason".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .any(|e| e.interruption_type == InterruptionType::SseTruncated),
            "non-whitelisted finish_reason should still trigger SseTruncated"
        );
    }

    #[test]
    fn test_sse_length_not_truncated_but_token_limit_fires() {
        // SSE + finish_reason="length" → 不产生 SseTruncated
        // 但 rule 9 的 TokenLimit 逻辑仍正常触发
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        call.request.max_tokens = Some(4096);
        call.token_usage = Some(TokenUsage {
            input_tokens: 1000,
            output_tokens: 3900, // 3900/4096 = 0.952 >= 0.95
            total_tokens: 4900,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("length".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .all(|e| e.interruption_type != InterruptionType::SseTruncated),
            "length should not trigger SseTruncated (handled by rule 9/10)"
        );
        assert!(
            events
                .iter()
                .any(|e| e.interruption_type == InterruptionType::TokenLimit),
            "length should still trigger TokenLimit via rule 9"
        );
    }

    #[test]
    fn test_sse_none_finish_still_truncated() {
        // SSE + finish_reason=None + duration > sse_min_duration → 仍产生 SseTruncated
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("is_sse".to_string(), "true".to_string());
        call.duration_ns = 2_000_000_000;
        // response.messages is empty → finish_reason = None
        let events = detector.detect(&call);
        assert!(
            events
                .iter()
                .any(|e| e.interruption_type == InterruptionType::SseTruncated),
            "None finish_reason with SSE should still trigger SseTruncated"
        );
    }

    #[test]
    fn test_auth_error_takes_priority_over_llm_error() {
        // 401 应被归类为 AuthError 而非 LlmError
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "401".to_string());
        call.error = Some("unauthorized".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::AuthError);
    }

    #[test]
    fn test_rate_limit_takes_priority_over_llm_error() {
        // 429 应被归类为 RateLimit 而非 LlmError
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "429".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::RateLimit);
    }

    #[test]
    fn test_response_body_bug_fix() {
        // 验证从 call.response.raw_body 读取响应体（非 metadata）
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "400".to_string());
        call.response.raw_body = Some("context_length_exceeded".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ContextOverflow
        );
    }

    #[test]
    fn test_disabled_detector_is_disabled() {
        let detector = InterruptionDetector::disabled();
        assert!(!detector.enabled);
    }

    // ── Rule 11: traced tool_result/tool_call_response ─────────────────────────

    #[test]
    fn test_detect_tool_failure_from_traced_tool_result() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.request.messages = vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("toolu-read-missing".to_string()),
                response: serde_json::json!({
                    "status": "error",
                    "tool": "read",
                    "error": "ENOENT: no such file or directory, access '/tmp/missing'"
                }),
            }],
            name: None,
        }];

        let events = detector.detect(&call);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::ToolFailure);
        let detail: serde_json::Value =
            serde_json::from_str(events[0].detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["tool_use_id"], "toolu-read-missing");
        assert_eq!(detail["source"], "tool_call_response");
    }

    #[test]
    fn test_detect_unauthorized_action_from_traced_tool_result() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.request.messages = vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("toolu-denied".to_string()),
                response: serde_json::json!({
                    "type": "tool_result",
                    "is_error": true,
                    "content": "Permission denied: cannot write to /etc"
                }),
            }],
            name: None,
        }];

        let events = detector.detect(&call);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::UnauthorizedAction
        );
    }

    // ── Rule 11: EmptyResponse ─────────────────────────────────────────────────

    #[test]
    fn test_detect_empty_response() {
        // HTTP 200, no messages, no error → EmptyResponse
        let detector = InterruptionDetector::default();
        let call = make_base_call();
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::EmptyResponse);
    }

    #[test]
    fn test_no_empty_response_when_messages_present() {
        // HTTP 200 with output messages → no EmptyResponse
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![MessagePart::Text {
                content: "ok".to_string(),
            }],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(events.is_empty());
    }

    #[test]
    fn test_no_empty_response_when_error_present() {
        // HTTP 200 but with error field → LlmError, not EmptyResponse
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("some_error".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::LlmError);
    }

    #[test]
    fn test_no_empty_response_when_http_error() {
        // HTTP >= 400, no messages → LlmError, not EmptyResponse
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "500".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::LlmError);
    }

    // ── detect_tool_use ────────────────────────────────────────────────────────

    fn make_base_tool_use() -> ToolUse {
        ToolUse {
            tool_use_id: "tu-001".to_string(),
            timestamp_ns: 1_000_000_000,
            tool_name: "bash".to_string(),
            arguments: serde_json::json!({"cmd": "ls"}),
            result: None,
            duration_ns: Some(500_000_000),
            success: true,
            error: None,
            parent_llm_call_id: Some("call-001".to_string()),
            pid: 1234,
        }
    }

    #[test]
    fn test_detect_tool_use_failure() {
        let detector = InterruptionDetector::default();
        let mut tool = make_base_tool_use();
        tool.success = false;
        tool.error = Some("command exited with code 1".to_string());

        let events = detector.detect_tool_use(
            &tool,
            Some("sess-1".to_string()),
            Some("conv-1".to_string()),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::ToolFailure);
        assert_eq!(events[0].session_id, Some("sess-1".to_string()));
        assert_eq!(events[0].conversation_id, Some("conv-1".to_string()));
        assert_eq!(events[0].pid, Some(1234));
        // Verify detail contains tool info
        let detail: serde_json::Value =
            serde_json::from_str(events[0].detail.as_ref().unwrap()).unwrap();
        assert_eq!(detail["tool_name"], "bash");
        assert_eq!(detail["error"], "command exited with code 1");
    }

    #[test]
    fn test_detect_tool_use_success_no_event() {
        let detector = InterruptionDetector::default();
        let tool = make_base_tool_use();
        let events = detector.detect_tool_use(&tool, None, None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_detect_tool_use_disabled_detector() {
        let detector = InterruptionDetector::disabled();
        let mut tool = make_base_tool_use();
        tool.success = false;
        tool.error = Some("error".to_string());
        let events = detector.detect_tool_use(&tool, None, None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_detect_tool_use_failure_no_error_message() {
        // Tool failed but no error message — still produces event
        let detector = InterruptionDetector::default();
        let mut tool = make_base_tool_use();
        tool.success = false;
        tool.error = None;

        let events = detector.detect_tool_use(&tool, None, Some("conv-2".to_string()));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::ToolFailure);
    }

    // ── Rule 5.5: ResourceExhaustion ─────────────────────────────────────────

    #[test]
    fn test_detect_resource_exhaustion_quota() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("quota exceeded for this month".to_string());
        call.metadata
            .insert("status_code".to_string(), "402".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ResourceExhaustion
        );
    }

    #[test]
    fn test_detect_resource_exhaustion_billing() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("billing limit reached".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ResourceExhaustion
        );
    }

    #[test]
    fn test_detect_resource_exhaustion_400() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "400".to_string());
        call.response.raw_body = Some("insufficient credits to complete this request".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::ResourceExhaustion
        );
    }

    // ── Rule 6.5: StateMachineError ─────────────────────────────────────────

    #[test]
    fn test_detect_state_machine_error_malformed() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.error = Some("malformed response from upstream".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::StateMachineError
        );
    }

    #[test]
    fn test_detect_state_machine_error_protocol() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.metadata
            .insert("status_code".to_string(), "500".to_string());
        call.error = Some("protocol error: unexpected response format".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::StateMachineError
        );
    }

    // ── Rule 12: SlowResponse ────────────────────────────────────────────────

    #[test]
    fn test_detect_slow_response() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.duration_ns = 150_000_000_000; // 150s > 120s threshold
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![MessagePart::Text {
                content: "ok".to_string(),
            }],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::SlowResponse);
    }

    #[test]
    fn test_no_slow_response_below_threshold() {
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.duration_ns = 60_000_000_000; // 60s < 120s threshold
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![MessagePart::Text {
                content: "ok".to_string(),
            }],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert!(events.is_empty());
    }

    #[test]
    fn test_no_slow_response_when_error_present() {
        // Error calls should be classified by error rules, not SlowResponse
        let detector = InterruptionDetector::default();
        let mut call = make_base_call();
        call.duration_ns = 200_000_000_000;
        call.error = Some("some error".to_string());
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::LlmError);
    }

    #[test]
    fn test_detect_slow_response_custom_threshold() {
        let config = DetectorConfig {
            token_limit_ratio: 0.95,
            sse_min_duration_ns: 1_000_000_000,
            slow_response_ns_threshold: 30_000_000_000, // 30s
        };
        let detector = InterruptionDetector::new(config);
        let mut call = make_base_call();
        call.duration_ns = 45_000_000_000; // 45s > 30s custom threshold
        call.response.messages = vec![OutputMessage {
            role: "assistant".to_string(),
            parts: vec![],
            name: None,
            finish_reason: Some("stop".to_string()),
        }];
        let events = detector.detect(&call);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::SlowResponse);
    }

    // ── detect_tool_use: UnauthorizedAction ───────────────────────────────

    #[test]
    fn test_detect_unauthorized_action_permission_denied() {
        let detector = InterruptionDetector::default();
        let mut tool = make_base_tool_use();
        tool.success = false;
        tool.error = Some("Permission denied: cannot write to /etc".to_string());

        let events = detector.detect_tool_use(&tool, None, Some("conv-1".to_string()));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::UnauthorizedAction
        );
    }

    #[test]
    fn test_detect_unauthorized_action_sandbox() {
        let detector = InterruptionDetector::default();
        let mut tool = make_base_tool_use();
        tool.success = false;
        tool.error = Some("sandbox violation: network access denied".to_string());

        let events = detector.detect_tool_use(&tool, None, None);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::UnauthorizedAction
        );
    }

    #[test]
    fn test_detect_unauthorized_action_eacces() {
        let detector = InterruptionDetector::default();
        let mut tool = make_base_tool_use();
        tool.success = false;
        tool.error = Some("EACCES: operation not permitted".to_string());

        let events = detector.detect_tool_use(&tool, None, None);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].interruption_type,
            InterruptionType::UnauthorizedAction
        );
    }

    #[test]
    fn test_tool_failure_not_unauthorized_for_generic_error() {
        // Generic errors should remain ToolFailure, not UnauthorizedAction
        let detector = InterruptionDetector::default();
        let mut tool = make_base_tool_use();
        tool.success = false;
        tool.error = Some("command exited with code 1".to_string());

        let events = detector.detect_tool_use(&tool, None, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].interruption_type, InterruptionType::ToolFailure);
    }
}
