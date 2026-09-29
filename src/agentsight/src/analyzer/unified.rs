//! Unified Analyzer - high-level entry point for analysis
//!
//! This module provides a unified interface for analyzing parsed and aggregated data.
//! It combines AuditAnalyzer, TokenParser, and MessageParser into a single entry point.
//!
//! # Example
//! ```rust,ignore
//! use agentsight::analyzer::Analyzer;
//! use agentsight::aggregator::AggregatedResult;
//!
//! let analyzer = Analyzer::new();
//!
//! // Analyze aggregated result
//! for result in analyzer.analyze_aggregated(&aggregated_result) {
//!     match result {
//!         AnalysisResult::Audit(record) => { /* handle audit record */ }
//!         AnalysisResult::Token(record) => { /* handle token record */ }
//!         AnalysisResult::Message(msg) => { /* handle parsed API message */ }
//!     }
//! }
//! ```

use crate::aggregator::AggregatedResult;
use crate::analyzer::token::extract_response_content;
use crate::parser::sse::{ParsedSseEvent, SSEParser};
use crate::tokenizer::LlmTokenizer;
use crate::tokenizer::get_global_tokenizer;

use super::result::{MessageTokenCount, OutputTokenCount, TokenConsumptionBreakdown};
use super::{
    AnalysisResult, AuditAnalyzer, HttpRecord, LLMProvider, MessageParser, ParsedApiMessage,
    TokenParser, TokenRecord, TokenUsage,
};

/// Token count result for request messages
#[derive(Debug, Clone)]
pub struct RequestTokenCount {
    /// Total input tokens
    pub total_tokens: usize,
    /// Token count by role (system, user, assistant, tool)
    pub by_role: std::collections::HashMap<String, usize>,
    /// Per-message token counts with role information
    pub per_message: Vec<MessageTokenCount>,
    /// Tool definitions token count
    pub tools_tokens: usize,
    /// System prompt token count
    pub system_prompt_tokens: usize,
}

/// Token count result for response content
#[derive(Debug, Clone)]
pub struct ResponseTokenCount {
    /// Total output tokens
    pub total_tokens: usize,
    /// Output token count by content type (text, reasoning, tool_calls)
    pub by_type: std::collections::HashMap<String, usize>,
    /// Per-content-block token counts
    pub per_block: Vec<OutputTokenCount>,
}

/// Count tokens in a request JSON using the provided tokenizer and chat template
///
/// # Arguments
/// * `request_json` - The request JSON (OpenAI format with "messages" array)
/// * `tokenizer` - The tokenizer to use for counting
/// * `chat_template` - The chat template for formatting messages
///
/// # Returns
/// `Some(RequestTokenCount)` if successful, `None` if the request has no messages
///
/// # Example
/// ```rust,ignore
/// let request = serde_json::json!({
///     "model": "qwen3.5-plus",
///     "messages": [
///         {"role": "system", "content": "You are helpful"},
///         {"role": "user", "content": "Hello"}
///     ]
/// });
/// let count = count_request_tokens(&request, tokenizer.as_ref(), template.as_ref())?;
/// println!("Total: {} tokens", count.total_tokens);
/// ```
pub fn count_request_tokens(
    request_json: &serde_json::Value,
    tokenizer: &LlmTokenizer,
    chat_template: &LlmTokenizer,
) -> Option<RequestTokenCount> {
    // Extract messages
    let messages = request_json.get("messages").and_then(|m| m.as_array())?;

    if messages.is_empty() {
        return None;
    }

    let mut by_role: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut per_message: Vec<MessageTokenCount> = Vec::new();

    // Prepare messages for apply_chat_template
    let template_messages: Vec<serde_json::Value> = messages.to_vec();

    // Extract tools JSON array for passing to template
    let tools_json: Option<Vec<serde_json::Value>> = request_json
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| arr.to_vec());

    // Count tools tokens separately (for informational breakdown)
    let mut tools_tokens: usize = tools_json
        .as_ref()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| serde_json::to_string(t).ok())
                .filter_map(|s| tokenizer.count(&s).ok())
                .sum()
        })
        .unwrap_or(0);

    // Use apply_chat_template_with_tools to format all messages WITH tools
    // This ensures the tools instruction text is included in the total count
    let tools_slice = tools_json.as_deref();
    let total_tokens =
        match chat_template.apply_chat_template_with_tools(&template_messages, tools_slice, true) {
            Ok(formatted) => tokenizer.count(&formatted).unwrap_or(0),
            Err(e) => {
                log::warn!("Failed to apply chat template with tools: {e}");
                // Fallback: count raw content + tools separately
                0
            }
        };

    // Count per-message tokens: first calculate raw token counts, then distribute total_tokens by percentage
    // Step 1: Calculate raw token count for each message
    let mut raw_per_message: Vec<(String, usize)> = Vec::new();
    for msg in messages.iter() {
        // Count tokens by serializing entire message (includes content, reasoning_content, etc.)
        let mut tokens = serde_json::to_string(msg)
            .ok()
            .and_then(|s| tokenizer.count(&s).ok())
            .unwrap_or(0);
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown")
            .to_string();
        if role == "tool" {
            tokens += tools_tokens;
            tools_tokens = 0;
        }
        raw_per_message.push((role, tokens));
    }

    // Step 2: Calculate total raw tokens and distribute total_tokens by percentage
    let raw_total: usize = raw_per_message.iter().map(|(_, t)| *t).sum();

    if raw_total > 0 {
        // Distribute total_tokens proportionally based on raw token percentages
        for (role, raw_tokens) in raw_per_message.iter() {
            let actual_tokens =
                ((*raw_tokens as f64 / raw_total as f64) * total_tokens as f64).round() as usize;
            *by_role.entry(role.clone()).or_insert(0) += actual_tokens;
            per_message.push(MessageTokenCount {
                role: role.clone(),
                tokens: actual_tokens,
            });
        }
    } else {
        // Fallback: if no raw tokens, just use zeros
        for (role, _) in raw_per_message.iter() {
            per_message.push(MessageTokenCount {
                role: role.clone(),
                tokens: 0,
            });
        }
    }
    let system_prompt_tokens = by_role.get("system").cloned().unwrap_or(0);
    let tools_tokens = by_role.get("tool").cloned().unwrap_or(0);
    Some(RequestTokenCount {
        total_tokens,
        by_role,
        per_message,
        tools_tokens,
        system_prompt_tokens,
    })
}

