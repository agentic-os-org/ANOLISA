//! CLI subcommand for ChatML token breakdown analysis from Chrome Trace
//!
//! Usage:
//! ```bash
//! agentsight analyze-chatml --chrome-trace <trace.json> [--model <name>] [--pretty]
//! ```

use serde_json::Value;
use structopt::StructOpt;

use crate::chrome_trace::ChromeTraceEvent;
use crate::tokenizer::get_global_tokenizer;

use super::breakdown::compute_breakdown;
use super::classifier::classify_document;
use super::lexer::parse_chatml;
use super::types::ResponseData;

/// Analyze ChatML token breakdown from Chrome Trace events
#[derive(Debug, StructOpt)]
pub struct AnalyzeChatmlCommand {
    /// Path to Chrome Trace file to read events from
    #[structopt(long = "chrome-trace", parse(from_os_str))]
    pub chrome_trace: std::path::PathBuf,

    /// Model name for tokenizer lookup (used with get_global_tokenizer)
    #[structopt(long, default_value = "qwen3.5-plus")]
    pub model: String,

    /// Pretty-print JSON output
    #[structopt(long)]
    pub pretty: bool,
}

impl AnalyzeChatmlCommand {
    pub fn execute(&self) {
        if let Err(e) = self.run() {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }

    fn run(&self) -> anyhow::Result<()> {
        // Read and parse Chrome Trace file
        let events = Self::parse_chrome_trace(&self.chrome_trace)?;
        // Process each request/response as independent events
        self.process_trace_events(&events)?;

        Ok(())
    }

    /// Process trace events - each http.request and http.response is an independent event
    fn process_trace_events(&self, events: &[ChromeTraceEvent]) -> anyhow::Result<()> {
        // Get global tokenizer for the specified model
        let tokenizer = get_global_tokenizer(&self.model).map_err(|e| {
            anyhow::anyhow!("tokenizer for model '{}' unavailable: {e}", self.model)
        })?;
        let chat_template = tokenizer.clone();

        // Sort events by timestamp to ensure correct order
        let mut sorted_events: Vec<ChromeTraceEvent> = events.to_vec();
        sorted_events.sort_by_key(|e| e.ts);

        // Process each event directly (no intermediate extraction)
        let mut breakdowns = Vec::new();

        for event in &sorted_events {
            let classified = match event.cat.as_str() {
                "http.request" => {
                    // Extract messages and tools from request body and process directly
                    if let Some(ref args) = event.args {
                        if let Some(body) = args.get("body") {
                            let (messages, tools) = if let Some(body_str) = body.as_str() {
                                serde_json::from_str::<serde_json::Value>(body_str)
                                    .ok()
                                    .map(|v| {
                                        let msgs = v.get("messages").cloned();
                                        let tools =
                                            v.get("tools").and_then(|t| t.as_array().cloned());
                                        (msgs, tools)
                                    })
                                    .unwrap_or((None, None))
                            } else {
                                let msgs = body.get("messages").cloned();
                                let tools = body.get("tools").and_then(|t| t.as_array().cloned());
                                (msgs, tools)
                            };

                            if let Some(mut msgs) = messages.and_then(|v| v.as_array().cloned()) {
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
                                                            serde_json::from_str::<Value>(args_str)
                                                        {
                                                            func["arguments"] = parsed;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                // Requests without a tools array are ordinary
                                // LLM traffic; the template accepts None and
                                // renders without tool definitions.
                                let chatml_text = chat_template.apply_chat_template_with_tools(
                                    &msgs,
                                    tools.as_deref(),
                                    false,
                                )?;
                                let doc = parse_chatml(&chatml_text)?;
                                Some(classify_document(&doc.blocks, None))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                "http.response" => {
                    // Extract response data from SSE events
                    if let Some(ref args) = event.args {
                        if let Some(sse_events) = args.get("sse_events").and_then(|v| v.as_array())
                        {
                            let response = Self::extract_response_from_sse(sse_events);
                            Some(classify_document(&[], Some(response)))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None, // Ignore other event types
            };

            if let Some(classified) = classified {
                let breakdown = compute_breakdown(&classified, &tokenizer)?;
                breakdowns.push(breakdown);
            }
        }

        if breakdowns.is_empty() {
            return Err(anyhow::anyhow!(
                "No valid http.request or http.response events found in Chrome Trace."
            ));
        }

        // Output JSON array of all breakdowns
        let json = if self.pretty {
            serde_json::to_string_pretty(&breakdowns)?
        } else {
            serde_json::to_string(&breakdowns)?
        };
        println!("{}", json);

        Ok(())
    }

    /// Parse Chrome Trace file and return list of events
    fn parse_chrome_trace(path: &std::path::Path) -> anyhow::Result<Vec<ChromeTraceEvent>> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read Chrome Trace file '{}': {}",
                path.display(),
                e
            )
        })?;

        // Chrome trace files are JSON arrays, but may have trailing commas
        // Try standard JSON array parsing first
        match serde_json::from_str::<Vec<ChromeTraceEvent>>(&content) {
            Ok(events) => Ok(events),
            Err(e) => {
                // Try to parse with relaxed format (handle trailing commas)
                Self::parse_trace_relaxed(&content).map_err(|_| {
                    anyhow::anyhow!(
                        "Failed to parse Chrome Trace file '{}': {}",
                        path.display(),
                        e
                    )
                })
            }
        }
    }

    /// Parse Chrome Trace file with relaxed format (handle trailing commas)
    fn parse_trace_relaxed(content: &str) -> anyhow::Result<Vec<ChromeTraceEvent>> {
        // Remove trailing commas before ] to handle non-standard JSON
        let cleaned = content
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim();

        if cleaned.is_empty() {
            return Ok(Vec::new());
        }

        // Split by lines and parse each event
        let mut events = Vec::new();
        for line in cleaned.lines() {
            let line = line.trim().trim_end_matches(',');
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ChromeTraceEvent>(line) {
                Ok(event) => events.push(event),
                Err(e) => {
                    eprintln!("Warning: Failed to parse trace event: {}", e);
                }
            }
        }

        Ok(events)
    }

    /// Extract response data from SSE events array
    ///
    /// The chrome trace stores the raw `data` payload of every SSE event
    /// verbatim, so the shape depends on the provider the captured call
    /// spoke to: OpenAI-compatible `choices[].delta`, the Anthropic
    /// `content_block_*` events, or the OpenAI Responses `response.*`
    /// events. All three shapes are aggregated; a stream answers in exactly
    /// one of them, so the accumulators never mix in practice.
    fn extract_response_from_sse(sse_events: &[serde_json::Value]) -> ResponseData {
        let mut content_parts = Vec::new();
        let mut reasoning_parts = Vec::new();
        // OpenAI-compatible streams deliver each tool call across deltas
        // keyed by `index`: the function name arrives once (usually in the
        // first fragment) and the arguments stream as string fragments that
        // must be concatenated per index before rendering "name: arguments".
        let mut tool_calls: Vec<(usize, String, String)> = Vec::new();
        // Anthropic tool_use blocks, keyed by content-block index: the id and
        // name arrive in `content_block_start`, the arguments stream as
        // `input_json_delta` fragments.
        let mut anthropic_calls: std::collections::BTreeMap<u64, (String, String, String)> =
            std::collections::BTreeMap::new();
        // Responses API: one function call in flight at a time (parallel calls
        // are flushed when the next one starts, matching the analyzer's
        // aggregator).
        let mut responses_call: Option<(String, String, String)> = None;
        let mut responses_calls: Vec<String> = Vec::new();

        for event in sse_events {
            // Parse the data field which contains JSON string
            if let Some(data_str) = event.get("data").and_then(|v| v.as_str()) {
                // Skip [DONE] marker
                if data_str == "[DONE]" {
                    continue;
                }

                // Parse the JSON data
                if let Ok(data_json) = serde_json::from_str::<serde_json::Value>(data_str) {
                    let event_type = data_json.get("type").and_then(|v| v.as_str());
                    match event_type {
                        // ── Anthropic streaming shapes ────────────────────
                        Some("content_block_start") => {
                            if let Some(block) = data_json.get("content_block") {
                                if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                                    let index = data_json
                                        .get("index")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    anthropic_calls.entry(index).or_insert_with(|| {
                                        (
                                            block
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or_default()
                                                .to_string(),
                                            block
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or_default()
                                                .to_string(),
                                            String::new(),
                                        )
                                    });
                                }
                            }
                        }
                        Some("content_block_delta") => {
                            let index =
                                data_json.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                            if let Some(delta) = data_json.get("delta") {
                                match delta.get("type").and_then(|v| v.as_str()) {
                                    Some("text_delta") => {
                                        if let Some(text) =
                                            delta.get("text").and_then(|v| v.as_str())
                                        {
                                            if !text.is_empty() {
                                                content_parts.push(text.to_string());
                                            }
                                        }
                                    }
                                    Some("thinking_delta") => {
                                        if let Some(text) =
                                            delta.get("thinking").and_then(|v| v.as_str())
                                        {
                                            if !text.is_empty() {
                                                reasoning_parts.push(text.to_string());
                                            }
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(fragment) =
                                            delta.get("partial_json").and_then(|v| v.as_str())
                                        {
                                            if let Some((_, _, args)) =
                                                anthropic_calls.get_mut(&index)
                                            {
                                                args.push_str(fragment);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        // ── OpenAI Responses shapes ───────────────────────
                        Some("response.output_text.delta") => {
                            if let Some(delta) = data_json.get("delta").and_then(|v| v.as_str()) {
                                if !delta.is_empty() {
                                    content_parts.push(delta.to_string());
                                }
                            }
                        }
                        Some("response.output_item.added") => {
                            if let Some(item) = data_json.get("item") {
                                if item.get("type").and_then(|v| v.as_str())
                                    == Some("function_call")
                                {
                                    // Parallel tool use: flush the in-flight call
                                    // before starting the next.
                                    if let Some((_, name, args)) = responses_call.take() {
                                        if !name.is_empty() || !args.is_empty() {
                                            responses_calls.push(format!("{name}: {args}"));
                                        }
                                    }
                                    responses_call = Some((
                                        item.get("call_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default()
                                            .to_string(),
                                        item.get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default()
                                            .to_string(),
                                        String::new(),
                                    ));
                                }
                            }
                        }
                        Some("response.function_call_arguments.delta") => {
                            if let Some(delta) = data_json.get("delta").and_then(|v| v.as_str()) {
                                if let Some((_, _, args)) = responses_call.as_mut() {
                                    args.push_str(delta);
                                }
                            }
                        }
                        Some("response.function_call_arguments.done") => {
                            if let Some((_, name, args)) = responses_call.take() {
                                if !name.is_empty() || !args.is_empty() {
                                    responses_calls.push(format!("{name}: {args}"));
                                }
                            }
                        }
                        _ => {}
                    }

                    // Extract content and reasoning_content from choices[].delta
                    if let Some(choices) = data_json.get("choices").and_then(|v| v.as_array()) {
                        for choice in choices {
                            if let Some(delta) = choice.get("delta") {
                                // Extract content
                                if let Some(content) = delta.get("content").and_then(|v| v.as_str())
                                {
                                    if !content.is_empty() {
                                        content_parts.push(content.to_string());
                                    }
                                }
                                // Extract reasoning_content
                                if let Some(reasoning) =
                                    delta.get("reasoning_content").and_then(|v| v.as_str())
                                {
                                    if !reasoning.is_empty() {
                                        reasoning_parts.push(reasoning.to_string());
                                    }
                                }
                                // Extract tool_calls - merge function name and
                                // streamed arguments fragments by index
                                if let Some(calls) =
                                    delta.get("tool_calls").and_then(|t| t.as_array())
                                {
                                    for call in calls {
                                        let index =
                                            call.get("index").and_then(|i| i.as_u64()).unwrap_or(0)
                                                as usize;
                                        let function = call.get("function");
                                        let name = function
                                            .and_then(|f| f.get("name"))
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("");
                                        let arguments = function
                                            .and_then(|f| f.get("arguments"))
                                            .and_then(|a| a.as_str())
                                            .unwrap_or("");
                                        match tool_calls.iter_mut().find(|(i, _, _)| *i == index) {
                                            Some((_, slot_name, slot_arguments)) => {
                                                if !name.is_empty() {
                                                    *slot_name = name.to_string();
                                                }
                                                slot_arguments.push_str(arguments);
                                            }
                                            None => {
                                                tool_calls.push((
                                                    index,
                                                    name.to_string(),
                                                    arguments.to_string(),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut tool_calls: Vec<String> = {
            let mut calls = tool_calls;
            calls.sort_by_key(|(index, _, _)| *index);
            calls
                .into_iter()
                .filter(|(_, name, arguments)| !name.is_empty() || !arguments.is_empty())
                .map(|(_, name, arguments)| format!("{name}: {arguments}"))
                .collect()
        };

        // Anthropic tool_use blocks, in content-block order.
        for (_, (_, name, args)) in anthropic_calls {
            if !name.is_empty() || !args.is_empty() {
                tool_calls.push(format!("{name}: {args}"));
            }
        }

        // Responses calls in stream order, then a still-in-flight call
        // (truncated stream without the done event).
        tool_calls.extend(responses_calls);
        if let Some((_, name, args)) = responses_call {
            if !name.is_empty() || !args.is_empty() {
                tool_calls.push(format!("{name}: {args}"));
            }
        }

        ResponseData {
            content: content_parts,
            reasoning_content: if reasoning_parts.is_empty() {
                None
            } else {
                Some(reasoning_parts.join(""))
            },
            tool_calls,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sse(payload: &str) -> serde_json::Value {
        json!({ "data": payload })
    }

    #[test]
    fn sse_tool_call_delta_is_extracted() {
        let events = vec![
            sse(r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#),
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_0","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Beijing\"}"}}]}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec![r#"get_weather: {"city":"Beijing"}"#.to_string()]
        );
    }

    #[test]
    fn sse_tool_call_arguments_fragments_merge_by_index() {
        let events = vec![
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Beijing\"}"}}]}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec![r#"get_weather: {"city":"Beijing"}"#.to_string()]
        );
    }

    #[test]
    fn sse_multiple_tool_calls_keep_index_order() {
        let events = vec![
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"name":"second_tool","arguments":"{}"}}]}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"first_tool","arguments":"{}"}}]}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec!["first_tool: {}".to_string(), "second_tool: {}".to_string()]
        );
    }

    #[test]
    fn sse_content_and_reasoning_unchanged_alongside_tool_calls() {
        let events = vec![
            sse(
                r#"{"choices":[{"index":0,"delta":{"reasoning_content":"thinking","content":"hi "}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":0,"delta":{"content":"there","tool_calls":[{"index":0,"function":{"name":"noop","arguments":""}}]}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["hi ".to_string(), "there".to_string()]);
        assert_eq!(resp.reasoning_content.as_deref(), Some("thinking"));
        assert_eq!(resp.tool_calls, vec!["noop: ".to_string()]);
    }

    /// The chrome trace stores the raw SSE `data` payloads verbatim, so an
    /// Anthropic trace carries `content_block_*` events. Before the protocol
    /// shapes were aggregated, such a response broke down as completely
    /// empty: zero content, zero reasoning, zero tool calls — silently.
    #[test]
    fn sse_anthropic_stream_is_extracted() {
        let events = vec![
            sse(
                r#"{"type":"message_start","message":{"id":"msg_1","role":"assistant","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            ),
            sse(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            sse(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"pondering"}}"#,
            ),
            sse(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            ),
            sse(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}"#,
            ),
            sse(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}"#,
            ),
            sse(
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
            ),
            sse(
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#,
            ),
            sse(
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"Beijing\"}"}}"#,
            ),
            sse(
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.content,
            vec!["Hel".to_string(), "lo".to_string()],
            "text deltas must be extracted"
        );
        assert_eq!(resp.reasoning_content.as_deref(), Some("pondering"));
        assert_eq!(
            resp.tool_calls,
            vec![r#"get_weather: {"city":"Beijing"}"#.to_string()],
            "input_json_delta fragments must concatenate into the arguments"
        );
    }

    /// Same story for the OpenAI Responses protocol (codex 0.137+ via
    /// /v1/responses): its `response.*` events previously produced an empty
    /// breakdown.
    #[test]
    fn sse_responses_stream_is_extracted() {
        let events = vec![
            sse(r#"{"type":"response.created","response":{"id":"resp_1"}}"#),
            sse(r#"{"type":"response.output_text.delta","delta":"Hel"}"#),
            sse(r#"{"type":"response.output_text.delta","delta":"lo"}"#),
            sse(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"read_file"}}"#,
            ),
            sse(r#"{"type":"response.function_call_arguments.delta","delta":"{\"path\":"}"#),
            sse(r#"{"type":"response.function_call_arguments.delta","delta":"\"/tmp/a.md\"}"}"#),
            sse(
                r#"{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(resp.reasoning_content, None);
        assert_eq!(
            resp.tool_calls,
            vec![r#"read_file: {"path":"/tmp/a.md"}"#.to_string()]
        );
    }

    /// Parallel Responses calls without per-call done events must all
    /// survive, in stream order.
    #[test]
    fn sse_responses_parallel_tool_calls_survive_without_done() {
        let events = vec![
            sse(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"first_tool"}}"#,
            ),
            sse(r#"{"type":"response.function_call_arguments.delta","delta":"{}"}"#),
            sse(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_2","name":"second_tool"}}"#,
            ),
            sse(r#"{"type":"response.function_call_arguments.delta","delta":"{}"}"#),
            sse(r#"{"type":"response.completed","response":{}}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec!["first_tool: {}".to_string(), "second_tool: {}".to_string()]
        );
    }
}
