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
                            if let Some((mut msgs, tools)) = Self::request_body_messages(body) {
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

    /// Normalize a captured request body into the message list the chat
    /// template consumes, plus the tools array.
    ///
    /// The body is stored either as a JSON string (the trace writer's
    /// fallback for non-JSON bodies) or as the parsed object. The message
    /// list itself comes from the same protocol shapes the genai request
    /// parser understands (`GenAIBuilder::extract_messages_view`): a plain
    /// `messages` array, the OpenAI Responses `input` array with its
    /// `instructions`, or an Anthropic `messages` array with the system
    /// prompt in the top-level `system` field. Without this, a Responses
    /// request event was silently skipped (no request breakdown at all) and
    /// an Anthropic request's system prompt vanished from the breakdown.
    /// The out-of-band system text is prepended as a system message so the
    /// template renders it.
    fn request_body_messages(
        body: &serde_json::Value,
    ) -> Option<(Vec<serde_json::Value>, Option<Vec<serde_json::Value>>)> {
        let parsed: Option<serde_json::Value> = match body {
            serde_json::Value::String(s) => serde_json::from_str(s).ok(),
            obj @ serde_json::Value::Object(_) => Some(obj.clone()),
            _ => None,
        };
        let body = parsed.as_ref()?;

        let tools = body.get("tools").and_then(|t| t.as_array().cloned());

        let (mut msgs, system_text) = crate::genai::GenAIBuilder::extract_messages_view(body)?;
        if let Some(system) = system_text {
            if !system.is_empty() {
                msgs.insert(0, serde_json::json!({"role": "system", "content": system}));
            }
        }
        Some((msgs, tools))
    }

    /// Extract response data from SSE events array
    fn extract_response_from_sse(sse_events: &[serde_json::Value]) -> ResponseData {
        let mut content_parts = Vec::new();
        let mut reasoning_parts = Vec::new();
        // OpenAI-compatible streams deliver each tool call across deltas
        // keyed by `index`: the function name arrives once (usually in the
        // first fragment) and the arguments stream as string fragments that
        // must be concatenated per index before rendering "name: arguments".
        let mut tool_calls: Vec<(usize, String, String)> = Vec::new();

        for event in sse_events {
            // Parse the data field which contains JSON string
            if let Some(data_str) = event.get("data").and_then(|v| v.as_str()) {
                // Skip [DONE] marker
                if data_str == "[DONE]" {
                    continue;
                }

                // Parse the JSON data
                if let Ok(data_json) = serde_json::from_str::<serde_json::Value>(data_str) {
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

        let tool_calls: Vec<String> = {
            let mut calls = tool_calls;
            calls.sort_by_key(|(index, _, _)| *index);
            calls
                .into_iter()
                .filter(|(_, name, arguments)| !name.is_empty() || !arguments.is_empty())
                .map(|(_, name, arguments)| format!("{name}: {arguments}"))
                .collect()
        };

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

    /// The chrome trace stores the request body either as the parsed JSON
    /// object or as its string form; both must yield the same messages.
    #[test]
    fn request_messages_accepts_string_and_object_bodies() {
        let object = json!({
            "model": "qwen3.5-plus",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "noop"}}],
        });
        let (msgs, tools) =
            AnalyzeChatmlCommand::request_body_messages(&object).expect("object body parses");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(
            tools.as_ref().expect("tools survive").len(),
            1,
            "tools survive"
        );

        let string = serde_json::Value::String(object.to_string());
        let (msgs2, tools2) =
            AnalyzeChatmlCommand::request_body_messages(&string).expect("string body parses");
        assert_eq!(msgs2, msgs);
        assert_eq!(tools2, tools);
    }

    /// An OpenAI Responses request (codex 0.137+ via /v1/responses) carries
    /// `input` + `instructions` instead of `messages`. The old arm read only
    /// `messages`, so such request events were silently skipped — no request
    /// breakdown at all for a codex trace.
    #[test]
    fn request_messages_reads_responses_api_input() {
        let body = json!({
            "model": "qwen3-coder-plus",
            "instructions": "Be terse.",
            "input": [
                {"type": "message", "role": "user", "content": "list the files"},
            ],
        });
        let (msgs, tools) =
            AnalyzeChatmlCommand::request_body_messages(&body).expect("responses body parses");
        assert_eq!(tools, None);
        assert_eq!(msgs.len(), 2, "instructions prepend a system message");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be terse.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "list the files");
    }

    /// An Anthropic request carries the system prompt in the top-level
    /// `system` field, outside the messages array. The old arm read only the
    /// `messages` array, so the system prompt vanished from the request
    /// breakdown.
    #[test]
    fn request_messages_keeps_anthropic_system_prompt() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "hi"}],
        });
        let (msgs, _) =
            AnalyzeChatmlCommand::request_body_messages(&body).expect("anthropic body parses");
        assert_eq!(msgs.len(), 2, "the system prompt is prepended");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are a helpful assistant.");
        assert_eq!(msgs[1]["role"], "user");

        // Anthropic's system field may also be an array of text blocks.
        let body = json!({
            "system": [{"type": "text", "text": "First."}, {"type": "text", "text": "Second."}],
            "messages": [{"role": "user", "content": "hi"}],
        });
        let (msgs, _) =
            AnalyzeChatmlCommand::request_body_messages(&body).expect("block system parses");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "First.\nSecond.");
    }

    /// Bodies without any known message shape (e.g. a GET with no body, or a
    /// non-LLM JSON body) stay skipped, and an OpenAI body with no top-level
    /// system field gets no synthetic system message.
    #[test]
    fn request_messages_skips_unknown_shapes_and_adds_no_system() {
        assert!(AnalyzeChatmlCommand::request_body_messages(&json!({"foo": 1})).is_none());
        assert!(
            AnalyzeChatmlCommand::request_body_messages(&serde_json::Value::String(
                "not json at all".to_string()
            ))
            .is_none()
        );

        let plain = json!({"messages": [{"role": "user", "content": "hi"}]});
        let (msgs, tools) =
            AnalyzeChatmlCommand::request_body_messages(&plain).expect("plain body parses");
        assert_eq!(msgs.len(), 1, "no synthetic system message");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(tools, None);
    }
}