/// Count tokens in response SSE chunks using the provided tokenizer
///
/// This function applies the Qwen ChatML template format to the response content
/// for accurate token counting, including special tokens like:
/// - `<|im_start|>assistant\n` - response start
/// - `<think>\n...\n</think>\n\n` - reasoning content wrapper  
/// - `<|im_end|>` - response end
/// - Tool call XML format markers
///
/// # Arguments
/// * `response_jsons` - Array of SSE response chunks (each line parsed as JSON)
/// * `tokenizer` - The tokenizer to use for counting
///
/// # Returns
/// `Some(ResponseTokenCount)` if successful, `None` if no content found
///
/// # Example
/// ```rust,ignore
/// // Read response file line by line and parse each line as JSON
/// let lines: Vec<serde_json::Value> = content.lines()
///     .filter_map(|line| serde_json::from_str(line).ok())
///     .collect();
/// let count = count_response_tokens(&lines, tokenizer.as_ref())?;
/// println!("Total: {} tokens", count.total_tokens);
/// ```
pub fn count_response_tokens(
    response_jsons: &[serde_json::Value],
    tokenizer: &LlmTokenizer,
) -> Option<ResponseTokenCount> {
    let mut by_type: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut per_block: Vec<OutputTokenCount> = Vec::new();

    // Accumulate content from all SSE chunks
    let mut all_content = String::new();
    let mut all_reasoning = String::new();
    let mut all_tool_calls = Vec::new();

    for chunk in response_jsons {
        if let Some((content, reasoning, tool_calls)) = extract_response_content(Some(chunk)) {
            if !content.is_empty() {
                all_content.push_str(&content);
            }
            if let Some(r) = reasoning {
                if !r.is_empty() {
                    all_reasoning.push_str(&r);
                }
            }
            for tc in tool_calls {
                if !tc.is_empty() {
                    all_tool_calls.push(tc);
                }
            }
        }
    }

    let mut has_content = false;

    // NOTE: API output token count includes:
    // - Model-generated text markers like <think>...</think> (these ARE counted)
    // - NOT special control tokens like <|im_start|>, <|im_end|> (these are NOT counted)

    // Add reasoning content with <think> wrapper (model generates these markers)
    if !all_reasoning.is_empty() {
        has_content = true;

        // Format: <think>\n{reasoning}\n</think>\n\n
        let reasoning_with_tags = format!("<think>\n{all_reasoning}\n</think>\n\n");
        let tokens = tokenizer
            .count(&reasoning_with_tags)
            .unwrap_or(all_reasoning.len() / 4);
        *by_type.entry("reasoning".to_string()).or_insert(0) += tokens;
        per_block.push(OutputTokenCount {
            content_type: "reasoning".to_string(),
            tokens,
        });
    }

    // Add text content
    if !all_content.is_empty() {
        has_content = true;

        let tokens = tokenizer
            .count(&all_content)
            .unwrap_or(all_content.len() / 4);
        *by_type.entry("text".to_string()).or_insert(0) += tokens;
        per_block.push(OutputTokenCount {
            content_type: "text".to_string(),
            tokens,
        });
    }

    // Add tool calls with Qwen template format
    // According to tokenizer_config.json, <tool_call>, <function=...>, <parameter=...> are NOT special tokens,
    // so they ARE counted as output tokens by the API
    //
    // NOTE: For SSE streaming, tool_calls come in chunks:
    // - First chunk: "exec: " (function name + colon)
    // - Following chunks: ": {...}" (colon + arguments fragments)
    // We need to aggregate all chunks first to get the complete tool call
    if !all_tool_calls.is_empty() {
        // Aggregate all chunks: first chunk has "name: ", rest have ": fragment"
        let mut aggregated = String::new();
        for tc in &all_tool_calls {
            if aggregated.is_empty() {
                // First chunk contains "name: " or just "name:"
                aggregated.push_str(tc);
            } else {
                // Subsequent chunks start with ": ", skip the leading ": "
                let fragment = tc.strip_prefix(": ").unwrap_or(tc);
                aggregated.push_str(fragment);
            }
        }

        // Now parse the aggregated "name: arguments" string
        let (name, arguments) = if let Some(pos) = aggregated.find(": ") {
            (&aggregated[..pos], &aggregated[pos + 2..])
        } else if let Some(pos) = aggregated.find(':') {
            // Handle case where there's no space after colon
            (&aggregated[..pos], &aggregated[pos + 1..])
        } else {
            ("", aggregated.as_str())
        };

        // Build Qwen tool_call template format:
        // <tool_call>
        // <function={name}>
        // <parameter={arg_name}>
        // {arg_value}
        // </parameter>
        // </function>
        // </tool_call>
        let mut tool_call_str = String::new();
        tool_call_str.push_str("<tool_call>\n<function=");
        tool_call_str.push_str(name);
        tool_call_str.push_str(">\n");

        // Parse arguments JSON and format each parameter
        if let Ok(args_json) = serde_json::from_str::<serde_json::Value>(arguments) {
            if let Some(obj) = args_json.as_object() {
                for (arg_name, arg_value) in obj {
                    tool_call_str.push_str("<parameter=");
                    tool_call_str.push_str(arg_name);
                    tool_call_str.push_str(">\n");
                    // Format value: if string use as-is, otherwise use JSON
                    let value_str = if let Some(s) = arg_value.as_str() {
                        s.to_string()
                    } else {
                        arg_value.to_string()
                    };
                    tool_call_str.push_str(&value_str);
                    tool_call_str.push_str("\n</parameter>\n");
                }
            }
        } else {
            // Fallback: use raw arguments string
            tool_call_str.push_str(arguments);
            tool_call_str.push('\n');
        }

        tool_call_str.push_str("</function>\n</tool_call>");

        has_content = true;

        let tokens = tokenizer
            .count(&tool_call_str)
            .unwrap_or(tool_call_str.len() / 4);
        *by_type.entry("tool_calls".to_string()).or_insert(0) += tokens;
        per_block.push(OutputTokenCount {
            content_type: "tool_calls".to_string(),
            tokens,
        });
    }

    if has_content {
        let total_tokens: usize = by_type.values().sum();
        Some(ResponseTokenCount {
            total_tokens,
            by_type,
            per_block,
        })
    } else {
        None
    }
}

/// Unified analyzer for extracting records from parsed/aggregated data
///
/// This analyzer provides a unified entry point for analysis, combining:
/// - `AuditAnalyzer`: Extracts audit records from aggregated results
/// - `TokenParser`: Extracts token usage from SSE events
/// - `MessageParser`: Parses LLM API request/response bodies
/// - Optional tokenizer for computing prompt token counts
pub struct Analyzer {
    audit: AuditAnalyzer,
    token: TokenParser,
    message: MessageParser,
    /// Optional tokenizer for computing prompt token counts
    tokenizer: Option<LlmTokenizer>,
    /// Optional chat template for formatting messages
    chat_template: Option<LlmTokenizer>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    /// Create a new unified analyzer without tokenizer
    pub fn new() -> Self {
        Analyzer {
            audit: AuditAnalyzer::new(),
            token: TokenParser::new(),
            message: MessageParser::new(),
            tokenizer: None,
            chat_template: None,
        }
    }

    /// Create a new unified analyzer with tokenizer for prompt token counting
    ///
    /// # Arguments
    /// * `tokenizer` - The tokenizer to use for computing prompt token counts
    /// * `chat_template` - The chat template to use for formatting messages
    ///
    /// # Example
    /// ```rust,ignore
    /// use agentsight::analyzer::Analyzer;
    /// use agentsight::tokenizer::{QwenTokenizer, ChatTemplateType};
    ///
    /// let tokenizer = QwenTokenizer::from_file("/path/to/tokenizer.json", "Qwen3.5")?;
    /// let chat_template = ChatTemplateType::Qwen.create_template();
    /// let analyzer = Analyzer::with_tokenizer(Box::new(tokenizer), chat_template);
    /// ```
    pub fn with_tokenizer(tokenizer: LlmTokenizer, chat_template: LlmTokenizer) -> Self {
        Analyzer {
            audit: AuditAnalyzer::new(),
            token: TokenParser::new(),
            message: MessageParser::new(),
            tokenizer: Some(tokenizer),
            chat_template: Some(chat_template),
        }
    }

    /// Analyze an aggregated result
    ///
    /// Returns `AnalysisResult::Audit` for LLM calls and process actions,
    /// `AnalysisResult::Token` for SSE streams containing token usage,
    /// or `AnalysisResult::Message` for parsed LLM API request/response bodies.
    pub fn analyze_aggregated(&self, result: &AggregatedResult) -> Vec<AnalysisResult> {
        log::debug!("Analyzing aggregated result({})", result.result_type());
        let mut results = Vec::new();

        // 1. Audit analysis for process actions (non-HTTP)
        if let AggregatedResult::ProcessComplete(_process) = result {
            if let Some(record) = self.audit.analyze(result) {
                results.push(AnalysisResult::Audit(record));
            }
            return results;
        }

        // 2. Token analysis - extract from SSE events or non-streaming JSON body
        let mut token_result = match result {
            AggregatedResult::SseComplete(pair) => {
                let pid = pair.request.source_event.pid;
                let comm = pair.request.source_event.comm_str();
                self.extract_token_from_sse(
                    &pair.response.sse_events,
                    pair.response.sse_continuation_bytes.as_deref(),
                    pid,
                    &comm,
                )
            }
            AggregatedResult::ResponseOnly { response, .. } if !response.sse_events.is_empty() => {
                let pid = response.pid();
                let comm = response.parsed.source_event.comm_str();
                self.extract_token_from_sse(
                    &response.sse_events,
                    response.sse_continuation_bytes.as_deref(),
                    pid,
                    &comm,
                )
            }
            AggregatedResult::HttpComplete(pair) => {
                let pid = pair.request.source_event.pid;
                let comm = pair.request.source_event.comm_str();
                self.extract_token_from_json_body(
                    pair.response.parsed.json_body().as_ref(),
                    pid,
                    &comm,
                )
            }
            AggregatedResult::Http2StreamComplete(stream) => {
                let pid = stream.pid();
                let comm = stream.comm();
                self.extract_token_from_json_body(stream.response_json_body().as_ref(), pid, &comm)
            }
            _ => None,
        };

        // 5. Token consumption analysis - breakdown by message role
        // This runs for any HTTP request with messages (not just SSE responses)
        // if let Some(breakdown) = self.analyze_token_consumption(result) {
        //     results.push(AnalysisResult::TokenConsumption(breakdown));
        // }

        // 3. Message analysis - parse LLM API request/response bodies
        if let Some(msg_result) = self.extract_message_from_http(result) {
            results.push(msg_result);
        }

        // 4. HTTP data export - extract raw HTTP request/response data
        if let Some(http_record) = self.extract_http_record(result) {
            if token_result.is_none() && http_record.is_sse {
                if let Some(body) = &http_record.response_body {
                    if let Ok(x) = serde_json::from_str::<Vec<serde_json::Value>>(body) {
                        if let Some(last) = x.last() {
                            let parser = TokenParser::new();
                            if let Some(usage) = parser.parse_json(last) {
                                let record = TokenRecord::new(
                                    http_record.pid,
                                    http_record.comm.clone(),
                                    usage.provider.to_string(),
                                    usage.input_tokens,
                                    usage.output_tokens,
                                )
                                .with_model(usage.model.clone().unwrap_or_default())
                                .with_cache_tokens(
                                    usage.cache_creation_input_tokens.unwrap_or(0),
                                    usage.cache_read_input_tokens.unwrap_or(0),
                                );

                                token_result = Some(record);
                            }
                        }
                    }
                }
            }

            // Backfill the model name from the request body when the response
            // does not carry one. The DashScope/Bailian native protocol puts
            // `model` only on the request (`output`/`usage`/`request_id` is all
            // the response has), so without this the token database records an
            // empty model for every native call.
            if let Some(record) = token_result.as_mut() {
                if record.model.as_deref().unwrap_or_default().is_empty() {
                    if let Some(model) = http_record
                        .request_body
                        .as_deref()
                        .and_then(Self::model_from_request_body)
                    {
                        record.model = Some(model);
                    }
                }
            }

            // Extract audit from HttpRecord (only for SSE responses / LLM calls)
            // Pass token_result so audit record gets populated token counts
            if let Some(audit_record) = self.audit.analyze_http(&http_record, token_result.as_ref())
            {
                results.push(AnalysisResult::Audit(audit_record));
            }

            results.push(AnalysisResult::Http(http_record));
        }

        if let Some(record) = token_result {
            results.push(AnalysisResult::Token(record));
        } else {
            // Fallback: manually compute tokens using get_global_tokenizer
            if let Some(record) = self.compute_tokens_manually(result) {
                results.push(AnalysisResult::Token(record));
            }
        }

        results
    }

