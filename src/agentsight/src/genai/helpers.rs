//! GenAI Builder helper functions
//!
//! Pure helpers for LLM request classification, provider/model extraction,
//! user-query parsing and agent-name resolution. Logic preserved verbatim
//! from the original `builder.rs`; only visibility was widened to
//! `pub(super)` so siblings (`builder` / `call_builder`) can call these.

use super::GenAIBuilder;
use super::semantic::{LLMRequest, MessagePart};
use crate::analyzer::ParsedApiMessage;
use crate::config::default_cmdline_rules;
use crate::discovery::matcher::{CmdlineGlobMatcher, ProcessContext};

/// LLM call classification: Main (normal), Recap (compaction/summary), WebSearch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum CallKind {
    Main,
    Recap,
    WebSearch,
}

impl CallKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallKind::Main => "main",
            CallKind::Recap => "recap",
            CallKind::WebSearch => "web_search",
        }
    }
}

/// Classify an LLM call based on request content (dual-path: system + first-user).
///
/// Conservative: unmatched → Main (zero false positives > recall).
/// Signatures are from real captures (case-sensitive .contains()).
///
/// **Best-effort matching**: patterns are tied to specific agent prompt text
/// (e.g., QwenCode's "Managed memory has TWO directories"). When agents update
/// their prompts, these patterns may need extending. Consider moving to
/// config-driven rules if the list grows beyond a handful of agents.
pub(super) fn classify_call_kind(request: &LLMRequest) -> CallKind {
    // Collect system instructions text
    let system_text: String = request
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .filter_map(|m| m.parts.first())
        .filter_map(|p| match p {
            MessagePart::Text { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // ① Check system_instructions (Cosh patterns)
    if !system_text.is_empty() {
        // Cosh recap/compaction: both markers present
        if system_text.contains("summarizes internal chat history")
            && system_text.contains("<state_snapshot>")
        {
            return CallKind::Recap;
        }
        // Cosh recap/project-summary
        if system_text.contains("specialized context summarizer") {
            return CallKind::Recap;
        }
        // QwenCode memory extraction subagent
        if system_text.contains("managed memory extraction subagent") {
            return CallKind::Recap;
        }
    }

    // ② Check first-user text (Claude Code patterns + Cosh tool-output)
    let first_user_text: Option<&str> = request
        .messages
        .iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| m.parts.first())
        .filter_map(|p| match p {
            MessagePart::Text { content } => Some(content.as_str()),
            _ => None,
        })
        .next();

    if let Some(text) = first_user_text {
        // Cosh tool-output + Claude Code recap (both → Recap)
        if text.starts_with("Summarize the following tool output to be a maximum of")
            || (text
                .contains("Your task is to create a detailed summary of the conversation so far")
                && text.contains("Do NOT call any tools"))
        {
            return CallKind::Recap;
        }
        // Cosh-shell personal analyzer background summary (#2750): fixed
        // prompt from cosh-ng recommendation/personal_analyzer.rs; runs in a
        // setsid child process, must not surface as a user session.
        if text.starts_with("Summarize only grounded work into the supplied JSON schema") {
            return CallKind::Recap;
        }
        // QwenCode memory extraction subagent
        if text.starts_with("Managed memory has TWO directories") {
            return CallKind::Recap;
        }
        // QwenCode suggestion subagent
        if text.starts_with("[SUGGESTION MODE:") {
            return CallKind::Recap;
        }
        // Claude Code web_search
        if text.contains("Perform a web search for the query:") {
            return CallKind::WebSearch;
        }
    }

    // ③ Default: Main (conservative, zero false positives)
    CallKind::Main
}

/// Classify an LLM call from raw JSON strings (pending-write path).
///
/// `system_instructions` is the serialized JSON array of system messages.
/// `first_user_text` is the extracted first user message text.
///
/// This mirrors [`classify_call_kind`] but operates on raw strings from the
/// HTTP body rather than the structured `LLMRequest`, used in
/// `build_pending_from_request` where full semantic parsing has not occurred.
pub(super) fn classify_call_kind_from_raw(
    system_instructions: &Option<String>,
    first_user_text: &str,
) -> &'static str {
    let sys_text = system_instructions
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let c = m.get("content")?;
                    if let Some(s) = c.as_str() {
                        return Some(s.to_string());
                    }
                    if let Some(arr) = c.as_array() {
                        let text: String = arr
                            .iter()
                            .filter_map(|item| {
                                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    item.get("text").and_then(|t| t.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !text.is_empty() {
                            return Some(text);
                        }
                    }
                    None
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    if (sys_text.contains("summarizes internal chat history")
        && sys_text.contains("<state_snapshot>"))
        || sys_text.contains("specialized context summarizer")
        || sys_text.contains("managed memory extraction subagent")
        || first_user_text.starts_with("Summarize the following tool output to be a maximum of")
        || (first_user_text
            .contains("Your task is to create a detailed summary of the conversation so far")
            && first_user_text.contains("Do NOT call any tools"))
        || first_user_text.starts_with("Managed memory has TWO directories")
        || first_user_text.starts_with("[SUGGESTION MODE:")
        // Cosh-shell personal analyzer background summary (#2750)
        || first_user_text.starts_with("Summarize only grounded work into the supplied JSON schema")
    {
        "recap"
    } else if first_user_text.contains("Perform a web search for the query:") {
        "web_search"
    } else {
        "main"
    }
}

/// Trait abstracting over PID → agent_name caches.
///
/// Implemented for both `HashMap` (tests) and `LruCache` (production) so the
/// GenAI builder helpers do not need to know the concrete cache type.
pub trait PidAgentNameCache {
    /// Look up the agent name for a given PID.
    fn get_agent_name(&self, pid: &u32) -> Option<&String>;
}

impl PidAgentNameCache for std::collections::HashMap<u32, String> {
    fn get_agent_name(&self, pid: &u32) -> Option<&String> {
        self.get(pid)
    }
}

impl PidAgentNameCache for lru::LruCache<u32, String> {
    fn get_agent_name(&self, pid: &u32) -> Option<&String> {
        // Use `peek` to avoid mutable access; the LRU order update is not
        // important for agent-name lookup.
        self.peek(pid)
    }
}

impl GenAIBuilder {
    /// Path suffixes of the DashScope/Bailian **native** protocol.
    ///
    /// Full form: `POST https://{WorkspaceId}.{region}.maas.aliyuncs.com
    /// /api/v1/services/aigc/{text,multimodal}-generation/generation`.
    /// Distinct from the OpenAI-compatible mode
    /// (`/compatible-mode/v1/chat/completions`), which already matches the
    /// `/v1/chat/completions` pattern.
    pub(super) const DASHSCOPE_NATIVE_PATHS: [&'static str; 2] = [
        "/aigc/text-generation/generation",
        "/aigc/multimodal-generation/generation",
    ];

    /// Whether the path belongs to the DashScope/Bailian native protocol.
    pub(super) fn is_dashscope_native_path(path: &str) -> bool {
        Self::DASHSCOPE_NATIVE_PATHS
            .iter()
            .any(|p| path.contains(p))
    }

    /// Check if the path indicates an LLM API call
    pub(super) fn is_llm_api_path(&self, path: &str) -> bool {
        path.contains("/v1/chat/completions")
            || path.contains("/v1/completions")
            || path.contains("/v1/messages")
            || path.contains("/v1/responses")
            || path.contains("/chat/completions")
            || path.contains("/completions")
            || path.contains("/api/v1/copilot/generate_copilot")
            || Self::is_dashscope_native_path(path)
    }

    /// Check if request body contains SysOM POP API markers
    /// SysOM uses path "/" with action in body (llmParamString field)
    pub(super) fn is_sysom_pop_request(request_body: &Option<String>) -> bool {
        request_body
            .as_ref()
            .map(|b| b.contains("llmParamString"))
            .unwrap_or(false)
    }

    /// Normalize the messages array from a parsed request body.
    ///
    /// Supports:
    /// - OpenAI chat completions: top-level `"messages"` array.
    /// - OpenAI Responses API (codex 0.137+ via dashscope `/v1/responses`):
    ///   top-level `"input"` array with sibling `"instructions"` string.
    /// - DashScope/Bailian native protocol: top-level `"input"` **object**
    ///   wrapping a `"messages"` array.
    ///
    /// Returns `(messages_vec, instructions_text)` where `instructions_text`
    /// is the system-prompt fallback used when the messages array has no
    /// `role == "system"` entry. It is set for:
    /// - OpenAI Responses API: the top-level `"instructions"` string.
    /// - Anthropic Messages API: the top-level `"system"` field (string or
    ///   array of `{"type":"text","text":"..."}` blocks), since Anthropic
    ///   carries the system prompt outside the messages array.
    ///
    /// The native protocol needs no fallback: its system prompt lives inside
    /// `input.messages`.
    pub(super) fn extract_messages_view(
        body: &serde_json::Value,
    ) -> Option<(Vec<serde_json::Value>, Option<String>)> {
        if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
            let system_text = body.get("system").and_then(Self::extract_system_text);
            return Some((arr.clone(), system_text));
        }
        if let Some(input) = body.get("input") {
            if let Some(arr) = input.as_array() {
                let instructions = body
                    .get("instructions")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                return Some((arr.clone(), instructions));
            }
            if let Some(arr) = input.get("messages").and_then(|m| m.as_array()) {
                return Some((arr.clone(), None));
            }
        }
        None
    }

    /// Extract text from Anthropic's top-level `system` field.
    ///
    /// The field is either a plain string or an array of content blocks
    /// (`{"type":"text","text":"..."}`). Returns `None` when empty so the
    /// caller's "no system role in messages" fallback stays inactive.
    fn extract_system_text(system: &serde_json::Value) -> Option<String> {
        match system {
            serde_json::Value::String(s) => {
                if s.is_empty() {
                    None
                } else {
                    Some(s.clone())
                }
            }
            serde_json::Value::Array(blocks) => {
                let text: String = blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() { None } else { Some(text) }
            }
            _ => None,
        }
    }

    /// Extract human-readable text from a message's `content` field.
    ///
    /// Supports:
    /// - Plain string: `"content": "text"`.
    /// - Array of content blocks with type `text` / `input_text` / `output_text`:
    ///   `"content": [{"type":"input_text","text":"..."}]`.
    pub(super) fn extract_message_text(message: &serde_json::Value) -> Option<String> {
        let c = message.get("content")?;
        if let Some(s) = c.as_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
        if let Some(arr) = c.as_array() {
            let text: String = arr
                .iter()
                .filter_map(|item| {
                    let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if matches!(ty, "text" | "input_text" | "output_text") {
                        item.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return Some(text);
            }
        }
        None
    }

    /// Extract provider from path
    pub(super) fn extract_provider_from_path(&self, path: &str) -> Option<String> {
        if path.contains("anthropic") || path.contains("/v1/messages") {
            Some("anthropic".to_string())
        } else if path.contains("/v1/chat/completions")
            || path.contains("/v1/completions")
            || path.contains("/v1/responses")
        {
            Some("openai".to_string())
        } else if path.contains("/api/v1/copilot/generate_copilot") {
            Some("sysom".to_string())
        } else if Self::is_dashscope_native_path(path) {
            Some("dashscope".to_string())
        } else {
            None
        }
    }

    /// Extract provider from request body (for POP API style requests)
    pub(super) fn extract_provider_from_body(request_body: &Option<String>) -> Option<String> {
        if Self::is_sysom_pop_request(request_body) {
            Some("sysom".to_string())
        } else {
            None
        }
    }

    /// Extract model from parsed message
    pub(super) fn extract_model_from_message(
        &self,
        message: &Option<ParsedApiMessage>,
    ) -> Option<String> {
        match message {
            Some(ParsedApiMessage::OpenAICompletion { request, .. }) => {
                request.as_ref().map(|r| r.model.clone())
            }
            Some(ParsedApiMessage::AnthropicMessage { request, .. }) => {
                request.as_ref().map(|r| r.model.clone())
            }
            Some(ParsedApiMessage::SysomMessage { request, .. }) => {
                request.as_ref().map(|r| r.params.model.clone())
            }
            _ => None,
        }
    }

    /// 从 HTTP request/response body 中直接提取 model 字段
    ///
    /// 优先从 request body 取（用户请求的 model），
    /// 如果没有则从 response body 取（SSE 响应中的 model）
    /// 对于 SysOM 请求，需要从 llmParamString 内嵌 JSON 中提取 model
    pub(super) fn extract_model_from_body(
        request_body: &Option<String>,
        response_body: &Option<String>,
    ) -> Option<String> {
        // 尝试从 request body 获取
        if let Some(body) = request_body {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                // 标准 OpenAI/Anthropic 格式
                if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
                    if !model.is_empty() {
                        return Some(model.to_string());
                    }
                }
                // SysOM 格式：model 嵌套在 llmParamString 中
                if let Some(lps) = v.get("llmParamString").and_then(|v| v.as_str()) {
                    if let Ok(inner) = serde_json::from_str::<serde_json::Value>(lps) {
                        if let Some(model) = inner.get("model").and_then(|m| m.as_str()) {
                            if !model.is_empty() {
                                return Some(model.to_string());
                            }
                        }
                    }
                }
            }
        }
        // 尝试从 response body 获取（SSE 响应是 JSON 数组，取第一个 chunk）
        if let Some(body) = response_body {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                // 非 SSE: 直接是 JSON 对象
                if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
                    if !model.is_empty() {
                        return Some(model.to_string());
                    }
                }
                // SSE: JSON 数组，取第一个 chunk 的 model
                if let Some(arr) = v.as_array() {
                    for chunk in arr {
                        if let Some(model) = chunk.get("model").and_then(|m| m.as_str()) {
                            if !model.is_empty() {
                                return Some(model.to_string());
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// 提取第一条有实际文本内容的 user message 的原始文本
    ///
    /// 仅返回含非空 `Text` 片段的首条 user message，供 `IdResolver`
    /// 生成 session_key 使用。跳过只含 tool_result 等的 user message。
    pub(super) fn extract_first_user_raw(request: &LLMRequest) -> Option<String> {
        request
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .find_map(|m| {
                let text: String = m
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        MessagePart::Text { content } if !content.is_empty() => {
                            Some(content.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() { None } else { Some(text) }
            })
    }

    /// 提取最后一条有实际文本内容的 user message 的原始文本
    ///
    /// 跳过 Anthropic 格式中只包含 tool_result 的 user message
    pub(super) fn extract_last_user_raw(request: &LLMRequest) -> Option<String> {
        request
            .messages
            .iter()
            .rev()
            .filter(|m| m.role == "user")
            .find_map(|m| {
                let text: String = m
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        MessagePart::Text { content } if !content.is_empty() => {
                            Some(content.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() { None } else { Some(text) }
            })
    }

    /// 统计请求中"真正的用户消息"条数：role=user 且包含至少一个非空 Text 部分。
    ///
    /// 仅含 `ToolCallResponse` 部分的 user message（Anthropic 风格的工具返回）不计入。
    /// 同一轮工具调用循环内该值不变（工具结果是 role=tool 或仅含 tool_result 的
    /// role=user），用户发送新消息时必然 +1，用于 conversation bucket key 的
    /// 结构性去重。
    pub(super) fn count_real_user_messages(request: &LLMRequest) -> usize {
        request
            .messages
            .iter()
            .filter(|m| {
                m.role == "user"
                    && m.parts
                        .iter()
                        .any(|p| matches!(p, MessagePart::Text { content } if !content.is_empty()))
            })
            .count()
    }

    /// 提取清理后的 user query（去除 metadata 前缀，用于展示）
    pub(super) fn extract_last_user_query(request: &LLMRequest) -> Option<String> {
        Self::extract_last_user_raw(request).map(|raw| Self::strip_user_query_prefix(&raw))
    }

    /// 去除 user message 中的 metadata 前缀，只保留用户实际输入的文本
    ///
    /// 支持的 Agent prompt 模板格式：
    ///
    /// **cosh-ng**: user_input 标记行
    /// ```text
    /// Handle this natural-language shell prompt request ...
    /// ...
    /// user_input: 查看一下系统状态
    ///
    /// runtime_frame:
    /// ```
    ///
    /// **OpenClaw**: 时间戳方括号
    /// ````text
    /// Sender (untrusted metadata):
    /// ```json
    /// {"label":"...", ...}
    /// ```
    ///
    /// [Tue 2026-03-31 17:19 GMT+8] 用户实际输入
    /// ````
    ///
    /// **QwenCode**: <system-reminder> 标签块
    /// ```text
    /// <system-reminder>...skills...</system-reminder>
    /// <system-reminder>...context...</system-reminder>
    /// 用户实际输入
    /// ```
    /// `pub(crate)` rather than `pub(super)`: the preference rule engine
    /// (`crate::preferences::detector`) reuses this stripper so evidence
    /// excerpts quote the user's actual words, not agent template noise.
    pub(crate) fn strip_user_query_prefix(text: &str) -> String {
        // cosh-ng: find a line starting with "user_input:" (after trimming)
        // and extract its value. This line-by-line approach tolerates template
        // adjustments such as indentation changes, missing leading newlines,
        // or extra whitespace around the marker.
        for line in text.lines() {
            let trimmed = line.trim_start();
            if let Some(value) = trimmed.strip_prefix("user_input:") {
                let extracted = value.trim();
                if !extracted.is_empty() {
                    return extracted.to_string();
                }
            }
        }

        // OpenClaw: 查找最后一个 [timestamp] 模式，取其后的内容
        // 格式: [Day YYYY-MM-DD HH:MM TZ] 或 [Day, DD Mon YYYY HH:MM:SS TZ]
        if let Some(pos) = text.rfind(']') {
            // 确认 ] 前面有对应的 [
            if let Some(bracket_start) = text[..pos].rfind('[') {
                let bracket_content = &text[bracket_start + 1..pos];
                // 简单验证：方括号内包含数字（日期）和冒号（时间）
                if bracket_content.contains(':')
                    && bracket_content.chars().any(|c| c.is_ascii_digit())
                {
                    let after = text[pos + 1..].trim_start();
                    if !after.is_empty() {
                        return after.to_string();
                    }
                }
            }
        }

        // QwenCode: strip <system-reminder>...</system-reminder> blocks
        if text.contains("<system-reminder>") {
            let stripped = Self::strip_system_reminder_tags(text);
            if !stripped.is_empty() {
                return stripped;
            }
        }

        text.to_string()
    }

    /// Remove all `<system-reminder>...</system-reminder>` blocks from text.
    fn strip_system_reminder_tags(text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("<system-reminder>") {
            result.push_str(&rest[..start]);
            if let Some(end_offset) = rest[start..].find("</system-reminder>") {
                rest = &rest[start + end_offset + "</system-reminder>".len()..];
            } else {
                break;
            }
        }
        result.push_str(rest);
        result.trim().to_string()
    }

    /// Match a process context against the configured cmdline rules.
    ///
    /// Returns the agent name of the first matching allow rule, or `None`.
    /// Never returns a domain — the result is always a config-defined name.
    pub(super) fn match_agent_by_ctx(ctx: &ProcessContext) -> Option<String> {
        default_cmdline_rules()
            .iter()
            .filter_map(CmdlineGlobMatcher::from_config)
            .find(|m| m.matches(ctx))
            .map(|m| m.info().name.clone())
    }

    /// Resolve agent name from comm string only (no /proc access).
    /// Used for dead-PID drain where the process is already gone.
    pub(super) fn resolve_agent_name_from_comm(
        comm: &str,
        pid: u32,
        cache: &impl PidAgentNameCache,
    ) -> Option<String> {
        // The cache only ever holds real agent names (config-rule matches); the
        // connection scanner no longer caches its "domain:<host>" fallback.
        if let Some(name) = cache.get_agent_name(&pid) {
            return Some(name.clone());
        }
        let ctx = ProcessContext {
            comm: comm.to_string(),
            cmdline_args: vec![],
            exe_path: String::new(),
        };
        Self::match_agent_by_ctx(&ctx)
    }

    /// Match the process against the agent registry and return the known agent name.
    ///
    /// `pub(crate)` rather than `pub(super)`: the raw HTTPS reporting path in
    /// `unified.rs` resolves the name through this same ladder, so both event kinds
    /// report one value per pid. See the `FfiEventSender::send_https` call site.
    pub(crate) fn resolve_agent_name(
        comm: &str,
        pid: u32,
        cache: &impl PidAgentNameCache,
    ) -> Option<String> {
        // The cache only ever holds real agent names (config-rule matches); the
        // connection scanner no longer caches its "domain:<host>" fallback.
        if let Some(name) = cache.get_agent_name(&pid) {
            return Some(name.clone());
        }
        // Read cmdline from <procfs root>/{pid}/cmdline for accurate agent matching
        let cmdline_args = std::fs::read(crate::utils::procfs::proc_pid_entry(pid, "cmdline"))
            .ok()
            .map(|data| {
                data.split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let exe_path = std::fs::read_link(crate::utils::procfs::proc_pid_entry(pid, "exe"))
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let ctx = ProcessContext {
            comm: comm.to_string(),
            cmdline_args,
            exe_path,
        };
        // Config rule match, else fall back to the *process* comm
        // (`<procfs root>/{pid}/comm`) — never the caller's per-event thread
        // comm, which may be a library thread name such as "HTTP client".
        Self::match_agent_by_ctx(&ctx).or_else(|| crate::discovery::scanner::read_comm(pid))
    }
}

#[cfg(test)]
mod tests {
    use super::super::semantic::InputMessage;
    use super::*;
    use std::collections::HashMap;

    fn make_llm_request(messages: Vec<InputMessage>) -> LLMRequest {
        LLMRequest {
            messages,
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
        }
    }

    #[test]
    fn test_classify_cosh_compaction_recap() {
        let req = make_llm_request(vec![InputMessage {
            role: "system".to_string(),
            parts: vec![MessagePart::Text {
                content: "This summarizes internal chat history with <state_snapshot> data"
                    .to_string(),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Recap);
    }

    #[test]
    fn test_classify_cosh_project_summary_recap() {
        let req = make_llm_request(vec![InputMessage {
            role: "system".to_string(),
            parts: vec![MessagePart::Text {
                content: "You are a specialized context summarizer for project files".to_string(),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Recap);
    }

    #[test]
    fn test_classify_cosh_tool_output_recap() {
        let req = make_llm_request(vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: "Summarize the following tool output to be a maximum of 500 tokens: ..."
                    .to_string(),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Recap);
    }

    #[test]
    fn test_classify_cosh_personal_analyzer_recap() {
        // cosh-shell personal analyzer background summary: fixed prompt from
        // cosh-ng recommendation/personal_analyzer.rs build_fixed_prompt (#2750).
        let req = make_llm_request(vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: "Summarize only grounded work into the supplied JSON schema. List every business entity mentioned by a summary or prompt in that item's entities field.\nSCHEMA:\n{}".to_string(),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Recap);
    }

    #[test]
    fn test_classify_similar_analyzer_wording_stays_main() {
        // Similar summarize wording without the analyzer's fixed prefix must
        // not be misclassified (#2750).
        let req = make_llm_request(vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: "Please summarize only grounded work into a JSON report for me"
                    .to_string(),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Main);
    }

    #[test]
    fn test_classify_claude_web_search() {
        let req = make_llm_request(vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: "Perform a web search for the query: rust async programming".to_string(),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::WebSearch);
    }

    #[test]
    fn test_classify_claude_recap() {
        let req = make_llm_request(vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: "Your task is to create a detailed summary of the conversation so far. Do NOT call any tools during this.".to_string(),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Recap);
    }

    #[test]
    fn test_classify_normal_call_not_misclassified() {
        let req = make_llm_request(vec![
            InputMessage {
                role: "system".to_string(),
                parts: vec![MessagePart::Text {
                    content: "You are Copilot Shell, a helpful assistant.".to_string(),
                }],
                name: None,
            },
            InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "explain this code".to_string(),
                }],
                name: None,
            },
        ]);
        assert_eq!(classify_call_kind(&req), CallKind::Main);
    }

    #[test]
    fn test_classify_empty_request_is_main() {
        let req = make_llm_request(vec![]);
        assert_eq!(classify_call_kind(&req), CallKind::Main);
    }

    #[test]
    fn test_is_llm_api_path() {
        let builder = GenAIBuilder::new();
        assert!(builder.is_llm_api_path("/v1/chat/completions"));
        assert!(builder.is_llm_api_path("/v1/completions"));
        assert!(builder.is_llm_api_path("/v1/messages"));
        assert!(builder.is_llm_api_path("/api/v1/copilot/generate_copilot"));
        assert!(builder.is_llm_api_path("/proxy/v1/chat/completions"));
        assert!(!builder.is_llm_api_path("/api/health"));
        assert!(!builder.is_llm_api_path("/v1/models"));
    }

    /// DashScope/Bailian native protocol endpoints end in `/generation`, which
    /// matched none of the compatible-mode patterns. Without them the whole
    /// non-streaming call was dropped at the `build_llm_call` gate.
    #[test]
    fn test_is_llm_api_path_dashscope_native() {
        let builder = GenAIBuilder::new();
        assert!(builder.is_llm_api_path("/api/v1/services/aigc/text-generation/generation"));
        assert!(builder.is_llm_api_path("/api/v1/services/aigc/multimodal-generation/generation"));
        // Other aigc services (image synthesis, embeddings) stay out.
        assert!(!builder.is_llm_api_path("/api/v1/services/aigc/text2image/image-synthesis"));
    }

    #[test]
    fn test_is_sysom_pop_request() {
        assert!(GenAIBuilder::is_sysom_pop_request(&Some(
            r#"{"llmParamString":"{}"}"#.to_string()
        )));
        assert!(!GenAIBuilder::is_sysom_pop_request(&Some("{}".to_string())));
        assert!(!GenAIBuilder::is_sysom_pop_request(&None));
    }

    #[test]
    fn test_extract_provider_from_path() {
        let builder = GenAIBuilder::new();
        assert_eq!(
            builder.extract_provider_from_path("/v1/chat/completions"),
            Some("openai".to_string())
        );
        assert_eq!(
            builder.extract_provider_from_path("/v1/messages"),
            Some("anthropic".to_string())
        );
        assert_eq!(
            builder.extract_provider_from_path("/api/v1/copilot/generate_copilot"),
            Some("sysom".to_string())
        );
        assert_eq!(builder.extract_provider_from_path("/unknown"), None);
    }

    /// Native protocol calls used to fall through to `"unknown"` (streaming) or
    /// be mislabelled `"anthropic"` via usage-shape detection (token record).
    #[test]
    fn test_extract_provider_from_path_dashscope_native() {
        let builder = GenAIBuilder::new();
        assert_eq!(
            builder.extract_provider_from_path("/api/v1/services/aigc/text-generation/generation"),
            Some("dashscope".to_string())
        );
        assert_eq!(
            builder.extract_provider_from_path(
                "/api/v1/services/aigc/multimodal-generation/generation"
            ),
            Some("dashscope".to_string())
        );
        // Compatible mode keeps reporting openai — it speaks the OpenAI schema.
        assert_eq!(
            builder.extract_provider_from_path("/compatible-mode/v1/chat/completions"),
            Some("openai".to_string())
        );
    }

    #[test]
    fn test_extract_provider_from_body() {
        assert_eq!(
            GenAIBuilder::extract_provider_from_body(&Some(
                r#"{"llmParamString":"{}"} "#.to_string()
            )),
            Some("sysom".to_string())
        );
        assert_eq!(
            GenAIBuilder::extract_provider_from_body(&Some("{}".to_string())),
            None
        );
    }

    #[test]
    fn test_extract_model_from_body_request() {
        let body = Some(r#"{"model": "gpt-4", "messages": []}"#.to_string());
        assert_eq!(
            GenAIBuilder::extract_model_from_body(&body, &None),
            Some("gpt-4".to_string())
        );
    }

    #[test]
    fn test_extract_model_from_body_sysom() {
        let body = Some(r#"{"llmParamString": "{\"model\":\"qwen-max\"}"} "#.to_string());
        assert_eq!(
            GenAIBuilder::extract_model_from_body(&body, &None),
            Some("qwen-max".to_string())
        );
    }

    #[test]
    fn test_extract_model_from_body_response() {
        let resp = Some(r#"{"model": "claude-3"}"#.to_string());
        assert_eq!(
            GenAIBuilder::extract_model_from_body(&None, &resp),
            Some("claude-3".to_string())
        );
    }

    #[test]
    fn test_extract_model_from_body_sse_array() {
        let resp = Some(r#"[{"model": "gpt-4o"}, {"model": "gpt-4o"}]"#.to_string());
        assert_eq!(
            GenAIBuilder::extract_model_from_body(&None, &resp),
            Some("gpt-4o".to_string())
        );
    }

    #[test]
    fn test_extract_model_from_body_none() {
        assert_eq!(GenAIBuilder::extract_model_from_body(&None, &None), None);
    }

    #[test]
    fn test_strip_user_query_prefix_with_timestamp() {
        let text = "Sender (untrusted metadata):\n```json\n{}\n```\n\n[Tue 2026-03-31 17:19 GMT+8] hello world";
        assert_eq!(GenAIBuilder::strip_user_query_prefix(text), "hello world");
    }

    #[test]
    fn test_strip_user_query_prefix_cosh_ng_user_input() {
        let text = "Handle this natural-language shell prompt request for a Shell-first assistant.\n\
                     Decide based on user intent:\n\
                     - If the user wants to DO something...\n\n\
                     user_input: 查看一下系统状态\n\n\
                     runtime_frame:\n\
                     cwd: ~\n\n\
                     cosh-shell Agent contract:";
        assert_eq!(
            GenAIBuilder::strip_user_query_prefix(text),
            "查看一下系统状态"
        );
    }

    #[test]
    fn test_strip_user_query_prefix_cosh_ng_indented() {
        // Template with indentation before user_input: marker
        let text = "  Some preamble text\n  user_input: 检查磁盘空间\n\nruntime_frame:";
        assert_eq!(GenAIBuilder::strip_user_query_prefix(text), "检查磁盘空间");
    }

    #[test]
    fn test_strip_user_query_prefix_no_timestamp() {
        let text = "plain user input";
        assert_eq!(
            GenAIBuilder::strip_user_query_prefix(text),
            "plain user input"
        );
    }

    #[test]
    fn test_strip_user_query_prefix_bracket_no_datetime() {
        let text = "[not a timestamp] content";
        // No ':' and digit in bracket content -> returns original
        assert_eq!(
            GenAIBuilder::strip_user_query_prefix(text),
            "[not a timestamp] content"
        );
    }

    #[test]
    fn test_strip_user_query_prefix_qwencode_system_reminder() {
        let text = "<system-reminder>\nskills list here\n</system-reminder>\
                     <system-reminder>\nfolder structure\n</system-reminder>\
                     1+1等于几";
        assert_eq!(GenAIBuilder::strip_user_query_prefix(text), "1+1等于几");
    }

    #[test]
    fn test_strip_user_query_prefix_qwencode_single_block() {
        let text = "<system-reminder>context</system-reminder>hello world";
        assert_eq!(GenAIBuilder::strip_user_query_prefix(text), "hello world");
    }

    #[test]
    fn test_strip_user_query_prefix_qwencode_unclosed_tag() {
        // Unclosed tag — should still strip what it can
        let text =
            "<system-reminder>some context</system-reminder><system-reminder>no end tag user input";
        assert_eq!(
            GenAIBuilder::strip_user_query_prefix(text),
            "<system-reminder>no end tag user input"
        );
    }

    #[test]
    fn test_strip_user_query_prefix_qwencode_only_unclosed_tag() {
        // No closing tag at all — nothing to strip, returns original trimmed
        let text = "<system-reminder>some context without end tag and user input";
        assert_eq!(
            GenAIBuilder::strip_user_query_prefix(text),
            "<system-reminder>some context without end tag and user input"
        );
    }

    #[test]
    fn test_extract_last_user_query() {
        let req = LLMRequest {
            messages: vec![
                InputMessage {
                    role: "system".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "sys".to_string(),
                    }],
                    name: None,
                },
                InputMessage {
                    role: "user".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "[Mon 2026-01-01 10:00 GMT+8] hi".to_string(),
                    }],
                    name: None,
                },
            ],
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
        assert_eq!(
            GenAIBuilder::extract_last_user_query(&req),
            Some("hi".to_string())
        );
    }

    #[test]
    fn test_count_real_user_messages_empty() {
        let req = LLMRequest {
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
        assert_eq!(GenAIBuilder::count_real_user_messages(&req), 0);
    }

    #[test]
    fn test_count_real_user_messages_single_user() {
        let req = LLMRequest {
            messages: vec![InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "hello".to_string(),
                }],
                name: None,
            }],
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
        assert_eq!(GenAIBuilder::count_real_user_messages(&req), 1);
    }

    #[test]
    fn test_count_real_user_messages_tool_result_only() {
        let req = LLMRequest {
            messages: vec![InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::ToolCallResponse {
                    id: Some("tool-1".to_string()),
                    response: serde_json::json!({"result": "ok"}),
                }],
                name: None,
            }],
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
        assert_eq!(
            GenAIBuilder::count_real_user_messages(&req),
            0,
            "tool-result-only user messages should not count"
        );
    }

    #[test]
    fn test_count_real_user_messages_mixed() {
        let req = LLMRequest {
            messages: vec![
                InputMessage {
                    role: "system".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "sys".to_string(),
                    }],
                    name: None,
                },
                InputMessage {
                    role: "user".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "first question".to_string(),
                    }],
                    name: None,
                },
                InputMessage {
                    role: "assistant".to_string(),
                    parts: vec![MessagePart::ToolCall {
                        id: Some("tc-1".to_string()),
                        name: "search".to_string(),
                        arguments: None,
                    }],
                    name: None,
                },
                InputMessage {
                    role: "user".to_string(),
                    parts: vec![MessagePart::ToolCallResponse {
                        id: Some("tc-1".to_string()),
                        response: serde_json::json!({"data": "result"}),
                    }],
                    name: None,
                },
                InputMessage {
                    role: "user".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "second question".to_string(),
                    }],
                    name: None,
                },
            ],
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
        assert_eq!(
            GenAIBuilder::count_real_user_messages(&req),
            2,
            "2 text-bearing user msgs, 1 tool-result-only, should count 2"
        );
    }

    #[test]
    fn test_resolve_agent_name_from_comm_with_cache() {
        let mut cache = HashMap::new();
        cache.insert(42u32, "CachedAgent".to_string());
        let result = GenAIBuilder::resolve_agent_name_from_comm("unknown", 42, &cache);
        assert_eq!(result, Some("CachedAgent".to_string()));
    }

    #[test]
    fn test_resolve_agent_name_from_comm_no_match() {
        let cache = HashMap::new();
        let result = GenAIBuilder::resolve_agent_name_from_comm("random_process", 99, &cache);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_agent_name_falls_back_to_process_comm() {
        // Empty cache + a comm that matches no rule must NOT return None (which
        // would let the caller use the per-event thread comm, e.g. "HTTP
        // client"). Instead it reads the live process comm from /proc/<pid>/comm.
        let cache = HashMap::new();
        let result =
            GenAIBuilder::resolve_agent_name("random_thread_name", std::process::id(), &cache);
        assert!(result.is_some());
        assert!(!result.unwrap().starts_with("domain:"));
    }

    #[test]
    fn test_resolve_agent_name_prefers_cached_real_name() {
        // A cached real agent name (config-rule match) is authoritative and
        // returned as-is; the cache never holds a "domain:<host>" value.
        let mut cache = HashMap::new();
        cache.insert(std::process::id(), "Claude".to_string());
        let result = GenAIBuilder::resolve_agent_name("x", std::process::id(), &cache);
        assert_eq!(result, Some("Claude".to_string()));
    }

    #[test]
    fn test_match_agent_by_ctx_matches_rule() {
        // A cmdline whose argv[0] matches the default `claude*` rule resolves to
        // the configured agent name.
        let ctx = crate::discovery::matcher::ProcessContext {
            comm: "claude".to_string(),
            cmdline_args: vec!["claude".to_string()],
            exe_path: String::new(),
        };
        assert_eq!(
            GenAIBuilder::match_agent_by_ctx(&ctx),
            Some("Claude".to_string())
        );
    }

    #[test]
    fn test_match_agent_by_ctx_no_match_returns_none() {
        let ctx = crate::discovery::matcher::ProcessContext {
            comm: "x".to_string(),
            cmdline_args: vec!["some-random-binary".to_string()],
            exe_path: String::new(),
        };
        assert_eq!(GenAIBuilder::match_agent_by_ctx(&ctx), None);
    }

    // ─── classify_call_kind_from_raw tests ─────────────────────────────────────

    use super::classify_call_kind_from_raw;

    #[test]
    fn test_raw_classify_recap_cosh_compaction() {
        // system_instructions contains both markers
        let sys = Some(
            serde_json::to_string(&serde_json::json!([
                {"content": "This summarizes internal chat history and <state_snapshot> data"}
            ]))
            .unwrap(),
        );
        assert_eq!(classify_call_kind_from_raw(&sys, ""), "recap");
    }

    #[test]
    fn test_raw_classify_recap_specialized_summarizer() {
        let sys = Some(
            serde_json::to_string(&serde_json::json!([
                {"content": "You are a specialized context summarizer"}
            ]))
            .unwrap(),
        );
        assert_eq!(classify_call_kind_from_raw(&sys, ""), "recap");
    }

    #[test]
    fn test_raw_classify_recap_tool_output() {
        let first_user = "Summarize the following tool output to be a maximum of 500 tokens: ...";
        assert_eq!(classify_call_kind_from_raw(&None, first_user), "recap");
    }

    #[test]
    fn test_raw_classify_recap_claude_code() {
        let first_user = "Your task is to create a detailed summary of the conversation so far. Please Do NOT call any tools in this turn.";
        assert_eq!(classify_call_kind_from_raw(&None, first_user), "recap");
    }

    #[test]
    fn test_raw_classify_recap_cosh_personal_analyzer() {
        let first_user = "Summarize only grounded work into the supplied JSON schema. List every business entity mentioned by a summary or prompt in that item's entities field.\nSCHEMA:\n{}";
        assert_eq!(classify_call_kind_from_raw(&None, first_user), "recap");
    }

    #[test]
    fn test_raw_classify_similar_analyzer_wording_stays_main() {
        let first_user = "Please summarize only grounded work into a JSON report for me";
        assert_eq!(classify_call_kind_from_raw(&None, first_user), "main");
    }

    #[test]
    fn test_raw_classify_web_search() {
        let first_user = "Perform a web search for the query: rust async";
        assert_eq!(classify_call_kind_from_raw(&None, first_user), "web_search");
    }

    #[test]
    fn test_raw_classify_main_default() {
        assert_eq!(
            classify_call_kind_from_raw(&None, "explain this code"),
            "main"
        );
    }

    #[test]
    fn test_raw_classify_main_no_sys_instructions() {
        assert_eq!(classify_call_kind_from_raw(&None, ""), "main");
    }

    #[test]
    fn test_raw_classify_content_array_format() {
        // system_instructions with content as array of {type, text} objects
        let sys = Some(
            serde_json::to_string(&serde_json::json!([
                {"content": [{"type": "text", "text": "This summarizes internal chat history"}, {"type": "text", "text": "and <state_snapshot> data"}]}
            ]))
            .unwrap(),
        );
        assert_eq!(classify_call_kind_from_raw(&sys, ""), "recap");
    }

    #[test]
    fn test_raw_classify_invalid_json_sys_instructions() {
        // Invalid JSON in system_instructions → falls through to first_user_text
        let sys = Some("not valid json".to_string());
        assert_eq!(classify_call_kind_from_raw(&sys, ""), "main");
    }

    #[test]
    fn test_raw_classify_partial_recap_markers_not_enough() {
        // Only one of the two markers → not recap
        let sys = Some(
            serde_json::to_string(&serde_json::json!([
                {"content": "This summarizes internal chat history without snapshot"}
            ]))
            .unwrap(),
        );
        assert_eq!(classify_call_kind_from_raw(&sys, ""), "main");
    }

    #[test]
    fn test_call_kind_as_str() {
        assert_eq!(CallKind::Main.as_str(), "main");
        assert_eq!(CallKind::Recap.as_str(), "recap");
        assert_eq!(CallKind::WebSearch.as_str(), "web_search");
    }

    #[test]
    fn test_classify_with_non_text_parts_system() {
        // System message with only a ToolCall part → no text extracted → Main
        let req = make_llm_request(vec![InputMessage {
            role: "system".to_string(),
            parts: vec![MessagePart::ToolCall {
                id: None,
                name: "read_file".to_string(),
                arguments: None,
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Main);
    }

    #[test]
    fn test_classify_with_non_text_parts_user() {
        // User message with ToolCallResponse part → first_user_text is None → Main
        let req = make_llm_request(vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("tc-1".to_string()),
                response: serde_json::json!({"result": "ok"}),
            }],
            name: None,
        }]);
        assert_eq!(classify_call_kind(&req), CallKind::Main);
    }

    #[test]
    fn test_extract_model_from_message() {
        let builder = GenAIBuilder::new();
        let msg = Some(ParsedApiMessage::OpenAICompletion {
            request: Some(crate::analyzer::message::types::OpenAIRequest {
                model: "gpt-4-turbo".to_string(),
                messages: vec![],
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
            }),
            response: None,
        });
        assert_eq!(
            builder.extract_model_from_message(&msg),
            Some("gpt-4-turbo".to_string())
        );
        assert_eq!(builder.extract_model_from_message(&None), None);
    }

    #[test]
    fn test_extract_messages_view_chat_completions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"}
            ]
        });
        let (msgs, instructions) = GenAIBuilder::extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(instructions.is_none());
    }

    #[test]
    fn test_extract_messages_view_responses_api() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": [{"role": "user", "content": "hi"}],
            "instructions": "sys prompt"
        });
        let (msgs, instructions) = GenAIBuilder::extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(instructions.as_deref(), Some("sys prompt"));
    }

    #[test]
    fn test_extract_messages_view_none() {
        let body = serde_json::json!({"model": "gpt-4"});
        assert!(GenAIBuilder::extract_messages_view(&body).is_none());
    }

    #[test]
    fn test_extract_messages_view_responses_api_without_instructions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": [{"role": "user", "content": "hi"}]
        });
        let (msgs, instructions) = GenAIBuilder::extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(instructions.is_none());
    }

    #[test]
    fn test_extract_system_text_string() {
        let system = serde_json::json!("You are helpful");
        assert_eq!(
            GenAIBuilder::extract_system_text(&system),
            Some("You are helpful".to_string())
        );
    }

    #[test]
    fn test_extract_system_text_empty_string() {
        let system = serde_json::json!("");
        assert_eq!(GenAIBuilder::extract_system_text(&system), None);
    }

    #[test]
    fn test_extract_system_text_array() {
        let system = serde_json::json!([
            {"type": "text", "text": "Part 1"},
            {"type": "text", "text": "Part 2"}
        ]);
        assert_eq!(
            GenAIBuilder::extract_system_text(&system),
            Some("Part 1\nPart 2".to_string())
        );
    }

    #[test]
    fn test_extract_system_text_empty_array() {
        let system = serde_json::json!([]);
        assert_eq!(GenAIBuilder::extract_system_text(&system), None);
    }

    #[test]
    fn test_extract_system_text_non_text() {
        assert_eq!(
            GenAIBuilder::extract_system_text(&serde_json::json!(123)),
            None
        );
        assert_eq!(
            GenAIBuilder::extract_system_text(&serde_json::Value::Null),
            None
        );
    }

    /// DashScope/Bailian native protocol wraps the messages array inside an
    /// `input` **object**, unlike the Responses API where `input` is an array.
    #[test]
    fn test_extract_messages_view_dashscope_native_input_object() {
        let body = serde_json::json!({
            "model": "qwen-plus",
            "input": {
                "messages": [
                    {"role": "system", "content": "sys"},
                    {"role": "user", "content": "hi"}
                ]
            },
            "parameters": {"result_format": "message"}
        });
        let (msgs, instructions) = GenAIBuilder::extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].get("role").and_then(|r| r.as_str()), Some("system"));
        // Native protocol carries the system prompt inside the messages array,
        // so no top-level instructions fallback is needed.
        assert!(instructions.is_none());
    }

    /// An `input` object without a `messages` array carries no conversation.
    #[test]
    fn test_extract_messages_view_dashscope_native_input_object_without_messages() {
        let body = serde_json::json!({
            "model": "qwen-plus",
            "input": {"prompt": "hi"}
        });
        assert!(GenAIBuilder::extract_messages_view(&body).is_none());
    }

    #[test]
    fn test_extract_messages_view_anthropic_system() {
        let body = serde_json::json!({
            "model": "claude-3",
            "system": "You are helpful",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let (msgs, instructions) = GenAIBuilder::extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(instructions.as_deref(), Some("You are helpful"));
    }

    #[test]
    fn test_extract_messages_view_anthropic_system_array() {
        let body = serde_json::json!({
            "model": "claude-3",
            "system": [{"type": "text", "text": "sys prompt"}],
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let (msgs, instructions) = GenAIBuilder::extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(instructions.as_deref(), Some("sys prompt"));
    }

    #[test]
    fn test_extract_message_text_string() {
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        assert_eq!(
            GenAIBuilder::extract_message_text(&msg),
            Some("hello".to_string())
        );
    }

    #[test]
    fn test_extract_message_text_array() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "input_text", "text": "hello"},
                {"type": "output_text", "text": "world"},
                {"type": "image", "text": "ignored"}
            ]
        });
        assert_eq!(
            GenAIBuilder::extract_message_text(&msg),
            Some("hello\nworld".to_string())
        );
    }

    #[test]
    fn test_extract_message_text_empty() {
        let msg = serde_json::json!({"role": "user", "content": ""});
        assert_eq!(GenAIBuilder::extract_message_text(&msg), None);
    }

    #[test]
    fn test_extract_message_text_array_non_text_only() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "image", "text": "ignored"},
                {"type": "image_url", "image_url": {"url": "http://example.com"}}
            ]
        });
        assert_eq!(GenAIBuilder::extract_message_text(&msg), None);
    }

    #[test]
    fn test_pid_agent_name_cache_hashmap() {
        let mut cache = std::collections::HashMap::new();
        cache.insert(123u32, "TestAgent".to_string());
        assert_eq!(cache.get_agent_name(&123), Some(&"TestAgent".to_string()));
        assert_eq!(cache.get_agent_name(&999), None);
    }

    #[test]
    fn test_pid_agent_name_cache_lru() {
        let mut cache = lru::LruCache::new(std::num::NonZeroUsize::new(2).unwrap());
        cache.put(1, "Agent1".to_string());
        cache.put(2, "Agent2".to_string());
        assert_eq!(cache.get_agent_name(&1), Some(&"Agent1".to_string()));
        assert_eq!(cache.get_agent_name(&2), Some(&"Agent2".to_string()));
        assert_eq!(cache.get_agent_name(&3), None);
    }
}
