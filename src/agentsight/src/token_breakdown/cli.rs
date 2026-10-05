//! CLI subcommand for ChatML token breakdown analysis from Chrome Trace
//!
//! Usage:
//! ```bash
//! agentsight analyze-chatml --chrome-trace <trace.json> [--model <name>] [--pretty]
//! ```

use serde_json::Value;
use structopt::StructOpt;

use crate::chrome_trace::ChromeTraceEvent;
use crate::tokenizer::{LlmTokenizer, get_global_tokenizer};

use super::breakdown::compute_breakdown;
use super::classifier::classify_document;
use super::lexer::parse_chatml;
use super::types::{ChatMLTokenBreakdown, ResponseData};

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

        // Sort events by timestamp to ensure correct order
        let mut sorted_events: Vec<ChromeTraceEvent> = events.to_vec();
        sorted_events.sort_by_key(|e| e.ts);

        let breakdowns = Self::process_events(&sorted_events, &tokenizer, &self.model)?;

        // Output JSON array of all breakdowns
        let json = if self.pretty {
            serde_json::to_string_pretty(&breakdowns)?
        } else {
            serde_json::to_string(&breakdowns)?
        };
        println!("{}", json);

        Ok(())
    }

    /// Process each request/response event independently.
    fn process_events(
        events: &[ChromeTraceEvent],
        tokenizer: &LlmTokenizer,
        model_name: &str,
    ) -> anyhow::Result<Vec<ChatMLTokenBreakdown>> {
        let chat_template = tokenizer.clone();

        // Process each event directly (no intermediate extraction)
        let mut breakdowns = Vec::new();

        for event in events {
            let classified = match event.cat.as_str() {
                "http.request" => {
                    // Extract messages and tools from request body and process directly
                    if let Some(ref args) = event.args {
                        if let Some(body) = args.get("body") {
                            // An empty message list has nothing to render; the
                            // same skip the analyzer applies.
                            if let Some((mut msgs, tools)) = Self::request_body_messages(body)
                                .filter(|(msgs, _)| !msgs.is_empty())
                            {
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
                                //
                                // A single event that cannot be rendered or
                                // parsed is reported and skipped, the same
                                // policy parse_trace_relaxed uses: one
                                // malformed request must not discard every
                                // other event of the trace.
                                match chat_template
                                    .apply_chat_template_with_tools(&msgs, tools.as_deref(), false)
                                    .and_then(|chatml_text| parse_chatml(&chatml_text))
                                {
                                    Ok(doc) => Some(classify_document(&doc.blocks, None)),
                                    Err(e) => {
                                        eprintln!("Warning: skipping http.request event: {e}");
                                        None
                                    }
                                }
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
                let breakdown = compute_breakdown(&classified, tokenizer, model_name)?;
                breakdowns.push(breakdown);
            }
        }

        if breakdowns.is_empty() {
            return Err(anyhow::anyhow!(
                "No valid http.request or http.response events found in Chrome Trace."
            ));
        }

        Ok(breakdowns)
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
        // Trailing commas before a closing bracket are the one deviation
        // serde_json cannot read (several trace exporters emit them). Strip
        // them and parse the array as a whole: that also covers
        // pretty-printed (multi-line) traces, which the previous line-by-line
        // fallback could not read at all — every line of a multi-line event
        // failed to parse on its own, so a pretty-printed trace answered
        // "no valid events found" even though every event was present.
        let cleaned = strip_trailing_commas(content);
        // Parse the array element by element: one event with missing required
        // fields — Chrome DevTools' metadata events (`ph: "M"`) carry no `ts`
        // by definition — must not fail the whole file, or a complete
        // pretty-printed trace answers "no valid events found".
        if let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(&cleaned) {
            let mut events = Vec::new();
            for value in values {
                match serde_json::from_value::<ChromeTraceEvent>(value) {
                    Ok(event) => events.push(event),
                    Err(e) => {
                        eprintln!("Warning: Failed to parse trace event: {e}");
                    }
                }
            }
            return Ok(events);
        }

        // Last resort: one event per line, for traces that are neither a
        // valid array nor multi-line pretty-printed.
        let cleaned = cleaned
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
    /// list itself comes from the same protocol shapes the parser-layer
    /// request view understands (`parser::llm::extract_messages_view`): a plain
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

        let (mut msgs, system_text) = crate::parser::llm::extract_messages_view(body)?;
        if let Some(system) = system_text {
            if !system.is_empty() {
                msgs.insert(0, serde_json::json!({"role": "system", "content": system}));
            }
        }
        Some((msgs, tools))
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
        // Responses API: the shared aggregator routes each argument event to
        // its own output item by id/index and honors a full `done` payload,
        // matching the live analyzer instead of assuming one call in flight.
        let mut responses_tool_calls = crate::analyzer::message::ResponsesToolCalls::default();

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
                        // Reasoning models stream their thinking on the
                        // same channel (dashscope qwen3-coder sends
                        // reasoning_text, the o-series summary_text); both
                        // belong in reasoning_content, like the chat-completions
                        // reasoning_content delta and the analyzer's Responses
                        // aggregation.
                        Some("response.reasoning_text.delta")
                        | Some("response.reasoning_summary_text.delta") => {
                            if let Some(delta) = data_json.get("delta").and_then(|v| v.as_str()) {
                                if !delta.is_empty() {
                                    reasoning_parts.push(delta.to_string());
                                }
                            }
                        }
                        Some("response.output_item.added")
                        | Some("response.function_call_arguments.delta")
                        | Some("response.function_call_arguments.done") => {
                            responses_tool_calls.observe(&data_json);
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

        // Responses calls in output-item arrival order. `into_calls` also
        // emits a call whose stream ended before its done event.
        for (_, name, arguments) in responses_tool_calls.into_calls() {
            tool_calls.push(format!("{name}: {arguments}"));
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

/// Remove commas that precede only whitespace and a closing bracket, so a
/// trace with trailing commas becomes valid JSON serde can parse. String
/// literals (and escaped characters inside them) are respected, so a comma
/// inside a quoted value survives.
fn strip_trailing_commas(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            let ch = content[i..].chars().next().expect("char boundary");
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                out.push(b as char);
                i += 1;
            }
            b',' => {
                // Drop the comma when only whitespace separates it from a
                // closing bracket (an object or array close).
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] as char).is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b']' || bytes[j] == b'}') {
                    i += 1; // skip the comma
                } else {
                    out.push(',');
                    i += 1;
                }
            }
            _ => {
                // Copy the (possibly multi-byte) character verbatim.
                let ch = content[i..].chars().next().expect("char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::LlmTokenizer;
    use serde_json::json;

    /// Minimal HuggingFace tokenizer (WordLevel + Whitespace) so the ChatML
    /// path can be exercised in tests without the network or the real Qwen
    /// tokenizer, which is not vendored in the repository.
    const TOKENIZER_JSON: &str = r#"{
  "version": "1.0",
  "truncation": null,
  "padding": null,
  "added_tokens": [
    {"id": 0, "content": "<|im_start|>", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true},
    {"id": 1, "content": "<|im_end|>", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true},
    {"id": 2, "content": "[UNK]", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true}
  ],
  "normalizer": null,
  "pre_tokenizer": {"type": "Whitespace"},
  "post_processor": null,
  "decoder": null,
  "model": {
    "type": "WordLevel",
    "vocab": {
      "<|im_start|>": 0,
      "<|im_end|>": 1,
      "[UNK]": 2,
      "system": 3,
      "user": 4,
      "assistant": 5,
      "hello": 6,
      "there": 7
    },
    "unk_token": "[UNK]"
  }
}"#;

    /// ChatML template in the shape the Qwen models use: `+` concatenation of
    /// `role` and `content`, which fails on a non-string `content` exactly like
    /// the real template does.
    const TOKENIZER_CONFIG_JSON: &str = r#"{
  "tokenizer_class": "PreTrainedTokenizerFast",
  "chat_template": "{% for message in messages %}{{ '<|im_start|>' + message['role'] + '\n' + message['content'] + '<|im_end|>' + '\n' }}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant\n' }}{% endif %}",
  "bos_token": "<|im_start|>",
  "eos_token": "<|im_end|>",
  "unk_token": "[UNK]",
  "model_max_length": 32768
}"#;

    fn fixture_tokenizer() -> LlmTokenizer {
        let dir =
            std::env::temp_dir().join(format!("agentsight-chatml-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let tokenizer_path = dir.join("tokenizer.json");
        let config_path = dir.join("tokenizer_config.json");
        std::fs::write(&tokenizer_path, TOKENIZER_JSON).expect("write tokenizer.json");
        std::fs::write(&config_path, TOKENIZER_CONFIG_JSON).expect("write tokenizer_config.json");
        LlmTokenizer::from_file(&tokenizer_path, &config_path).expect("fixture tokenizer loads")
    }

    #[test]
    fn minimal_fixture_tokenizer_loads() {
        let tokenizer = fixture_tokenizer();
        let rendered = tokenizer
            .apply_chat_template_with_tools(
                &[json!({"role": "user", "content": "hello"})],
                None,
                false,
            )
            .expect("fixture template renders");
        assert_eq!(rendered, "<|im_start|>user\nhello<|im_end|>\n");
    }

    #[test]
    fn tool_definition_tokens_are_counted_without_tool_messages() {
        // The first request of a tool-using agent carries `tools` but no
        // `role: "tool"` message yet. `tools_tokens` is documented as the
        // tool *definition* count, so it must not read 0 here.
        let tokenizer = fixture_tokenizer();
        let request = json!({
            "messages": [{"role": "user", "content": "list the files"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "list_dir",
                    "description": "List the entries of a directory",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            }]
        });

        let count = crate::analyzer::count_request_tokens(&request, &tokenizer, &tokenizer)
            .expect("request is counted");
        assert!(
            count.tools_tokens > 0,
            "tool definitions must be reported, got {count:?}"
        );

        // A tool-role message must not be reported as tool definitions: the
        // field keeps the definition count, not the tool-message share.
        let with_tool_message = json!({
            "messages": [
                {"role": "user", "content": "list the files"},
                {"role": "tool", "tool_call_id": "tc1", "content": "a.txt b.txt"}
            ],
            "tools": request["tools"].clone()
        });
        let other =
            crate::analyzer::count_request_tokens(&with_tool_message, &tokenizer, &tokenizer)
                .expect("request is counted");
        assert_eq!(other.tools_tokens, count.tools_tokens);
    }

    #[test]
    fn breakdown_reports_the_model_the_command_was_given() {
        // `ChatMLTokenBreakdown::model_name` is documented as the model name
        // used for tokenization. It used to come from the tokenizer's own
        // name, which is the file it was loaded from: every auto-downloaded
        // tokenizer is `tokenizer.json`, so the output reported the literal
        // string "tokenizer" instead of the requested model.
        let tokenizer = fixture_tokenizer();
        let blocks = vec![crate::token_breakdown::types::ChatMLBlock {
            role: "user".to_string(),
            raw_content: "hello".to_string(),
        }];
        let doc = classify_document(&blocks, None);

        let breakdown =
            compute_breakdown(&doc, &tokenizer, "qwen3.5-plus").expect("breakdown computes");
        assert_eq!(
            breakdown.model_name, "qwen3.5-plus",
            "the output must name the model the command was given"
        );
    }

    fn request_event(body: serde_json::Value, ts: u64) -> ChromeTraceEvent {
        let mut event = ChromeTraceEvent::instant("http.request", "http.request", 1, 1, ts);
        event.args = Some(json!({ "body": body }));
        event
    }

    #[test]
    fn one_bad_request_does_not_discard_the_other_events() {
        let tokenizer = fixture_tokenizer();
        let events = vec![
            request_event(
                json!({"messages": [{"role": "user", "content": "hello"}]}),
                3,
            ),
            // Content as a parts array: the ChatML template concatenates the
            // content with `+`, which fails on a sequence (the shape real
            // traces hit with multimodal requests).
            request_event(
                json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}]}),
                2,
            ),
            // Nothing to render.
            request_event(json!({"messages": []}), 1),
        ];

        let breakdowns = AnalyzeChatmlCommand::process_events(&events, &tokenizer, "test-model")
            .expect("a malformed event must not abort the other events");
        assert_eq!(breakdowns.len(), 1);
    }

    #[test]
    fn all_failed_events_still_report_no_valid_events() {
        let tokenizer = fixture_tokenizer();
        let events = vec![request_event(
            json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}]}),
            1,
        )];

        let err = AnalyzeChatmlCommand::process_events(&events, &tokenizer, "test-model")
            .expect_err("every event failed to render");
        assert!(
            err.to_string()
                .contains("No valid http.request or http.response events found"),
            "unexpected error: {err}"
        );
    }

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

    /// A stream that only sends `output_item.added` plus
    /// `function_call_arguments.done` carries the arguments in the done
    /// payload; ignoring them recorded the call with empty arguments.
    #[test]
    fn sse_responses_done_payload_is_used() {
        let events = vec![
            sse(
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"tool_a","arguments":""}}"#,
            ),
            sse(
                r#"{"type":"response.function_call_arguments.done","output_index":0,"item_id":"fc_a","arguments":"{\"a\":1}"}"#,
            ),
            sse(r#"{"type":"response.completed","response":{}}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.tool_calls, vec!["tool_a: {\"a\":1}".to_string()]);
    }

    /// Parallel Responses calls interleave their argument deltas; keying only
    /// on "the most recent call" attached every delta to the last-started call
    /// and finalized the wrong one.
    #[test]
    fn sse_responses_interleaved_deltas_keep_their_call() {
        let events = vec![
            sse(
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"tool_a","arguments":""}}"#,
            ),
            sse(
                r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"tool_b","arguments":""}}"#,
            ),
            sse(
                r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"fc_a","delta":"{\"a\":1}"}"#,
            ),
            sse(
                r#"{"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_b","delta":"{\"b\":2}"}"#,
            ),
            sse(r#"{"type":"response.completed","response":{}}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec![
                "tool_a: {\"a\":1}".to_string(),
                "tool_b: {\"b\":2}".to_string()
            ]
        );
    }

    /// Reasoning models on the Responses API stream their thinking as
    /// `response.reasoning_text.delta` (dashscope qwen3-coder) or
    /// `response.reasoning_summary_text.delta` (the o-series); the analyzer
    /// keeps both, but the trace breakdown dropped them, reporting no
    /// reasoning at all for a stream that had one.
    #[test]
    fn sse_responses_reasoning_deltas_are_kept() {
        let events = vec![
            sse(r#"{"type":"response.output_item.added","item":{"type":"reasoning","id":"rs_1"}}"#),
            sse(r#"{"type":"response.reasoning_text.delta","delta":"Think "}"#),
            sse(r#"{"type":"response.reasoning_text.delta","delta":"hard."}"#),
            sse(r#"{"type":"response.output_text.delta","delta":"Hello"}"#),
            sse(r#"{"type":"response.completed","response":{}}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["Hello".to_string()]);
        assert_eq!(resp.reasoning_content.as_deref(), Some("Think hard."));
    }

    /// The o-series spelling (`reasoning_summary_text.delta`) carries the
    /// same reasoning and must reach `reasoning_content` too.
    #[test]
    fn sse_responses_reasoning_summary_deltas_are_kept() {
        let events = vec![
            sse(r#"{"type":"response.reasoning_summary_text.delta","delta":"summar"}"#),
            sse(r#"{"type":"response.reasoning_summary_text.delta","delta":"izing"}"#),
            sse(r#"{"type":"response.completed","response":{}}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.reasoning_content.as_deref(), Some("summarizing"));
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

    /// A pretty-printed trace (one event spread over multiple lines) with a
    /// trailing comma: the relaxed parser used to split by lines, so every
    /// line of a multi-line event failed to parse on its own and the command
    /// answered "no valid events found" even though every event was present.
    #[test]
    fn parse_trace_relaxed_reads_pretty_printed_traces() {
        let content = "[\n{\n  \"ph\": \"X\",\n  \"name\": \"POST /v1/messages\",\n  \"cat\": \"http.request\",\n  \"ts\": 100,\n  \"dur\": 50,\n  \"pid\": 1,\n  \"tid\": 2,\n  \"args\": {\"body\": \"{\\\"messages\\\":[]}\"}\n},\n{\n  \"ph\": \"X\",\n  \"name\": \"200 OK\",\n  \"cat\": \"http.response\",\n  \"ts\": 200,\n  \"dur\": 50,\n  \"pid\": 1,\n  \"tid\": 2\n},\n]\n";
        let events = AnalyzeChatmlCommand::parse_trace_relaxed(content)
            .expect("pretty-printed trace with trailing comma parses");
        assert_eq!(events.len(), 2, "both events must survive");
        assert_eq!(events[0].cat, "http.request");
        assert_eq!(
            events[0].args.as_ref().unwrap()["body"],
            "{\"messages\":[]}"
        );
        assert_eq!(events[1].cat, "http.response");
    }

    /// Chrome DevTools traces carry metadata events (`ph: "M"`); per the trace
    /// event format they have no timestamp. One such event used to fail the
    /// whole-array parse, and the line fallback cannot read pretty-printed
    /// traces, so a complete trace answered "no valid events found".
    #[test]
    fn parse_trace_relaxed_skips_events_with_missing_fields() {
        let content = concat!(
            "[\n",
            "{\n  \"args\": {\"name\": \"Browser\"},\n  \"cat\": \"__metadata\",\n  \"name\": \"process_name\",\n  \"ph\": \"M\",\n  \"pid\": 1,\n  \"tid\": 1\n},\n",
            "{\n  \"ph\": \"X\",\n  \"name\": \"POST /v1/messages\",\n  \"cat\": \"http.request\",\n  \"ts\": 100,\n  \"dur\": 50,\n  \"pid\": 1,\n  \"tid\": 2,\n  \"args\": {\"body\": \"{\\\"messages\\\":[]}\"}\n},\n",
            "]\n"
        );
        let events = AnalyzeChatmlCommand::parse_trace_relaxed(content)
            .expect("a metadata event must not poison the trace");
        assert_eq!(
            events.len(),
            1,
            "the http event must survive the metadata event: {events:?}"
        );
        assert_eq!(events[0].cat, "http.request");
    }

    /// Guard: an incomplete event in a single-line trace is skipped as before.
    #[test]
    fn parse_trace_relaxed_single_line_incomplete_event_still_survives() {
        let content = concat!(
            "[\n",
            "{\"name\":\"process_name\",\"ph\":\"M\",\"pid\":1,\"tid\":1},\n",
            "{\"ph\":\"i\",\"name\":\"a\",\"cat\":\"c\",\"ts\":1,\"pid\":1,\"tid\":1},\n",
            "]\n"
        );
        let events = AnalyzeChatmlCommand::parse_trace_relaxed(content).expect("parses");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cat, "c");
    }

    /// The single-line-per-event shape with trailing commas (the case the
    /// line fallback was built for) keeps working, and commas inside string
    /// values survive the strip.
    #[test]
    fn parse_trace_relaxed_keeps_single_line_and_string_commas() {
        let content = concat!(
            "[\n",
            "{\"ph\":\"i\",\"name\":\"a, b\",\"cat\":\"c\",\"ts\":1,\"pid\":1,\"tid\":1},\n",
            "{\"ph\":\"i\",\"name\":\"second\",\"cat\":\"c\",\"ts\":2,\"pid\":1,\"tid\":1},\n",
            "]\n"
        );
        let events = AnalyzeChatmlCommand::parse_trace_relaxed(content)
            .expect("single-line trace with trailing commas parses");
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].name, "a, b",
            "commas inside string values survive"
        );
        assert_eq!(events[1].name, "second");
    }

    /// An empty array (with or without a trailing comma) still yields no
    /// events rather than an error.
    #[test]
    fn parse_trace_relaxed_empty_array_yields_no_events() {
        let empty = AnalyzeChatmlCommand::parse_trace_relaxed("[]\n").expect("empty array");
        assert!(empty.is_empty());
        let empty_pretty =
            AnalyzeChatmlCommand::parse_trace_relaxed("[\n]\n").expect("empty pretty array");
        assert!(empty_pretty.is_empty());
    }
}