    /// Read the top-level `model` field of an LLM request body.
    fn model_from_request_body(body: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()?
            .get("model")?
            .as_str()
            .filter(|m| !m.is_empty())
            .map(|m| m.to_string())
    }

    /// Extract parsed API message from HTTP request/response bodies
    fn extract_message_from_http(&self, result: &AggregatedResult) -> Option<AnalysisResult> {
        // eBPF capture feeds every HTTPS connection through this pipeline, so
        // non-LLM traffic (e.g. MCP server requests) lands here too.
        // parse_by_path would reject it anyway, but only after logging a
        // per-request diagnostic; filter up front to keep such traffic out of
        // the message parse path entirely.
        if !Self::should_parse_message(result) {
            return None;
        }
        match result {
            AggregatedResult::HttpComplete(pair) => {
                let req_body = pair.request.json_body();
                let resp_body = pair.response.parsed.json_body();
                self.analyze_message(&pair.request.path, req_body.as_ref(), resp_body.as_ref())
            }
            AggregatedResult::SseComplete(pair) => {
                let req_body = pair.request.json_body();
                // For SSE responses, parse from SSE events
                self.analyze_message_with_sse(
                    &pair.request.path,
                    req_body.as_ref(),
                    &pair.response.sse_events,
                )
            }
            AggregatedResult::RequestOnly { request, .. } => {
                let req_body = request.json_body();
                self.analyze_message(&request.path, req_body.as_ref(), None)
            }
            AggregatedResult::Http2StreamComplete(stream) => {
                let path = stream.path();
                if path.is_empty() {
                    return None;
                }
                let req_body = stream.request_json_body();
                if let Some(sse_json) = stream.response_sse_json_array() {
                    return self
                        .message
                        .parse_by_path(&path, req_body.as_ref(), Some(&sse_json))
                        .map(AnalysisResult::Message);
                }
                let resp_body = stream.response_json_body();
                self.analyze_message(&path, req_body.as_ref(), resp_body.as_ref())
            }
            AggregatedResult::ResponseOnly { .. }
            | AggregatedResult::ProcessComplete(_)
            | AggregatedResult::Http2Frames { .. } => None,
        }
    }

    /// Whether the aggregated result's request path belongs to a known LLM
    /// provider and is therefore worth message parsing.
    ///
    /// Unlike `GenAIBuilder::build_llm_call`, no `is_sse` exception is needed
    /// here: this predicate is exactly the OR of the per-provider
    /// `matches_path` checks that gate `parse_by_path` itself, so any SSE
    /// stream it rejects could never have produced a parsed message. The
    /// genai SSE fallback consumes the `HttpRecord` built elsewhere in
    /// `analyze_aggregated` and is unaffected by this filter.
    fn should_parse_message(result: &AggregatedResult) -> bool {
        match result {
            AggregatedResult::HttpComplete(pair) | AggregatedResult::SseComplete(pair) => {
                MessageParser::is_llm_api_path(&pair.request.path)
            }
            AggregatedResult::RequestOnly { request, .. } => {
                MessageParser::is_llm_api_path(&request.path)
            }
            AggregatedResult::Http2StreamComplete(stream) => {
                MessageParser::is_llm_api_path(&stream.path())
            }
            // Variants without a request path never reach message parsing.
            AggregatedResult::ResponseOnly { .. }
            | AggregatedResult::ProcessComplete(_)
            | AggregatedResult::Http2Frames { .. } => false,
        }
    }

    /// Parse API message from HTTP request body and SSE events
    ///
    /// This method parses LLM API request body and SSE response events
    /// to reconstruct the complete message.
    pub fn analyze_message_with_sse(
        &self,
        path: &str,
        request_body: Option<&serde_json::Value>,
        sse_events: &[ParsedSseEvent],
    ) -> Option<AnalysisResult> {
        self.message
            .parse_by_path_with_sse(path, request_body, sse_events)
            .map(AnalysisResult::Message)
    }

    /// Extract token usage from SSE events by merging field-by-field.
    ///
    /// Anthropic (and Anthropic-compatible proxies) split token usage across
    /// events: `message_start` carries `input_tokens` plus the cache counters,
    /// while the terminal `message_delta` carries only `output_tokens`. Some
    /// proxies additionally emit a zero-placeholder `message_start` or drop it
    /// entirely. Picking a single event therefore yields a bogus zero total, so
    /// we merge every parseable event, taking the max of each cumulative
    /// counter. OpenAI/Gemini pack all fields into one event, so the merge is a
    /// no-op for them.
    fn extract_token_from_sse(
        &self,
        sse_events: &[ParsedSseEvent],
        continuation_bytes: Option<&[u8]>,
        pid: u32,
        comm: &str,
    ) -> Option<TokenRecord> {
        let mut usage = sse_events
            .iter()
            .filter_map(|e| self.token.parse_event(e))
            .fold(None, Self::merge_usage);

        if usage.is_none() {
            // Fallback: OpenAI Responses API embeds usage in a final
            // `response.completed` event whose `data:` field routinely
            // exceeds a single TLS record. The aggregator buffers the
            // raw continuation bytes; re-parse them with the legacy
            // SSEParser (which concatenates multi-line data fields)
            // and merge all events. If reassembled events still don't
            // yield usage, fall back to a partial-scan over the raw
            // buffer text.
            if let Some(extra) = continuation_bytes {
                let text = String::from_utf8_lossy(extra);
                let reassembled = SSEParser::parse_stream(&text);
                usage = reassembled
                    .events
                    .iter()
                    .filter_map(|e| self.token.parse_data(&e.data))
                    .fold(None, Self::merge_usage);
                if usage.is_none() {
                    usage = self.token.parse_data(&text);
                    if usage.is_none() {
                        log::debug!(
                            "[extract_token_from_sse] continuation buffer scan miss: len={} reassembled_events={}",
                            extra.len(),
                            reassembled.events.len(),
                        );
                    }
                }
            }
        }

        let usage = usage?;

        let record = TokenRecord::new(
            pid,
            comm.to_string(),
            usage.provider.to_string(),
            usage.input_tokens,
            usage.output_tokens,
        )
        .with_model(usage.model.clone().unwrap_or_default())
        .with_cache_tokens(
            usage.cache_creation_input_tokens.unwrap_or(0),
            usage.cache_read_input_tokens.unwrap_or(0),
        );

        // NOTE: tool_calls and reasoning_content extraction from SSE events
        // is handled in genai::builder via direct SSE response body parsing.

        if record.total_tokens() == 0 {
            return None;
        }

        Some(record)
    }

    /// Merge two token-usage snapshots from the same SSE stream.
    ///
    /// All counters are cumulative within a stream, so the max resolves the
    /// split-across-events case (message_start input/cache vs message_delta
    /// output) and tolerates zero-placeholder events without ever regressing a
    /// larger value. Model and provider are taken from the first event that
    /// carries them.
    fn merge_usage(acc: Option<TokenUsage>, next: TokenUsage) -> Option<TokenUsage> {
        let Some(mut cur) = acc else {
            return Some(next);
        };
        cur.input_tokens = cur.input_tokens.max(next.input_tokens);
        cur.output_tokens = cur.output_tokens.max(next.output_tokens);
        cur.cache_creation_input_tokens = Self::max_opt(
            cur.cache_creation_input_tokens,
            next.cache_creation_input_tokens,
        );
        cur.cache_read_input_tokens =
            Self::max_opt(cur.cache_read_input_tokens, next.cache_read_input_tokens);
        if cur.model.is_none() {
            cur.model = next.model;
        }
        if cur.provider == LLMProvider::Unknown {
            cur.provider = next.provider;
        }
        Some(cur)
    }

    /// Max of two optional counters, preserving a value when only one is set.
    fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
        match (a, b) {
            (Some(x), Some(y)) => Some(x.max(y)),
            (x, None) => x,
            (None, y) => y,
        }
    }

    fn extract_token_from_json_body(
        &self,
        json: Option<&serde_json::Value>,
        pid: u32,
        comm: &str,
    ) -> Option<TokenRecord> {
        let json = json?;
        let usage = self.token.parse_json(json)?;
        let record = TokenRecord::new(
            pid,
            comm.to_string(),
            usage.provider.to_string(),
            usage.input_tokens,
            usage.output_tokens,
        )
        .with_model(usage.model.unwrap_or_default())
        .with_cache_tokens(
            usage.cache_creation_input_tokens.unwrap_or(0),
            usage.cache_read_input_tokens.unwrap_or(0),
        );
        if record.total_tokens() > 0 {
            Some(record)
        } else {
            None
        }
    }

    /// Manually compute token counts using get_global_tokenizer
    ///
    /// This method is called when token extraction from SSE events fails.
    /// It uses the global tokenizer to compute input and output tokens
    /// from the request messages and response content.
    ///
    /// If the analyzer was created without a tokenizer (the `tokenizer`
    /// feature is disabled), the fallback is skipped to avoid triggering
    /// downloads/loads of large tokenizer models.
    fn compute_tokens_manually(&self, result: &AggregatedResult) -> Option<TokenRecord> {
        self.tokenizer.as_ref()?;

        // Extract context from the aggregated result.
        // Both SseComplete and Http2StreamComplete are unified into
        // Vec<serde_json::Value> chunks to avoid a method-local enum
        // (which cbindgen 0.27 cannot parse).
        let (pid, comm, request_json, sse_chunks, path) = match result {
            AggregatedResult::SseComplete(pair) => {
                let chunks: Vec<serde_json::Value> = pair
                    .response
                    .sse_events
                    .iter()
                    .filter_map(|e| e.json_body())
                    .collect();
                (
                    pair.request.source_event.pid,
                    pair.request.source_event.comm_str(),
                    pair.request.json_body(),
                    chunks,
                    pair.request.path.clone(),
                )
            }
            AggregatedResult::Http2StreamComplete(stream) => {
                let chunks = stream
                    .response_sse_json_array()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                (
                    stream.pid(),
                    stream.comm(),
                    stream.request_json_body(),
                    chunks,
                    stream.path(),
                )
            }
            _ => return None,
        };

        // Get request JSON
        let request_json_ref = request_json.as_ref()?;

        // Extract model name
        let model = request_json_ref
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown");

        // Try to get tokenizer for the model
        let tokenizer = match get_global_tokenizer(model) {
            Ok(t) => t,
            Err(e) => {
                log::debug!("Failed to get tokenizer for model '{model}': {e}");
                return None;
            }
        };

        // Extract provider from path
        let provider = if path.contains("anthropic") {
            "anthropic"
        } else {
            "openai"
        };

        // Count input tokens from request messages using chat template.
        // Supports both OpenAI chat completions format (top-level "messages")
        // and Responses API format (top-level "input" + "instructions").
        let messages_owned: Option<Vec<serde_json::Value>> = request_json_ref
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .or_else(|| {
                let input = request_json_ref.get("input").and_then(|m| m.as_array())?;
                let mut combined = Vec::new();
                if let Some(instr) = request_json_ref
                    .get("instructions")
                    .and_then(|s| s.as_str())
                {
                    if !instr.is_empty() {
                        combined.push(serde_json::json!({
                            "role": "system",
                            "content": instr,
                        }));
                    }
                }
                combined.extend(input.iter().cloned());
                Some(combined)
            });

        let input_tokens = if let Some(messages) = messages_owned {
            if messages.is_empty() {
                0
            } else {
                // Clone messages for in-place modification of tool_calls.arguments
                let mut msgs = messages;

                // Process tool_calls arguments: parse JSON string to object in place
                for msg in msgs.iter_mut() {
                    if let Some(tool_calls) =
                        msg.get_mut("tool_calls").and_then(|tc| tc.as_array_mut())
                    {
                        for tool_call in tool_calls.iter_mut() {
                            if let Some(func) = tool_call.get_mut("function") {
                                if let Some(args) = func.get("arguments") {
                                    if let Some(args_str) = args.as_str() {
                                        // Try to parse arguments string as JSON object
                                        if let Ok(parsed) =
                                            serde_json::from_str::<serde_json::Value>(args_str)
                                        {
                                            func["arguments"] = parsed;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Extract tools JSON array for passing to template
                let tools_json: Option<Vec<serde_json::Value>> = request_json_ref
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .map(|arr| arr.to_vec());
                let tools_slice = tools_json.as_deref();

                // Apply chat template with tools to get the actual prompt sent to LLM
                match tokenizer.apply_chat_template_with_tools(&msgs, tools_slice, true) {
                    Ok(formatted) => tokenizer.count(&formatted).unwrap_or(0) as u64,
                    Err(e) => {
                        log::warn!("Failed to apply chat template: {e}, falling back to raw count");
                        // Fallback: count raw message content
                        let mut total = 0u64;
                        for msg in &msgs {
                            if let Ok(msg_str) = serde_json::to_string(msg) {
                                total += tokenizer.count(&msg_str).unwrap_or(0) as u64;
                            }
                        }
                        total
                    }
                }
            }
        } else {
            0
        };

        // Count output tokens from SSE events content
        let output_tokens = {
            let mut all_content = String::new();
            let mut all_reasoning = String::new();
            let mut all_tool_calls = Vec::new();

            for chunk in &sse_chunks {
                if let Some((content, reasoning, tool_calls)) =
                    extract_response_content(Some(chunk))
                {
                    if !content.is_empty() {
                        all_content.push_str(&content);
                    }
                    if let Some(r) = reasoning {
                        if !r.is_empty() {
                            all_reasoning.push_str(&r);
                        }
                    }
                    for tc in tool_calls {
                        if !tc.is_empty() {
                            all_tool_calls.push(tc);
                        }
                    }
                }
            }

            let mut total = 0u64;

            // Count reasoning tokens
            if !all_reasoning.is_empty() {
                let reasoning_with_tags = format!("<think>\n{all_reasoning}\n</think>\n\n");
                total += tokenizer.count(&reasoning_with_tags).unwrap_or(0) as u64;
            }

            // Count text tokens
            if !all_content.is_empty() {
                total += tokenizer.count(&all_content).unwrap_or(0) as u64;
            }

            // Count tool call tokens
            if !all_tool_calls.is_empty() {
                let aggregated_tool_calls = all_tool_calls.join("");
                total += tokenizer.count(&aggregated_tool_calls).unwrap_or(0) as u64;
            }

            total
        };

        // Create TokenRecord
        let record = TokenRecord::new(
            pid,
            comm.to_string(),
            provider.to_string(),
            input_tokens,
            output_tokens,
        )
        .with_model(model.to_string());

        Some(record)
    }

    /// Extract HTTP request/response record from an aggregated result
    ///
    /// Exports the raw HTTP exchange data including method, path, headers,
    /// body, status code, and SSE event payloads for persistence.
    fn extract_http_record(&self, result: &AggregatedResult) -> Option<HttpRecord> {
        match result {
            AggregatedResult::HttpComplete(pair) => {
                let req = &pair.request;
                let resp = &pair.response;

                let request_body = req
                    .json_body()
                    .map(|v| serde_json::to_string(&v).unwrap_or_default())
                    .or_else(|| {
                        let body = req.body();
                        if body.is_empty() {
                            None
                        } else {
                            Some(String::from_utf8_lossy(body).to_string())
                        }
                    });

                let response_body = resp
                    .parsed
                    .json_body()
                    .map(|v| serde_json::to_string(&v).unwrap_or_default())
                    .or_else(|| {
                        let body_str = resp.parsed.body_str_decompressed();
                        if body_str.is_empty() {
                            None
                        } else {
                            Some(body_str)
                        }
                    });

                Some(HttpRecord {
                    timestamp_ns: req.source_event.timestamp_ns,
                    pid: req.source_event.pid,
                    comm: req.source_event.comm_str(),
                    method: req.method.clone(),
                    path: req.path.clone(),
                    status_code: resp.status_code(),
                    request_headers: serde_json::to_string(&req.headers).unwrap_or_default(),
                    request_body,
                    response_headers: serde_json::to_string(&resp.parsed.headers)
                        .unwrap_or_default(),
                    response_body,
                    duration_ns: resp
                        .end_timestamp_ns()
                        .saturating_sub(req.source_event.timestamp_ns),
                    first_output_timestamp_ns: None,
                    is_sse: false,
                    sse_event_count: 0,
                })
            }
            AggregatedResult::SseComplete(pair) => {
                let req = &pair.request;
                let resp = &pair.response;

                let request_body = req
                    .json_body()
                    .map(|v| serde_json::to_string(&v).unwrap_or_default())
                    .or_else(|| {
                        let body = req.body();
                        if body.is_empty() {
                            None
                        } else {
                            Some(String::from_utf8_lossy(body).to_string())
                        }
                    });

                // For SSE responses, aggregate all SSE event JSON payloads
                let sse_json_bodies = resp.json_body();
                let response_body = if sse_json_bodies.is_empty() {
                    None
                } else {
                    serde_json::to_string(&sse_json_bodies).ok()
                };

                Some(HttpRecord {
                    timestamp_ns: req.source_event.timestamp_ns,
                    pid: req.source_event.pid,
                    comm: req.source_event.comm_str(),
                    method: req.method.clone(),
                    path: req.path.clone(),
                    status_code: resp.status_code(),
                    request_headers: serde_json::to_string(&req.headers).unwrap_or_default(),
                    request_body,
                    response_headers: serde_json::to_string(&resp.parsed.headers)
                        .unwrap_or_default(),
                    response_body,
                    duration_ns: resp
                        .end_timestamp_ns()
                        .saturating_sub(req.source_event.timestamp_ns),
                    first_output_timestamp_ns: resp.first_output_timestamp_ns(),
                    is_sse: true,
                    sse_event_count: resp.sse_event_count(),
                })
            }
            AggregatedResult::RequestOnly { request, .. } => {
                let request_body = request
                    .json_body()
                    .map(|v| serde_json::to_string(&v).unwrap_or_default())
                    .or_else(|| {
                        let body = request.body();
                        if body.is_empty() {
                            None
                        } else {
                            Some(String::from_utf8_lossy(body).to_string())
                        }
                    });

                Some(HttpRecord {
                    timestamp_ns: request.source_event.timestamp_ns,
                    pid: request.source_event.pid,
                    comm: request.source_event.comm_str(),
                    method: request.method.clone(),
                    path: request.path.clone(),
                    status_code: 0,
                    request_headers: serde_json::to_string(&request.headers).unwrap_or_default(),
                    request_body,
                    response_headers: String::new(),
                    response_body: None,
                    duration_ns: 0,
                    first_output_timestamp_ns: None,
                    is_sse: false,
                    sse_event_count: 0,
                })
            }
            AggregatedResult::Http2StreamComplete(stream) => {
                let request_body = stream.request_body_str();

                // Try SSE parsing first, fallback to regular text if it fails
                // This is more robust than checking content-type header (which may fail due to HPACK)
                let parsed_sse_json = stream.response_sse_json_array();
                let sse_event_count = stream.response_sse_event_count();
                let response_body = if let Some(sse_json) = parsed_sse_json.as_ref() {
                    // Successfully parsed as SSE
                    Some(serde_json::to_string(sse_json).unwrap_or_default())
                } else {
                    // Not SSE, try regular JSON or raw text
                    stream
                        .response_json_body()
                        .map(|v| serde_json::to_string(&v).unwrap_or_default())
                        .or_else(|| stream.response_body_str())
                };
                let is_sse = stream.is_response_sse() || sse_event_count > 0;

                Some(HttpRecord {
                    timestamp_ns: stream.start_timestamp_ns,
                    pid: stream.pid(),
                    comm: stream.comm(),
                    method: stream.method(),
                    path: stream.path(),
                    status_code: stream.status_code(),
                    request_headers: stream.request_headers_json(),
                    request_body,
                    response_headers: stream.response_headers_json(),
                    response_body,
                    duration_ns: stream
                        .end_timestamp_ns
                        .saturating_sub(stream.start_timestamp_ns),
                    first_output_timestamp_ns: if is_sse {
                        stream.first_output_timestamp_ns()
                    } else {
                        None
                    },
                    is_sse,
                    sse_event_count,
                })
            }
            // ProcessComplete and ResponseOnly don't have request info, skip
            _ => None,
        }
    }

    /// Analyze an SSE event for token usage
    ///
    /// Returns `TokenUsage` if the event contains usage information.
    /// Use `analyze_sse_event_full` to get a complete `TokenRecord`.
    pub fn analyze_sse_event(&self, event: &ParsedSseEvent) -> Option<TokenUsage> {
        self.token.parse_event(event)
    }

    /// Analyze an SSE event and build a complete TokenRecord
    ///
    /// This method extracts token usage and combines it with process metadata
    /// to create a complete record suitable for storage.
    pub fn analyze_sse_event_full(
        &self,
        event: &ParsedSseEvent,
        pid: u32,
        comm: &str,
    ) -> Option<TokenRecord> {
        let usage = self.token.parse_event(event)?;

        Some(
            TokenRecord::new(
                pid,
                comm.to_string(),
                usage.provider.to_string(),
                usage.input_tokens,
                usage.output_tokens,
            )
            .with_model(usage.model.clone().unwrap_or_default())
            .with_cache_tokens(
                usage.cache_creation_input_tokens.unwrap_or(0),
                usage.cache_read_input_tokens.unwrap_or(0),
            ),
        )
    }

    /// Analyze an SSE event and return AnalysisResult
    ///
    /// Convenience method that wraps the result in `AnalysisResult::Token`.
    pub fn analyze_sse_as_result(
        &self,
        event: &ParsedSseEvent,
        pid: u32,
        comm: &str,
    ) -> Option<AnalysisResult> {
        self.analyze_sse_event_full(event, pid, comm)
            .map(AnalysisResult::Token)
    }

    /// Get reference to the audit analyzer
    pub fn audit_analyzer(&self) -> &AuditAnalyzer {
        &self.audit
    }

    /// Get reference to the token parser
    pub fn token_parser(&self) -> &TokenParser {
        &self.token
    }

    /// Get reference to the message parser
    pub fn message_parser(&self) -> &MessageParser {
        &self.message
    }

    /// Parse API message from HTTP request/response bodies
    ///
    /// This method parses LLM API request and response bodies based on the
    /// HTTP path to detect the provider (OpenAI or Anthropic).
    ///
    /// # Arguments
    /// * `path` - The HTTP request path (e.g., "/v1/chat/completions")
    /// * `request_body` - Optional JSON body from the HTTP request
    /// * `response_body` - Optional JSON body from the HTTP response
    ///
    /// # Returns
    /// * `Some(AnalysisResult::Message)` if parsing succeeds
    /// * `None` if the path doesn't match a known LLM API endpoint
    pub fn analyze_message(
        &self,
        path: &str,
        request_body: Option<&serde_json::Value>,
        response_body: Option<&serde_json::Value>,
    ) -> Option<AnalysisResult> {
        self.message
            .parse_by_path(path, request_body, response_body)
            .map(AnalysisResult::Message)
    }

    /// Parse API message and return the raw ParsedApiMessage
    ///
    /// Use this method when you need direct access to the parsed message
    /// without the AnalysisResult wrapper.
    pub fn parse_message(
        &self,
        path: &str,
        request_body: Option<&serde_json::Value>,
        response_body: Option<&serde_json::Value>,
    ) -> Option<ParsedApiMessage> {
        self.message
            .parse_by_path(path, request_body, response_body)
    }

    /// Analyze AggregatedResult and extract token consumption breakdown
    ///
    /// This is a convenience method that combines extract_token_data and
    /// compute_token_consumption into a single call.
    pub fn analyze_token_consumption(
        &self,
        result: &AggregatedResult,
    ) -> Option<TokenConsumptionBreakdown> {
        // Extract context (pid, comm) from the aggregated result (SseComplete only)
        let (pid, comm, request_json, response_jsons, path) = match result {
            AggregatedResult::SseComplete(pair) => (
                pair.request.source_event.pid,
                pair.request.source_event.comm_str(),
                pair.request.json_body(),
                // For SSE responses, get all SSE event JSONs for aggregation
                {
                    let mut res = pair.response.parsed.json_body().map_or(vec![], |x| vec![x]);
                    res.extend(pair.response.json_body());
                    res
                },
                pair.request.path.clone(),
            ),
            _ => return None,
        };

        // Get tokenizer and chat template
        let (tokenizer, chat_template) = match (&self.tokenizer, &self.chat_template) {
            (Some(t), Some(ct)) => (t, ct),
            _ => {
                log::warn!(
                    "Tokenizer or chat template not available, cannot compute accurate token consumption"
                );
                return None;
            }
        };

        // Get request JSON reference
        let request_json_ref = request_json.as_ref()?;

        // Extract model
        let model = request_json_ref
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Extract provider from path
        let provider = if path.contains("anthropic") {
            "anthropic"
        } else {
            "openai"
        }
        .to_string();

        // Count request tokens
        let request_count = count_request_tokens(request_json_ref, tokenizer, chat_template)?;

        // Count response tokens
        let response_count = count_response_tokens(&response_jsons, tokenizer)?;

        // Build TokenConsumptionBreakdown from request and response counts
        Some(TokenConsumptionBreakdown {
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            pid,
            comm,
            provider,
            model,
            total_input_tokens: request_count.total_tokens,
            total_output_tokens: response_count.total_tokens,
            by_role: request_count.by_role,
            per_message: request_count.per_message,
            tools_tokens: request_count.tools_tokens,
            system_prompt_tokens: request_count.system_prompt_tokens,
            output_by_type: response_count.by_type,
            output_per_block: response_count.per_block,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::sslsniff::SslEvent;
    use std::rc::Rc;

    fn create_test_event(data: &str) -> ParsedSseEvent {
        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1234567890,
            delta_ns: 0,
            pid: 1234,
            tid: 5678,
            uid: 0,
            len: data.len() as u32,
            rw: 0,
            comm: String::new(),
            buf: data.as_bytes().to_vec(),
            is_handshake: false,
            ssl_ptr: 0,
        });
        ParsedSseEvent::new(None, None, None, 0, data.len(), ssl_event)
    }

    #[test]
    fn test_extract_token_from_json_body_openai() {
        let analyzer = Analyzer::new();
        let json = serde_json::json!({
            "id": "chatcmpl-test",
            "model": "gpt-4o",
            "choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let result = analyzer.extract_token_from_json_body(Some(&json), 1234, "test");
        assert!(result.is_some());
        let record = result.unwrap();
        assert_eq!(record.input_tokens, 10);
        assert_eq!(record.output_tokens, 5);
    }

    #[test]
    fn test_extract_token_from_json_body_none() {
        let analyzer = Analyzer::new();
        assert!(
            analyzer
                .extract_token_from_json_body(None, 1234, "test")
                .is_none()
        );
    }

    #[test]
    fn test_extract_token_from_json_body_no_usage() {
        let analyzer = Analyzer::new();
        let json = serde_json::json!({"id": "test", "choices": []});
        assert!(
            analyzer
                .extract_token_from_json_body(Some(&json), 1234, "test")
                .is_none()
        );
    }

    #[test]
    fn test_analyze_http2_stream_complete_extracts_tokens() {
        use crate::aggregator::{Http2Stream, StreamId};
        use crate::parser::{Http2FrameType, ParsedHttp2Frame};
        use crate::probes::sslsniff::SslEvent;
        use std::rc::Rc;

        let analyzer = Analyzer::new();

        let response_body = serde_json::json!({
            "id": "chatcmpl-h2-test",
            "model": "gpt-4o",
            "choices": [{"message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28}
        });
        let body_bytes = serde_json::to_vec(&response_body).unwrap();

        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1_000_000_000,
            delta_ns: 0,
            pid: 2000,
            tid: 2000,
            uid: 0,
            len: body_bytes.len() as u32,
            rw: 0,
            comm: "python3".to_string(),
            buf: body_bytes.clone(),
            is_handshake: false,
            ssl_ptr: 0x5000,
        });

        let request_body =
            b"{\"model\":\"gpt-4o\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
        let req_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 900_000_000,
            delta_ns: 0,
            pid: 2000,
            tid: 2000,
            uid: 0,
            len: request_body.len() as u32,
            rw: 1,
            comm: "python3".to_string(),
            buf: request_body.to_vec(),
            is_handshake: false,
            ssl_ptr: 0x5000,
        });

        use crate::aggregator::ConnectionId;
        let mut stream = Http2Stream::new(
            StreamId {
                connection_id: ConnectionId {
                    pid: 2000,
                    ssl_ptr: 0x5000,
                },
                stream_id: 1,
            },
            900_000_000,
        );
        stream.request_headers = Some(ParsedHttp2Frame {
            frame_type: Http2FrameType::Headers,
            flags: 0x4,
            stream_id: 1,
            payload_offset: 0,
            payload_len: 0,
            source_event: Rc::clone(&req_event),
        });
        stream.decoded_request_headers = Some(vec![
            (":method".to_string(), "POST".to_string()),
            (":path".to_string(), "/v1/chat/completions".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        stream.request_data_frames.push(ParsedHttp2Frame {
            frame_type: Http2FrameType::Data,
            flags: 0x1,
            stream_id: 1,
            payload_offset: 0,
            payload_len: request_body.len(),
            source_event: req_event,
        });
        stream.response_headers = Some(ParsedHttp2Frame {
            frame_type: Http2FrameType::Headers,
            flags: 0x4,
            stream_id: 1,
            payload_offset: 0,
            payload_len: 0,
            source_event: Rc::clone(&ssl_event),
        });
        stream.decoded_response_headers = Some(vec![
            (":status".to_string(), "200".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        stream.response_data_frames.push(ParsedHttp2Frame {
            frame_type: Http2FrameType::Data,
            flags: 0x1,
            stream_id: 1,
            payload_offset: 0,
            payload_len: body_bytes.len(),
            source_event: ssl_event,
        });
        stream.request_complete = true;
        stream.response_complete = true;
        stream.end_timestamp_ns = 1_100_000_000;

        let agg = AggregatedResult::Http2StreamComplete(stream);
        let results = analyzer.analyze_aggregated(&agg);

        let token_result = results
            .iter()
            .find(|r| matches!(r, AnalysisResult::Token(_)));
        assert!(
            token_result.is_some(),
            "Http2StreamComplete with usage should produce a TokenRecord"
        );
        if let Some(AnalysisResult::Token(record)) = token_result {
            assert_eq!(record.input_tokens, 20);
            assert_eq!(record.output_tokens, 8);
        }
    }

    /// Build a minimal `Http2Stream` with an SSE-formatted response body
    /// (`data: {...}\n\n`) so `response_sse_json_array()` detects it as SSE.
    fn build_sse_http2_stream(
        path: &str,
        request_body: &[u8],
        sse_chunk: Option<&serde_json::Value>,
    ) -> crate::aggregator::Http2Stream {
        let response_body = sse_chunk.map_or_else(
            || "data: [DONE]\n\n".to_string(),
            |chunk| format!("data: {}\n\ndata: [DONE]\n\n", chunk),
        );
        build_http2_stream(
            path,
            request_body,
            response_body.into_bytes(),
            "text/event-stream",
        )
    }

    fn build_http2_stream(
        path: &str,
        request_body: &[u8],
        body_bytes: Vec<u8>,
        content_type: &str,
    ) -> crate::aggregator::Http2Stream {
        use crate::aggregator::{ConnectionId, Http2Stream, StreamId};
        use crate::parser::{Http2FrameType, ParsedHttp2Frame};
        use crate::probes::sslsniff::SslEvent;
        use std::rc::Rc;

        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1_000_000_000,
            delta_ns: 0,
            pid: 3000,
            tid: 3000,
            uid: 0,
            len: body_bytes.len() as u32,
            rw: 0,
            comm: "python3".to_string(),
            buf: body_bytes.clone(),
            is_handshake: false,
            ssl_ptr: 0x6000,
        });

        let req_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 900_000_000,
            delta_ns: 0,
            pid: 3000,
            tid: 3000,
            uid: 0,
            len: request_body.len() as u32,
            rw: 1,
            comm: "python3".to_string(),
            buf: request_body.to_vec(),
            is_handshake: false,
            ssl_ptr: 0x6000,
        });

        let mut stream = Http2Stream::new(
            StreamId {
                connection_id: ConnectionId {
                    pid: 3000,
                    ssl_ptr: 0x6000,
                },
                stream_id: 1,
            },
            900_000_000,
        );
        stream.request_headers = Some(ParsedHttp2Frame {
            frame_type: Http2FrameType::Headers,
            flags: 0x4,
            stream_id: 1,
            payload_offset: 0,
            payload_len: 0,
            source_event: Rc::clone(&req_event),
        });
        stream.decoded_request_headers = Some(vec![
            (":method".to_string(), "POST".to_string()),
            (":path".to_string(), path.to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        stream.request_data_frames.push(ParsedHttp2Frame {
            frame_type: Http2FrameType::Data,
            flags: 0x1,
            stream_id: 1,
            payload_offset: 0,
            payload_len: request_body.len(),
            source_event: req_event,
        });
        stream.response_headers = Some(ParsedHttp2Frame {
            frame_type: Http2FrameType::Headers,
            flags: 0x4,
            stream_id: 1,
            payload_offset: 0,
            payload_len: 0,
            source_event: Rc::clone(&ssl_event),
        });
        stream.decoded_response_headers = Some(vec![
            (":status".to_string(), "200".to_string()),
            ("content-type".to_string(), content_type.to_string()),
        ]);
        stream.response_data_frames.push(ParsedHttp2Frame {
            frame_type: Http2FrameType::Data,
            flags: 0x1,
            stream_id: 1,
            payload_offset: 0,
            payload_len: body_bytes.len(),
            source_event: ssl_event,
        });
        stream.request_complete = true;
        stream.response_complete = true;
        stream.end_timestamp_ns = 1_100_000_000;
        stream
    }
    fn analyze_http2_record(
        analyzer: &Analyzer,
        stream: crate::aggregator::Http2Stream,
    ) -> HttpRecord {
        analyzer
            .analyze_aggregated(&AggregatedResult::Http2StreamComplete(stream))
            .into_iter()
            .find_map(|result| match result {
                AnalysisResult::Http(record) => Some(record),
                _ => None,
            })
            .expect("Analyzer must emit HttpRecord")
    }

    #[test]
    fn first_output_timestamp_propagates_from_http2_to_latency_metrics() {
        use crate::genai::{GenAIBuilder, GenAISemanticEvent};
        use crate::response_map::ResponseSessionMapper;
        use std::collections::HashMap;

        let analyzer = Analyzer::new();
        let request_body =
            br#"{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
        let chunk = serde_json::json!({
            "id": "chatcmpl-h2-propagation",
            "model": "gpt-4o",
            "choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]
        });
        let stream = build_sse_http2_stream("/v1/chat/completions", request_body, Some(&chunk));
        let results = analyzer.analyze_aggregated(&AggregatedResult::Http2StreamComplete(stream));

        let http = results
            .iter()
            .find_map(|result| match result {
                AnalysisResult::Http(record) => Some(record),
                _ => None,
            })
            .expect("Analyzer must emit HttpRecord");
        assert_eq!(http.first_output_timestamp_ns, Some(1_000_000_000));
        assert!(http.is_sse);

        let builder = GenAIBuilder::new();
        let mapper = ResponseSessionMapper::new();
        let cache = HashMap::<u32, String>::new();
        let (built, pending) = builder.build_with_pending(&results, &mapper, &cache);
        let call = built
            .events
            .into_iter()
            .find_map(|event| match event {
                GenAISemanticEvent::LLMCall(call) => Some(call),
                _ => None,
            })
            .expect("GenAIBuilder must emit LLMCall");
        assert_eq!(
            call.metadata.get("first_output_timestamp_ns"),
            Some(&"1000000000".to_string())
        );

        let event = GenAISemanticEvent::LLMCall(call);
        let path = std::env::temp_dir().join(format!(
            "agentsight_analyzer_latency_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = crate::storage::sqlite::genai::GenAISqliteStore::new_with_path(
            &path,
            crate::config::PeriodicStoragePolicy::default(),
        )
        .unwrap();
        if let Some(info) = pending.as_ref() {
            store.insert_pending(info).unwrap();
        }
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

    /// Regression for #3129: a native DashScope streaming call carries its
    /// streaming switch in the `X-DashScope-SSE` request header, not the body
    /// "stream" field, so the pending row is inserted with is_sse=0. The
    /// observed value must be backfilled by `complete_pending`, otherwise
    /// `streaming_call_count` misses every native streaming call.
    #[test]
    fn native_dashscope_sse_backfills_is_sse_end_to_end() {
        use crate::genai::{GenAIBuilder, GenAISemanticEvent};
        use crate::response_map::ResponseSessionMapper;
        use std::collections::HashMap;

        let analyzer = Analyzer::new();
        // Native DashScope body: no top-level "stream" field.
        let request_body = br#"{"model":"qwen-flash","input":{"messages":[{"role":"user","content":"hi"}]},"parameters":{"incremental_output":true,"result_format":"message"}}"#;
        let chunk = serde_json::json!({
            "output": {"choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}]},
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
            "request_id": "native-sse-e2e"
        });
        let stream = build_sse_http2_stream(
            "/api/v1/services/aigc/text-generation/generation",
            request_body,
            Some(&chunk),
        );
        let results = analyzer.analyze_aggregated(&AggregatedResult::Http2StreamComplete(stream));
        let http = results
            .iter()
            .find_map(|result| match result {
                AnalysisResult::Http(record) => Some(record),
                _ => None,
            })
            .expect("Analyzer must emit HttpRecord");
        assert!(http.is_sse, "native SSE response must be observed");

        let builder = GenAIBuilder::new();
        let mapper = ResponseSessionMapper::new();
        let cache = HashMap::<u32, String>::new();
        let (built, pending) = builder.build_with_pending(&results, &mapper, &cache);
        // The request body has no "stream" field — the pending row captures
        // the request-side view (is_sse=false), the #3129 starting state.
        assert_eq!(pending.as_ref().map(|p| p.is_sse), Some(false));

        let event = built
            .events
            .into_iter()
            .find(|event| matches!(event, GenAISemanticEvent::LLMCall(_)))
            .expect("GenAIBuilder must emit LLMCall");

        let path =
            std::env::temp_dir().join(format!("agentsight_native_sse_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = crate::storage::sqlite::genai::GenAISqliteStore::new_with_path(
            &path,
            crate::config::PeriodicStoragePolicy::default(),
        )
        .unwrap();
        if let Some(info) = pending.as_ref() {
            store.insert_pending(info).unwrap();
        }
        store.complete_pending(&event).unwrap();

        let metrics = store.get_latency_metrics(0, 2_000_000_000, None).unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].streaming_call_count, 1,
            "native streaming call must be counted after is_sse backfill (#3129)"
        );

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_extract_message_from_http_parses_openai_sse_request() {
        // SSE-shaped OpenAI response: branch A now delegates to parse_by_path
        // (restored), so the request body is parsed even though the OpenAI
        // parser does not deep-parse the SSE response array — that part is
        // still reconstructed by genai::GenAIBuilder's SSE fallback.
        let analyzer = Analyzer::new();
        let request_body = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;
        let chunk = serde_json::json!({
            "id": "chatcmpl-h2-sse",
            "model": "gpt-4o",
            "choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]
        });
        let stream = build_sse_http2_stream("/v1/chat/completions", request_body, Some(&chunk));

        let agg = AggregatedResult::Http2StreamComplete(stream);
        let result = analyzer.extract_message_from_http(&agg);
        assert!(result.is_some());
    }

    #[test]
    fn test_extract_message_from_http_still_parses_sysom_sse() {
        // SysOM's llmParamString/tool_use envelope has no HttpRecord-based
        // fallback, so branch A must keep deep-parsing it even over SSE.
        let analyzer = Analyzer::new();
        let params = serde_json::json!({
            "model": "qwen3-coder-plus",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let request_body_val = serde_json::json!({"llmParamString": params.to_string()});
        let request_body = request_body_val.to_string().into_bytes();
        let chunk = serde_json::json!({
            "choices": [{"message": {"content": "hi", "tool_use": null}}]
        });
        let stream = build_sse_http2_stream(
            "/api/v1/copilot/generate_copilot",
            &request_body,
            Some(&chunk),
        );

        let agg = AggregatedResult::Http2StreamComplete(stream);
        let result = analyzer.extract_message_from_http(&agg);
        match result {
            Some(AnalysisResult::Message(ParsedApiMessage::SysomMessage {
                response: Some(resp),
                ..
            })) => {
                assert_eq!(resp.choices[0].message.content, "hi");
            }
            other => panic!("expected SysomMessage with response, got {other:?}"),
        }
    }

    #[test]
    fn test_should_parse_message_rejects_non_llm_path() {
        // MCP servers stream JSON-RPC over percent-encoded paths like the one
        // below. The discriminating point is the pre-filter branch itself:
        // parse_by_path returned None for such paths even before the filter
        // existed, so only a direct assertion on should_parse_message can
        // fail if the filter is removed.
        let analyzer = Analyzer::new();
        let request_body = br#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#;
        let chunk = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {}});
        let stream = build_sse_http2_stream(
            "/@modelcontextprotocol%2fserver-everything",
            request_body,
            Some(&chunk),
        );
        let agg = AggregatedResult::Http2StreamComplete(stream);

        assert!(!Analyzer::should_parse_message(&agg));
        assert!(analyzer.extract_message_from_http(&agg).is_none());

        // Positive control: the same filter must pass LLM paths (SSE
        // included), otherwise it would silently drop provider traffic.
        let llm_stream = build_sse_http2_stream(
            "/v1/chat/completions",
            br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
            Some(&serde_json::json!({"id": "chatcmpl-1"})),
        );
        assert!(Analyzer::should_parse_message(
            &AggregatedResult::Http2StreamComplete(llm_stream)
        ));
    }

    #[test]
    fn http2_sse_without_observable_output_remains_streaming() {
        let analyzer = Analyzer::new();
        let request_body =
            br#"{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
        let stream = build_sse_http2_stream("/v1/chat/completions", request_body, None);

        let http = analyze_http2_record(&analyzer, stream);

        assert!(http.is_sse);
        assert_eq!(http.first_output_timestamp_ns, None);
        assert_eq!(http.sse_event_count, 1);
    }

    #[test]
    fn http2_metadata_only_sse_keeps_streaming_identity_without_first_output() {
        let analyzer = Analyzer::new();
        let request_body =
            br#"{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
        let stream = build_sse_http2_stream(
            "/v1/chat/completions",
            request_body,
            Some(&serde_json::json!({"type": "response.created"})),
        );

        let http = analyze_http2_record(&analyzer, stream);

        assert!(http.is_sse);
        assert_eq!(http.first_output_timestamp_ns, None);
        assert_eq!(http.sse_event_count, 2);
    }

    #[test]
    fn http2_ordinary_json_response_is_not_streaming() {
        let analyzer = Analyzer::new();
        let request_body =
            br#"{"model":"gpt-4o","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
        let stream = build_http2_stream(
            "/v1/chat/completions",
            request_body,
            br#"{"id":"non-sse","choices":[]}"#.to_vec(),
            "application/json",
        );

        let http = analyze_http2_record(&analyzer, stream);

        assert!(!http.is_sse);
        assert_eq!(http.first_output_timestamp_ns, None);
        assert_eq!(http.sse_event_count, 0);
    }

    #[test]
    fn test_extract_token_from_json_body_zero_tokens() {
        let analyzer = Analyzer::new();
        let json = serde_json::json!({
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
        });
        assert!(
            analyzer
                .extract_token_from_json_body(Some(&json), 1234, "test")
                .is_none()
        );
    }

    #[test]
    fn test_extract_token_from_sse_continuation_buffer() {
        let analyzer = Analyzer::new();
        let events = vec![create_test_event(
            "data: {\"type\":\"response.output_text.delta\"}",
        )];
        let continuation = br#"event:response.completed
data:{"usage":{"input_tokens":57,"output_tokens":3}}"#;
        let record = analyzer
            .extract_token_from_sse(&events, Some(continuation), 1234, "test")
            .expect("should recover from continuation buffer");
        assert_eq!(record.input_tokens, 57);
        assert_eq!(record.output_tokens, 3);
    }

    #[test]
    fn test_extract_token_from_sse_events_only_no_usage() {
        let analyzer = Analyzer::new();
        let events = vec![create_test_event(
            "data: {\"type\":\"response.output_text.delta\"}",
        )];
        assert!(
            analyzer
                .extract_token_from_sse(&events, None, 1234, "test")
                .is_none()
        );
    }

    #[test]
    fn test_extract_token_from_sse_continuation_buffer_scan_fallback() {
        let analyzer = Analyzer::new();
        let events = vec![create_test_event(
            "data: {\"type\":\"response.output_text.delta\"}",
        )];
        // Truncated JSON that is neither valid JSON nor well-formed SSE, but
        // still carries token fields. This exercises the regex-free scan
        // fallback (lines 633-642).
        let continuation = b"{\"input_tokens\":57,\"output_tokens\":3";
        let record = analyzer
            .extract_token_from_sse(&events, Some(continuation), 1234, "test")
            .expect("should recover via partial scan");
        assert_eq!(record.input_tokens, 57);
        assert_eq!(record.output_tokens, 3);
    }

    #[test]
    fn test_extract_token_from_sse_continuation_no_usage_returns_none() {
        // Continuation buffer with no parseable token data at all.
        let analyzer = Analyzer::new();
        let events = vec![create_test_event(
            "data: {\"type\":\"response.output_text.delta\"}",
        )];
        let continuation = b"event: response.completed\ndata: {\"id\":\"resp_001\"}\n\n";
        let result = analyzer.extract_token_from_sse(&events, Some(continuation), 1234, "test");
        assert!(result.is_none(), "should return None when no usage found");
    }

    #[test]
    fn test_extract_token_from_sse_anthropic_official_merges_start_and_delta() {
        // Dialect A: message_start carries input + cache, terminal message_delta
        // carries only output_tokens. The merge must combine both instead of
        // picking one event (which would drop output or drop input+cache).
        let analyzer = Analyzer::new();
        let events = vec![
            create_test_event(
                "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":1234,\"cache_creation_input_tokens\":5678,\"cache_read_input_tokens\":90,\"output_tokens\":1}}}",
            ),
            create_test_event(
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}",
            ),
        ];
        let record = analyzer
            .extract_token_from_sse(&events, None, 1234, "test")
            .expect("should merge start + delta");
        assert_eq!(record.input_tokens, 1234);
        assert_eq!(record.output_tokens, 42);
        assert_eq!(record.cache_creation_tokens, Some(5678));
        assert_eq!(record.cache_read_tokens, Some(90));
    }

    #[test]
    fn test_extract_token_from_sse_anthropic_no_message_start() {
        // Dialect B: proxy strips message_start entirely; only message_delta
        // (output-only) survives. Must still yield output_tokens rather than
        // being dropped by a mandatory input_tokens requirement.
        let analyzer = Analyzer::new();
        let events = vec![create_test_event(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}",
        )];
        let record = analyzer
            .extract_token_from_sse(&events, None, 1234, "test")
            .expect("output-only delta should still produce a record");
        assert_eq!(record.input_tokens, 0);
        assert_eq!(record.output_tokens, 42);
    }

    #[test]
    fn test_extract_token_from_sse_anthropic_zero_placeholder_start() {
        // Dialect C: proxy sends a zero-placeholder message_start followed by
        // the real output in message_delta. The zero start must not mask the
        // delta's output_tokens.
        let analyzer = Analyzer::new();
        let events = vec![
            create_test_event(
                "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}",
            ),
            create_test_event(
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}",
            ),
        ];
        let record = analyzer
            .extract_token_from_sse(&events, None, 1234, "test")
            .expect("zero placeholder must not mask delta output");
        assert_eq!(record.input_tokens, 0);
        assert_eq!(record.output_tokens, 42);
    }

    #[test]
    fn test_analyze_aggregated_response_only_with_sse() {
        use crate::aggregator::{AggregatedResponse, ConnectionId};
        use crate::parser::http::ParsedResponse;

        let analyzer = Analyzer::new();

        let resp_buf = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
        let ssl_event = Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1_000_000_000,
            delta_ns: 0,
            pid: 3000,
            tid: 3000,
            uid: 0,
            len: resp_buf.len() as u32,
            rw: 0,
            comm: "python3".to_string(),
            buf: resp_buf.to_vec(),
            is_handshake: false,
            ssl_ptr: 0x9000,
        });

        let parsed_response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers: std::collections::HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: ssl_event,
        };

        let mut aggregated_response = AggregatedResponse::from_parsed(parsed_response);
        let usage_event =
            create_test_event("data: {\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7}}");
        aggregated_response.set_sse_events(vec![usage_event]);

        let result = AggregatedResult::ResponseOnly {
            connection_id: ConnectionId {
                pid: 3000,
                ssl_ptr: 0x9000,
            },
            response: aggregated_response,
        };

        let results = analyzer.analyze_aggregated(&result);
        let token_result = results
            .iter()
            .find(|r| matches!(r, AnalysisResult::Token(_)));
        assert!(
            token_result.is_some(),
            "ResponseOnly with SSE usage should produce a TokenRecord"
        );
        if let Some(AnalysisResult::Token(record)) = token_result {
            assert_eq!(record.input_tokens, 42);
            assert_eq!(record.output_tokens, 7);
        }
    }
}
