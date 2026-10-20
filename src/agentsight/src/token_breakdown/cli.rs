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
                    // The writer stores a stream under `sse_events` and a
                    // non-streaming answer under `body`. Both are the same
                    // provider payload the extractor aggregates, so a body is
                    // offered as the single frame of a synthetic stream.
                    if let Some(ref args) = event.args {
                        let response = args
                            .get("sse_events")
                            .and_then(|v| v.as_array())
                            .map(|events| Self::extract_response_from_sse(events))
                            .or_else(|| {
                                args.get("body").map(|body| {
                                    let payload = match body {
                                        serde_json::Value::String(text) => text.clone(),
                                        other => other.to_string(),
                                    };
                                    Self::extract_response_from_sse(&[serde_json::json!({
                                        "data": payload
                                    })])
                                })
                            });
                        response.map(|response| classify_document(&[], Some(response)))
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

        let tools = crate::parser::llm::extract_tools_view(body);

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
    /// `content_block_*` events, the OpenAI Responses `response.*` events, or
    /// the DashScope/Bailian native envelope (`output.…`). All four shapes are
    /// aggregated; a stream answers in exactly one of them, so the
    /// accumulators never mix in practice.
    fn extract_response_from_sse(sse_events: &[serde_json::Value]) -> ResponseData {
        let mut content_parts = Vec::new();
        let mut reasoning_parts = Vec::new();
        // OpenAI-compatible streams deliver each tool call across deltas
        // keyed by `index`: the function name arrives once (usually in the
        // first fragment) and the arguments stream as string fragments that
        // must be concatenated per index before rendering "name: arguments".
        // The index is scoped to its choice, so the slot key carries both.
        let mut tool_calls: Vec<((u64, usize), String, String)> = Vec::new();
        // Slot each choice's latest tool-call fragment landed in, so an
        // index-less continuation can extend the call that choice is holding
        // instead of folding it into slot 0.
        let mut current_tool_call_slot: std::collections::BTreeMap<u64, usize> =
            std::collections::BTreeMap::new();
        // Anthropic tool_use blocks, keyed by content-block index: the id and
        // name arrive in `content_block_start`, the arguments stream as
        // `input_json_delta` fragments.
        let mut anthropic_calls: std::collections::BTreeMap<u64, (String, String, String)> =
            std::collections::BTreeMap::new();
        // Responses API: the shared aggregator routes each argument event to
        // its own output item by id/index and honors a full `done` payload,
        // matching the live analyzer instead of assuming one call in flight.
        let mut responses_tool_calls = crate::analyzer::message::ResponsesToolCalls::default();
        // DashScope/Bailian native envelope: the payload rides under a
        // top-level `output` object (`output.text`, or
        // `output.choices[].message`), so neither the `choices[].delta` walk
        // below nor the typed event shapes above sees it. Native streaming
        // defaults to `incremental_output=false` and repeats the *cumulative*
        // text on every chunk, so text and reasoning accumulate
        // replace-or-append; native tool calls repeat complete on every chunk,
        // so the last value per index wins instead of the arguments being
        // concatenated. Both rules match the shared native merger.
        let mut native_content = String::new();
        let mut native_reasoning = String::new();
        let mut native_calls: std::collections::BTreeMap<u64, (String, String)> =
            std::collections::BTreeMap::new();
        // OpenAI chat-completions cumulative `message` snapshots (see the
        // choices walk below). They repeat the answer so far, so they need a
        // single replace-or-extend buffer rather than one entry per frame.
        let mut message_content = String::new();
        let mut message_reasoning = String::new();
        // A refusal rides its own field (`delta.refusal` on a stream,
        // `message.refusal` in a cumulative snapshot) while `content` stays
        // null, so the content walks above never see it. The message parser
        // and the token extractor both read it as the call's answer.
        let mut message_refusal = String::new();
        // Responses closing events repeat the finalized answer. Like the
        // analyzer and the drain merger, use them only when no delta carried
        // text: a capture that attached after the deltas has them as its only
        // copy, while a complete capture must not count the answer twice.
        let mut saw_responses_text_delta = false;
        let mut saw_responses_reasoning_delta = false;
        let mut responses_closing_content: Option<String> = None;
        // Reasoning items carry different thinking rather than a repeat of
        // one another (an agentic response interleaves one with each call it
        // precedes), so they accumulate in arrival order instead of replacing.
        let mut responses_closing_reasoning = String::new();

        /// Answer text of a closing `message` output item, or `None`.
        ///
        /// A refusal part keeps the answer in `refusal` rather than `text` —
        /// the shape the message parser and the token extractor read. Without
        /// it a capture that attached after the refusal deltas, whose only
        /// copy of the answer is the closing item, still rendered an empty
        /// response here.
        fn closing_message_item_text(chunk: &serde_json::Value) -> Option<String> {
            let item = chunk.get("item")?;
            if item.get("type").and_then(|t| t.as_str()) == Some("reasoning") {
                return None;
            }
            let text: String = item
                .get("content")?
                .as_array()?
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .or_else(|| part.get("refusal"))
                        .and_then(|t| t.as_str())
                })
                .collect();
            (!text.is_empty()).then_some(text)
        }

        /// Thinking text of a closing `reasoning` output item, or `None`.
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

        /// Accumulate cumulative-or-incremental text: a chunk repeating the
        /// buffer plus more replaces it, a bare fragment is appended.
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
            if let Some(text) = content.as_str() {
                return text.to_string();
            }
            content
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                        .collect::<String>()
                })
                .unwrap_or_default()
        }

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
                        // A compatible gateway may put the block's complete
                        // payload on `content_block_start` and send no deltas:
                        // seed the text/thinking buffers and the tool input
                        // from the start event, the rule the live analyzer
                        // applies. The normal stream sends an empty payload
                        // here, so the deltas still drive it.
                        Some("content_block_start") => {
                            if let Some(block) = data_json.get("content_block") {
                                let index =
                                    data_json.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                                match block.get("type").and_then(|v| v.as_str()) {
                                    Some("text") => {
                                        if let Some(text) =
                                            block.get("text").and_then(|v| v.as_str())
                                        {
                                            if !text.is_empty() {
                                                content_parts.push(text.to_string());
                                            }
                                        }
                                    }
                                    Some("thinking") => {
                                        if let Some(text) =
                                            block.get("thinking").and_then(|v| v.as_str())
                                        {
                                            if !text.is_empty() {
                                                reasoning_parts.push(text.to_string());
                                            }
                                        }
                                    }
                                    Some("tool_use") => {
                                        let arguments = block
                                            .get("input")
                                            .filter(|input| {
                                                input
                                                    .as_object()
                                                    .is_some_and(|object| !object.is_empty())
                                            })
                                            .map(|input| input.to_string())
                                            .unwrap_or_default();
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
                                                arguments,
                                            )
                                        });
                                    }
                                    _ => {}
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
                                    saw_responses_text_delta = true;
                                    content_parts.push(delta.to_string());
                                }
                            }
                        }
                        // The closing events carry the finalized answer and
                        // repeat what the deltas delivered. Read them only
                        // when no delta was captured, the delta-wins rule the
                        // analyzer and the drain merger apply.
                        Some("response.output_text.done") => {
                            if !saw_responses_text_delta {
                                if let Some(text) = data_json.get("text").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        responses_closing_content = Some(text.to_string());
                                    }
                                }
                            }
                        }
                        // A refusal is the model's answer for that call and
                        // arrives instead of a content delta, so it belongs in
                        // the answer text — the same place the shared
                        // aggregator and the live analyzer put it. Without
                        // this arm a refusal-only turn rendered an empty
                        // response while the message view showed the refusal.
                        Some("response.refusal.delta") => {
                            if let Some(delta) = data_json.get("delta").and_then(|v| v.as_str()) {
                                if !delta.is_empty() {
                                    // A refusal is answer text, so it counts
                                    // as an answer delta: the closing events
                                    // repeat it and must not be read again.
                                    saw_responses_text_delta = true;
                                    content_parts.push(delta.to_string());
                                }
                            }
                        }
                        // The closing event repeats the whole refusal and is
                        // read only when no delta was captured — the same
                        // delta-wins rule the aggregator applies. Like
                        // `output_text.done` it is held as the closing answer,
                        // so a later closing item that repeats it replaces it
                        // instead of being counted next to it.
                        Some("response.refusal.done") => {
                            if content_parts.is_empty() {
                                if let Some(refusal) =
                                    data_json.get("refusal").and_then(|v| v.as_str())
                                {
                                    if !refusal.is_empty() {
                                        responses_closing_content = Some(refusal.to_string());
                                    }
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
                                    saw_responses_reasoning_delta = true;
                                    reasoning_parts.push(delta.to_string());
                                }
                            }
                        }
                        // A closing output item carries the whole message or
                        // reasoning block, the fallback shape a capture that
                        // started after the deltas is left with.
                        Some("response.output_item.done") => {
                            responses_tool_calls.observe(&data_json);
                            if !saw_responses_text_delta {
                                if let Some(text) = closing_message_item_text(&data_json) {
                                    responses_closing_content = Some(text);
                                }
                            }
                            if !saw_responses_reasoning_delta {
                                if let Some(text) = closing_reasoning_item_text(&data_json) {
                                    responses_closing_reasoning.push_str(&text);
                                }
                            }
                        }
                        // The shared aggregator also treats `output_item.done`
                        // as authoritative for tool calls: it carries the
                        // complete arguments (and the whole call, for a
                        // capture that started mid-stream or a
                        // custom_tool_call whose input only appears there).
                        // A custom tool call streams its input on its own
                        // event pair, so those names belong here too.
                        Some("response.output_item.added")
                        | Some("response.function_call_arguments.delta")
                        | Some("response.function_call_arguments.done")
                        | Some("response.custom_tool_call_input.delta")
                        | Some("response.custom_tool_call_input.done") => {
                            responses_tool_calls.observe(&data_json);
                        }
                        _ => {}
                    }

                    // Extract content and reasoning_content from the OpenAI
                    // chat-completions payload. A choice carries either
                    // streaming `delta` fragments or a cumulative `message`
                    // snapshot (SysOM's Copilot stream and some gateways send
                    // the latter), so read both. A snapshot repeats the answer
                    // so far, so it accumulates replace-or-extend into a single
                    // buffer instead of being appended as another fragment.
                    if let Some(choices) = data_json.get("choices").and_then(|v| v.as_array()) {
                        for (choice_pos, choice) in choices.iter().enumerate() {
                            let choice_index = choice
                                .get("index")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(choice_pos as u64);
                            let (payload, cumulative) =
                                match (choice.get("delta"), choice.get("message")) {
                                    (Some(delta), _) => (delta, false),
                                    (None, Some(message)) => (message, true),
                                    // A legacy `/v1/completions` stream puts
                                    // each fragment straight on the choice as
                                    // `text`, with no `delta` or `message`
                                    // object; the extractor counts it, so the
                                    // breakdown has to read it too.
                                    (None, None) => {
                                        if let Some(text) =
                                            choice.get("text").and_then(|v| v.as_str())
                                        {
                                            if !text.is_empty() {
                                                content_parts.push(text.to_string());
                                            }
                                        }
                                        continue;
                                    }
                                };
                            // Extract content
                            if let Some(content) = payload.get("content").and_then(|v| v.as_str()) {
                                if !content.is_empty() {
                                    if cumulative {
                                        accumulate(&mut message_content, content);
                                    } else {
                                        content_parts.push(content.to_string());
                                    }
                                }
                            }
                            // Extract reasoning_content
                            if let Some(reasoning) =
                                payload.get("reasoning_content").and_then(|v| v.as_str())
                            {
                                if !reasoning.is_empty() {
                                    if cumulative {
                                        accumulate(&mut message_reasoning, reasoning);
                                    } else {
                                        reasoning_parts.push(reasoning.to_string());
                                    }
                                }
                            }
                            // A refusal is the model's answer for the call and
                            // arrives instead of content, so it belongs in the
                            // answer text — where the message parser and the
                            // token extractor put it. Without this the whole
                            // answer of a refused call was dropped.
                            if let Some(refusal) = payload.get("refusal").and_then(|v| v.as_str()) {
                                if !refusal.is_empty() {
                                    if cumulative {
                                        accumulate(&mut message_refusal, refusal);
                                    } else {
                                        content_parts.push(refusal.to_string());
                                    }
                                }
                            }
                            // Extract tool_calls - merge function name and
                            // streamed arguments fragments by index. A message
                            // snapshot carries the complete call, so it
                            // replaces the slot instead of appending to it.
                            if let Some(calls) =
                                payload.get("tool_calls").and_then(|t| t.as_array())
                            {
                                for (call_pos, call) in calls.iter().enumerate() {
                                    let function = call.get("function");
                                    let name = function
                                        .and_then(|f| f.get("name"))
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("");
                                    let arguments = function
                                        .and_then(|f| f.get("arguments"))
                                        .and_then(|a| a.as_str())
                                        .unwrap_or("");
                                    // `index` is optional on the wire. A
                                    // cumulative snapshot repeats the whole
                                    // list, so the position names the call; in
                                    // a delta stream a non-empty name starts a
                                    // new call and an unnamed fragment extends
                                    // the one this choice is holding.
                                    let index = match call.get("index").and_then(|i| i.as_u64()) {
                                        Some(index) => index as usize,
                                        None if cumulative => call_pos,
                                        None => {
                                            if name.is_empty() {
                                                current_tool_call_slot
                                                    .get(&choice_index)
                                                    .copied()
                                                    .unwrap_or(0)
                                            } else {
                                                tool_calls
                                                    .iter()
                                                    .filter(|((choice, _), _, _)| {
                                                        *choice == choice_index
                                                    })
                                                    .map(|((_, index), _, _)| *index)
                                                    .max()
                                                    .map_or(0, |slot| slot.saturating_add(1))
                                            }
                                        }
                                    };
                                    current_tool_call_slot.insert(choice_index, index);
                                    match tool_calls.iter_mut().find(|((choice, slot), _, _)| {
                                        *choice == choice_index && *slot == index
                                    }) {
                                        Some((_, slot_name, slot_arguments)) => {
                                            if !name.is_empty() {
                                                *slot_name = name.to_string();
                                            }
                                            if cumulative {
                                                *slot_arguments = arguments.to_string();
                                            } else {
                                                slot_arguments.push_str(arguments);
                                            }
                                        }
                                        None => {
                                            tool_calls.push((
                                                (choice_index, index),
                                                name.to_string(),
                                                arguments.to_string(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // DashScope/Bailian native envelope; see the accumulator
                    // notes at the top of this function.
                    if let Some(output) = data_json.get("output").filter(|o| o.is_object()) {
                        if let Some(text) = output.get("text").and_then(|t| t.as_str()) {
                            accumulate(&mut native_content, text);
                        }
                        for choice in output
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .map(|c| c.as_slice())
                            .unwrap_or_default()
                        {
                            let Some(message) = choice.get("message") else {
                                continue;
                            };
                            if let Some(content) = message.get("content") {
                                accumulate(&mut native_content, &content_text(content));
                            }
                            if let Some(reasoning) =
                                message.get("reasoning_content").and_then(|r| r.as_str())
                            {
                                accumulate(&mut native_reasoning, reasoning);
                            }
                            for (position, call) in message
                                .get("tool_calls")
                                .and_then(|t| t.as_array())
                                .map(|t| t.as_slice())
                                .unwrap_or_default()
                                .iter()
                                .enumerate()
                            {
                                let index = call
                                    .get("index")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(position as u64);
                                let function = call.get("function");
                                let name = function
                                    .and_then(|f| f.get("name"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let arguments = function
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let entry = native_calls.entry(index).or_default();
                                if !name.is_empty() {
                                    entry.0 = name.to_string();
                                }
                                if !arguments.is_empty() {
                                    entry.1 = arguments.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }

        // Native text and reasoning accumulated into a single buffer each: the
        // cumulative snapshots are not separate fragments to tokenize.
        if !native_content.is_empty() {
            content_parts.push(native_content);
        }
        if !native_reasoning.is_empty() {
            reasoning_parts.push(native_reasoning);
        }

        // A message-snapshot stream answers in one cumulative form: the final
        // snapshot is the answer, so it supersedes any delta fragments captured
        // alongside it (a stream normally uses one form or the other).
        if !message_content.is_empty() {
            content_parts.clear();
            content_parts.push(message_content);
        }
        if !message_reasoning.is_empty() {
            reasoning_parts.clear();
            reasoning_parts.push(message_reasoning);
        }
        // A refusal snapshot is the whole answer of that snapshot form, so it
        // supersedes any fragments captured alongside it, exactly as the
        // content snapshot does.
        if !message_refusal.is_empty() {
            content_parts.clear();
            content_parts.push(message_refusal);
        }

        // A capture that attached after the deltas has the answer only in the
        // closing events; a complete capture already counted its deltas.
        if !saw_responses_text_delta {
            if let Some(text) = responses_closing_content {
                if !text.is_empty() {
                    content_parts.push(text);
                }
            }
        }
        if !saw_responses_reasoning_delta && !responses_closing_reasoning.is_empty() {
            reasoning_parts.push(responses_closing_reasoning);
        }

        let mut tool_calls: Vec<String> = {
            let mut calls = tool_calls;
            calls.sort_by_key(|(key, _, _)| *key);
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

        // Native calls in wire-index order; every chunk repeated the complete
        // call, so only the last value per index survives.
        for (_, (name, arguments)) in native_calls {
            if !name.is_empty() || !arguments.is_empty() {
                tool_calls.push(format!("{name}: {arguments}"));
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

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

    /// Each call writes the fixture to a UNIQUE temp directory: the tests in
    /// this module run in parallel inside one test binary (one PID), so a
    /// PID-keyed path is shared by every test and a concurrent writer
    /// truncates the JSON under a reader's feet ("EOF while parsing a
    /// value"). Mirrors the `TemporaryRegularFile` idiom in
    /// `enforcement::target`: PID + nanosecond timestamp + atomic counter.
    fn fixture_tokenizer() -> LlmTokenizer {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after UNIX_EPOCH")
            .as_nanos();
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "agentsight-chatml-fixture-{pid}-{timestamp}-{counter}"
        ));
        // `create_dir`, not `create_dir_all`: a path collision must fail
        // loudly, never silently share the directory again.
        std::fs::create_dir(&dir).expect("create unique fixture dir");
        let tokenizer_path = dir.join("tokenizer.json");
        let config_path = dir.join("tokenizer_config.json");
        std::fs::write(&tokenizer_path, TOKENIZER_JSON).expect("write tokenizer.json");
        std::fs::write(&config_path, TOKENIZER_CONFIG_JSON).expect("write tokenizer_config.json");
        let tokenizer = LlmTokenizer::from_file(&tokenizer_path, &config_path)
            .expect("fixture tokenizer loads");
        // The tokenizer is loaded fully into memory, so the fixture files can
        // go away immediately; unique paths must not pile up in the temp dir
        // across test runs.
        let _ = std::fs::remove_dir_all(&dir);
        tokenizer
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

    /// A body whose content the chat template cannot render (a content block
    /// array, the shape multimodal clients send) made the template attempt in
    /// `count_request_tokens` fail, and its `Err` arm answered a plain 0 while
    /// claiming "count raw content + tools separately" — the request was
    /// recorded as a zero-input breakdown next to a counted output side, with
    /// `by_role`/`per_message` distributing that zero. The fallback must sum
    /// the serialized messages, the same estimate the manual counter's own
    /// fallback and the drain fallback answer with.
    #[test]
    fn unrenderable_content_still_counts_raw_tokens() {
        let tokenizer = fixture_tokenizer();
        let request = json!({
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello there"}]}]
        });
        let count = crate::analyzer::count_request_tokens(&request, &tokenizer, &tokenizer)
            .expect("request is counted");
        assert!(
            count.total_tokens > 0,
            "the raw fallback must count the messages, got {count:?}"
        );
        assert!(
            count.by_role.get("user").copied().unwrap_or(0) > 0,
            "the distributed per-role count must be nonzero, got {count:?}"
        );
        assert_eq!(count.per_message.len(), 1);
    }

    /// Tool definitions ride along in the same fallback: a body the template
    /// cannot render that also declares `tools` must count the definitions on
    /// top of the raw messages (they never reached the template, and there is
    /// no tool-role message to fold them into).
    #[test]
    fn unrenderable_content_still_counts_tool_definitions() {
        let tokenizer = fixture_tokenizer();
        let with_tools = json!({
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello there"}]}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "list_dir",
                    "description": "List the entries of a directory",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            }]
        });
        let without_tools = json!({
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hello there"}]}]
        });
        let counted = crate::analyzer::count_request_tokens(&with_tools, &tokenizer, &tokenizer)
            .expect("request is counted");
        let baseline =
            crate::analyzer::count_request_tokens(&without_tools, &tokenizer, &tokenizer)
                .expect("request is counted");
        assert!(
            counted.total_tokens > baseline.total_tokens,
            "the tool definitions must reach the fallback count: {} vs {}",
            counted.total_tokens,
            baseline.total_tokens
        );
        assert!(counted.tools_tokens > 0);
    }

    /// The Responses API repeats the whole answer in `*.done` events; folding
    /// every captured event through `extract_response_content` used to count
    /// it once per event. `merge_response_output_text` must count the deltas
    /// once and use the closing events only when no delta carried text.
    #[test]
    fn responses_done_events_do_not_double_count_the_answer() {
        // The fixture tokenizer is WordLevel with a whitespace pre-tokenizer,
        // so duplicated words are observable in the count.
        let tokenizer = fixture_tokenizer();
        let chunks = vec![
            json!({"type": "response.output_text.delta", "delta": "hello "}),
            json!({"type": "response.output_text.delta", "delta": "there"}),
            json!({"type": "response.output_text.done", "text": "hello there"}),
            json!({"type": "response.output_item.done", "item": {"content": [{"text": "hello there"}]}}),
        ];

        let merged = crate::analyzer::token::merge_response_output_text(&chunks);
        assert_eq!(
            merged.0, "hello there",
            "the deltas must win over the closing replay, got {:?}",
            merged.0
        );

        let count =
            crate::analyzer::count_response_tokens(&chunks, &tokenizer).expect("response counts");
        let once = tokenizer.count("hello there").expect("fixture counts");
        assert_eq!(
            count.total_tokens, once,
            "the answer must be counted once: {:?}",
            count.by_type
        );

        // A capture that only got the closing event still counts it.
        let only_done = vec![json!({"type": "response.output_text.done", "text": "hello there"})];
        let count = crate::analyzer::count_response_tokens(&only_done, &tokenizer)
            .expect("response counts");
        assert_eq!(count.total_tokens, once);
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

    fn response_event(body: serde_json::Value, ts: u64) -> ChromeTraceEvent {
        let mut event = ChromeTraceEvent::instant("200 OK", "http.response", 1, 1, ts);
        event.args = Some(json!({ "body": body }));
        event
    }

    /// The trace writer stores a non-streaming response under `body`, but the
    /// analyzer only read `sse_events`. A `stream: false` call therefore
    /// contributed no output side at all: with a request event present the
    /// response silently vanished, and a response-only trace failed with
    /// "No valid http.request or http.response events found".
    #[test]
    fn a_non_sse_response_body_is_counted() {
        let tokenizer = fixture_tokenizer();
        let events = vec![response_event(
            json!({
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hello there"},
                    "finish_reason": "stop"
                }]
            }),
            1,
        )];

        let breakdowns = AnalyzeChatmlCommand::process_events(&events, &tokenizer, "test-model")
            .expect("a non-SSE response must still break down");
        assert_eq!(breakdowns.len(), 1);
        assert!(
            breakdowns[0].total_tokens > 0,
            "the answer must be counted, got {:?}",
            breakdowns[0]
        );
    }

    fn sse(payload: &str) -> serde_json::Value {
        json!({ "data": payload })
    }

    /// A legacy `/v1/completions` stream carries each fragment directly on the
    /// choice (`choices[].text`), with no `delta` or `message` object. The
    /// choices walk skipped every frame, so the CLI broke down an empty answer
    /// while the token extractor counted the generated text.
    #[test]
    fn sse_legacy_completion_text_is_extracted() {
        let events = vec![
            sse(r#"{"choices":[{"index":0,"text":"hello","finish_reason":null}]}"#),
            sse(r#"{"choices":[{"index":0,"text":" there","finish_reason":null}]}"#),
            sse(r#"{"choices":[{"index":0,"text":"","finish_reason":"stop"}]}"#),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.content,
            vec!["hello".to_string(), " there".to_string()]
        );
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
    fn sse_tool_calls_keep_choice_identity_apart() {
        // OpenAI defines tool-call `index` per choice: both choices below
        // start at index 0. Keying slots only by the wire index merged them
        // into one call, replacing the first name and concatenating unrelated
        // argument fragments.
        let events = vec![
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"first_tool","arguments":"{\"a\":1}"}}]}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":1,"delta":{"tool_calls":[{"index":0,"function":{"name":"second_tool","arguments":"{\"b\":2}"}}]}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec![
                r#"first_tool: {"a":1}"#.to_string(),
                r#"second_tool: {"b":2}"#.to_string()
            ]
        );
    }

    #[test]
    fn sse_unindexed_sequential_tool_calls_stay_separate() {
        // OpenAI's `index` is optional. A compatible gateway streams calls
        // without one, marking each new call by its non-empty function name;
        // an unnamed fragment continues the current call. Keying every
        // index-less fragment to slot 0 folded the second call into the first
        // and concatenated unrelated argument strings.
        let events = vec![
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_1","function":{"name":"first_tool","arguments":"{\"a\":1}"}}]}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_2","function":{"name":"second_tool","arguments":"{\"b\":2}"}}]}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec![
                r#"first_tool: {"a":1}"#.to_string(),
                r#"second_tool: {"b":2}"#.to_string()
            ]
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

    /// A compatible Anthropic gateway may put the block's complete payload on
    /// `content_block_start` and send no deltas. The CLI discarded the start
    /// payload (text/thinking were ignored, the tool input was replaced with
    /// an empty string), so such a capture broke down with an empty answer
    /// and a tool call rendered as `name: `.
    #[test]
    fn sse_anthropic_block_start_payload_is_kept() {
        let events = vec![
            sse(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Hello"}}"#,
            ),
            sse(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"pondering"}}"#,
            ),
            sse(
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{"city":"Beijing"}}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["Hello".to_string()]);
        assert_eq!(resp.reasoning_content.as_deref(), Some("pondering"));
        assert_eq!(
            resp.tool_calls,
            vec![r#"get_weather: {"city":"Beijing"}"#.to_string()],
            "the block-start input must not be discarded"
        );
    }

    /// The DashScope/Bailian native envelope nests the answer under a
    /// top-level `output` object, so none of the protocol walks above matches
    /// it and a captured native stream broke down as completely empty.
    /// Native streaming defaults to `incremental_output=false`, so each chunk
    /// repeats the *cumulative* text: the aggregation must replace rather than
    /// concatenate, the rule the shared native merger applies.
    #[test]
    fn sse_dashscope_native_message_stream_is_extracted() {
        let events = vec![
            sse(
                r#"{"output":{"choices":[{"message":{"content":"你","reasoning_content":"让我想","role":"assistant"},"finish_reason":"null"}]},"usage":{"input_tokens":10,"output_tokens":1,"total_tokens":11},"request_id":"r1"}"#,
            ),
            sse(
                r#"{"output":{"choices":[{"message":{"content":"你好","reasoning_content":"让我想一想","role":"assistant"},"finish_reason":"null"}]},"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12},"request_id":"r1"}"#,
            ),
            sse(
                r#"{"output":{"choices":[{"message":{"content":"你好吗","reasoning_content":"让我想一想","role":"assistant"},"finish_reason":"stop"}]},"usage":{"input_tokens":10,"output_tokens":3,"total_tokens":13},"request_id":"r1"}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.content,
            vec!["你好吗".to_string()],
            "cumulative snapshots must replace, not concatenate"
        );
        assert_eq!(resp.reasoning_content.as_deref(), Some("让我想一想"));
    }

    /// `result_format: text` puts the answer directly on `output.text` with no
    /// `choices` array at all.
    #[test]
    fn sse_dashscope_native_text_format_is_extracted() {
        let events = vec![
            sse(r#"{"output":{"text":"1, 2","finish_reason":"null"},"request_id":"r2"}"#),
            sse(r#"{"output":{"text":"1, 2, 3.","finish_reason":"stop"},"request_id":"r2"}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["1, 2, 3.".to_string()]);
    }

    /// The native multimodal shape spells `content` as an array of
    /// `{"text": …}` blocks instead of a string (see the
    /// `dashscope_native/multimodal_generation_*` fixtures).
    #[test]
    fn sse_dashscope_native_content_blocks_are_extracted() {
        let events = vec![sse(
            r#"{"output":{"choices":[{"message":{"content":[{"text":"这张"},{"text":"照片"}],"role":"assistant"},"finish_reason":"stop"}]},"request_id":"r4"}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["这张照片".to_string()]);
    }

    /// Native tool calls arrive complete on every chunk, so the last value per
    /// index wins where the OpenAI shape streams argument fragments that must
    /// be concatenated.
    #[test]
    fn sse_dashscope_native_tool_calls_are_extracted() {
        let events = vec![
            sse(
                r#"{"output":{"choices":[{"message":{"content":"","tool_calls":[{"index":0,"function":{"name":"get_weather","arguments":"{\"city\":"}}]},"finish_reason":"null"}]},"request_id":"r3"}"#,
            ),
            sse(
                r#"{"output":{"choices":[{"message":{"content":"","tool_calls":[{"index":0,"function":{"name":"get_weather","arguments":"{\"city\":\"Beijing\"}"}}]},"finish_reason":"tool_calls"}]},"request_id":"r3"}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec![r#"get_weather: {"city":"Beijing"}"#.to_string()]
        );
    }

    /// SysOM's Copilot stream and some OpenAI-compatible gateways send
    /// `choices[].message` snapshots instead of `choices[].delta` fragments:
    /// each frame repeats the answer so far. The walk read only `delta`, so
    /// such a capture broke down as an empty response.
    #[test]
    fn sse_openai_message_snapshots_are_aggregated_once() {
        let events = vec![
            sse(r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"Hello"}}]}"#),
            sse(
                r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"Hello there!","reasoning_content":"thin"}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"Hello there!","reasoning_content":"thinking","tool_calls":[{"index":0,"function":{"name":"get_weather","arguments":"{\"city\":\"Beijing\"}"}}]}}]}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.content,
            vec!["Hello there!".to_string()],
            "cumulative snapshots must not be appended as separate fragments"
        );
        assert_eq!(resp.reasoning_content.as_deref(), Some("thinking"));
        assert_eq!(
            resp.tool_calls,
            vec![r#"get_weather: {"city":"Beijing"}"#.to_string()]
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

    /// A capture that started after the deltas carries the answer only in the
    /// closing events. The CLI routed every Responses event except those, so
    /// such a capture reported an empty response.
    #[test]
    fn sse_responses_closing_events_supply_a_missed_answer() {
        let only_text = vec![sse(
            r#"{"type":"response.output_text.done","text":"hello there"}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&only_text);
        assert_eq!(resp.content, vec!["hello there".to_string()]);
        assert_eq!(resp.reasoning_content, None);

        let only_item = vec![sse(
            r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"done answer"}]}}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&only_item);
        assert_eq!(resp.content, vec!["done answer".to_string()]);

        let only_reasoning = vec![sse(
            r#"{"type":"response.output_item.done","item":{"type":"reasoning","content":[{"type":"reasoning_text","text":"thought"}]}}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&only_reasoning);
        assert!(resp.content.is_empty());
        assert_eq!(resp.reasoning_content.as_deref(), Some("thought"));
    }

    /// An agentic Responses stream interleaves reasoning items with the calls
    /// they precede, so one response can carry several of them: `reasoning`,
    /// `function_call`, `reasoning`, `message`. A capture that attached after
    /// both thinking deltas has only the closing `response.output_item.done`
    /// events, and they carry different text. Keeping the last one counted
    /// only the final block as reasoning, so the breakdown under-reported the
    /// thinking the same capture delivered.
    #[test]
    fn sse_responses_keeps_every_closing_reasoning_item() {
        let events = vec![
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"reasoning","content":[{"type":"reasoning_text","text":"Check the logs. "}]}}"#,
            ),
            sse(
                r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_010","name":"read_log"}}"#,
            ),
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"reasoning","summary":[{"type":"summary_text","text":"Then retry."}]}}"#,
            ),
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"Done"}]}}"#,
            ),
            sse(r#"{"type":"response.completed","response":{"id":"resp_010"}}"#),
        ];

        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["Done".to_string()]);
        assert_eq!(
            resp.reasoning_content.as_deref(),
            Some("Check the logs. Then retry."),
            "every closing reasoning item carries different thinking and must be kept"
        );

        // Accumulating the closing items must not weaken the delta-wins rule:
        // when the thinking arrived as deltas, the closing items repeat it and
        // are dropped whatever their number.
        let with_deltas = vec![
            sse(r#"{"type":"response.reasoning_text.delta","delta":"Check the logs. "}"#),
            sse(r#"{"type":"response.reasoning_text.delta","delta":"Then retry."}"#),
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"reasoning","content":[{"type":"reasoning_text","text":"Check the logs. "}]}}"#,
            ),
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"reasoning","content":[{"type":"reasoning_text","text":"Then retry."}]}}"#,
            ),
            sse(r#"{"type":"response.output_text.delta","delta":"Done"}"#),
            sse(r#"{"type":"response.completed","response":{"id":"resp_011"}}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&with_deltas);
        assert_eq!(
            resp.reasoning_content.as_deref(),
            Some("Check the logs. Then retry."),
            "delivered deltas supersede every closing reasoning item"
        );
    }

    /// The closing events replay what the deltas already delivered; a complete
    /// capture must count the answer once.
    #[test]
    fn sse_responses_closing_events_do_not_double_count_deltas() {
        let events = vec![
            sse(r#"{"type":"response.output_text.delta","delta":"hello"}"#),
            sse(r#"{"type":"response.output_text.done","text":"hello"}"#),
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"hello"}]}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["hello".to_string()]);
    }

    /// `response.output_item.done` is authoritative for a Responses tool
    /// call: it repeats the complete arguments and is the only event that
    /// carries a `custom_tool_call`'s input or a call whose `added` event was
    /// missed. The CLI routed only `added` and the argument events, so the
    /// done item's payload was discarded.
    #[test]
    fn sse_responses_done_item_carries_the_tool_call() {
        let events = vec![
            sse(
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"tool_a","arguments":""}}"#,
            ),
            sse(
                r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"item_1","call_id":"call_1","name":"tool_a","arguments":"{\"a\":1}"}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec![r#"tool_a: {"a":1}"#.to_string()],
            "the done item's complete arguments must win"
        );

        // A capture that started after `added` still recovers the call.
        let only_done = vec![sse(
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"custom_tool_call","id":"item_2","call_id":"call_2","name":"tool_b","input":"{\"b\":2}"}}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&only_done);
        assert_eq!(resp.tool_calls, vec![r#"tool_b: {"b":2}"#.to_string()]);
    }

    /// A custom tool call streams its input on
    /// `response.custom_tool_call_input.delta`/`.done`; the CLI allow-list
    /// routed only the function-call events into the shared aggregator, so a
    /// stream cut before any done event reported the call with empty input
    /// while a function call in the same shape kept its partial arguments.
    #[test]
    fn sse_responses_custom_tool_input_deltas_reach_the_call() {
        let events = vec![
            sse(
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"custom_tool_call","id":"item_9","call_id":"call_9","name":"apply_patch","input":""}}"#,
            ),
            sse(
                r#"{"type":"response.custom_tool_call_input.delta","output_index":0,"delta":"*** Begin Patch\n"}"#,
            ),
            sse(
                r#"{"type":"response.custom_tool_call_input.delta","output_index":0,"delta":"*** End Patch"}"#,
            ),
            // No done event: the capture was cut mid-input.
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec!["apply_patch: *** Begin Patch\n*** End Patch".to_string()],
            "the partial custom tool input must be rendered, not dropped"
        );

        // A full done payload supersedes the partial deltas it repeats.
        let with_done = vec![
            sse(
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"custom_tool_call","id":"item_9","call_id":"call_9","name":"apply_patch","input":""}}"#,
            ),
            sse(
                r#"{"type":"response.custom_tool_call_input.delta","output_index":0,"delta":"*** Beg"}"#,
            ),
            sse(
                r#"{"type":"response.custom_tool_call_input.done","output_index":0,"input":"*** Begin Patch\n*** End Patch"}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&with_done);
        assert_eq!(
            resp.tool_calls,
            vec!["apply_patch: *** Begin Patch\n*** End Patch".to_string()],
            "the done payload must replace the deltas rather than duplicate them"
        );
    }

    /// Reasoning models on the Responses protocol stream their thinking as
    /// `response.reasoning_text.delta` (qwen3-coder via dashscope) or
    /// `response.reasoning_summary_text.delta` (o-series), the same events
    /// the live analyzer folds into the chat view's `reasoning_content`.
    /// Ignoring them left `ResponseData.reasoning_content` at `None`, so the
    /// breakdown reported a zero-token reasoning child and understated the
    /// response total that the percentages are computed from.
    #[test]
    fn sse_responses_reasoning_deltas_feed_breakdown() {
        let events = vec![
            sse(r#"{"type":"response.created","response":{"id":"resp_1"}}"#),
            sse(r#"{"type":"response.reasoning_summary_text.delta","delta":"hello "}"#),
            sse(r#"{"type":"response.reasoning_text.delta","delta":"there"}"#),
            sse(r#"{"type":"response.output_text.delta","delta":"final"}"#),
            sse(r#"{"type":"response.completed","response":{}}"#),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.reasoning_content.as_deref(),
            Some("hello there"),
            "both reasoning event kinds must accumulate in stream order"
        );
        assert_eq!(resp.content, vec!["final".to_string()]);

        let tokenizer = fixture_tokenizer();
        let blocks = vec![crate::token_breakdown::types::ChatMLBlock {
            role: "user".to_string(),
            raw_content: "hello".to_string(),
        }];
        let doc = classify_document(&blocks, Some(resp));
        let breakdown =
            compute_breakdown(&doc, &tokenizer, "qwen3.5-plus").expect("breakdown computes");
        let response_event = breakdown
            .events
            .iter()
            .find(|event| event.event_type == "response")
            .expect("response event present");
        let reasoning = response_event
            .children
            .iter()
            .find(|child| child.name == "reasoning_content")
            .expect("reasoning child present");
        assert!(
            reasoning.tokens > 0,
            "reasoning tokens must be counted, got {reasoning:?}"
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

    /// DashScope/Bailian native generation requests carry the message list
    /// under `input.messages` and every sampling parameter, tools included,
    /// under a top-level `parameters` object. Only the top-level `tools` was
    /// read, so a native request reported an empty tool list and its tool
    /// definitions were missing from the breakdown, while the live request
    /// parser has read both spellings since 30828845b.
    #[test]
    fn request_messages_reads_native_parameters_tools() {
        let body = json!({
            "model": "qwen3-max",
            "input": {"messages": [{"role": "user", "content": "list the files"}]},
            "parameters": {
                "tools": [{"type": "function", "function": {"name": "noop"}}],
            },
        });
        let (msgs, tools) =
            AnalyzeChatmlCommand::request_body_messages(&body).expect("native body parses");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(
            tools.as_ref().expect("native tools survive").len(),
            1,
            "parameters.tools must reach the breakdown"
        );

        // A top-level `tools` array still wins when both spellings are
        // present, matching `parse_request_body`.
        let both = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "top"}}],
            "parameters": {"tools": [{"type": "function", "function": {"name": "nested"}}]},
        });
        let (_, tools) = AnalyzeChatmlCommand::request_body_messages(&both).expect("body parses");
        let tools = tools.expect("tools survive");
        assert_eq!(tools.len(), 1, "the top-level array is the one that counts");
        assert_eq!(tools[0]["function"]["name"], "top");
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

    /// Streamed deltas are fragments of one answer. The fixture knows
    /// `hello` as one word and knows neither `hel` nor `lo`, so counting the
    /// fragments one by one reports two tokens for a one-token answer.
    #[test]
    fn streamed_fragments_are_counted_as_one_answer() {
        let tokenizer = fixture_tokenizer();
        let blocks = vec![crate::token_breakdown::types::ChatMLBlock {
            role: "user".to_string(),
            raw_content: "hello".to_string(),
        }];
        let response = crate::token_breakdown::types::ResponseData {
            content: vec!["hel".to_string(), "lo".to_string()],
            reasoning_content: None,
            tool_calls: Vec::new(),
        };
        let doc = classify_document(&blocks, Some(response));
        let breakdown =
            compute_breakdown(&doc, &tokenizer, "qwen3.5-plus").expect("breakdown computes");
        let response_event = breakdown
            .events
            .iter()
            .find(|event| event.event_type == "response")
            .expect("response event present");
        let content = response_event
            .children
            .iter()
            .find(|child| child.name == "content")
            .expect("content child present");
        let joined = tokenizer
            .count_with_special_tokens("hello")
            .expect("fixture counts");
        assert_eq!(
            content.tokens, joined,
            "the joined answer must be counted once, not once per delta"
        );
        assert_eq!(content.char_count, 5);
        assert_eq!(
            response_event.tokens, joined,
            "the response total must use the joined count"
        );
    }
    #[test]
    fn sse_unindexed_tool_calls_are_allocated_within_their_choice() {
        // Without a wire `index`, each choice owns its own growing slot
        // sequence. Allocating from the global maximum interleaved the two
        // choices' calls and made the output order depend on traffic from the
        // other choice.
        let events = vec![
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"name":"a","arguments":"1"}}]}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":1,"delta":{"tool_calls":[{"function":{"name":"b","arguments":"2"}}]}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"name":"c","arguments":"3"}}]}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(
            resp.tool_calls,
            vec!["a: 1".to_string(), "c: 3".to_string(), "b: 2".to_string()],
            "calls stay grouped and ordered by their choice"
        );
    }

    /// A refusal is the model's answer for the call: the Responses protocol
    /// streams it on `response.refusal.delta` and closes it with
    /// `response.refusal.done`, whose `refusal` field carries the finalized
    /// text. The dispatch read neither, so a refusal-only capture rendered an
    /// empty response in the token breakdown while the message view showed
    /// the refusal.
    #[test]
    fn sse_responses_refusal_text_reaches_the_answer() {
        let events = vec![
            sse(r#"{"type":"response.created","response":{"id":"resp_1"}}"#),
            sse(r#"{"type":"response.refusal.delta","delta":"I can't "}"#),
            sse(r#"{"type":"response.refusal.delta","delta":"help with that."}"#),
            sse(r#"{"type":"response.refusal.done","refusal":"I can't help with that."}"#),
            sse(
                r#"{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["I can't ", "help with that."]);
    }

    /// A capture that joins after the deltas keeps the closing refusal, and a
    /// stream that repeated both must not count the text twice.
    #[test]
    fn sse_responses_closing_refusal_fills_an_empty_answer() {
        let only_done = vec![sse(
            r#"{"type":"response.refusal.done","refusal":"I can't help with that."}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&only_done);
        assert_eq!(resp.content, vec!["I can't help with that."]);
    }

    /// A refusal also arrives as a content part of the closing `message`
    /// output item, typed `refusal` and carrying the text in `refusal` rather
    /// than `text`. A capture that attached after the refusal deltas has this
    /// part as its only copy of the answer, and the item reader took only the
    /// `text` parts, so the response rendered empty.
    #[test]
    fn sse_responses_closing_item_refusal_part_fills_an_empty_answer() {
        let only_item = vec![sse(
            r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"refusal","refusal":"I can't help with that."}]}}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&only_item);
        assert_eq!(resp.content, vec!["I can't help with that."]);
    }

    /// A capture that attached after the refusal deltas still sees both
    /// closing events, `response.refusal.done` and then the closing item with
    /// its refusal part. They carry the same answer, so it counts once.
    #[test]
    fn sse_responses_closing_refusal_and_item_count_once() {
        let events = vec![
            sse(r#"{"type":"response.refusal.done","refusal":"I can't help with that."}"#),
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"refusal","refusal":"I can't help with that."}]}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["I can't help with that."]);
    }

    /// The closing item repeats what the refusal deltas delivered; a complete
    /// capture must count the answer once.
    #[test]
    fn sse_responses_closing_item_does_not_repeat_refusal_deltas() {
        let events = vec![
            sse(r#"{"type":"response.refusal.delta","delta":"I can't help with that."}"#),
            sse(
                r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"refusal","refusal":"I can't help with that."}]}}"#,
            ),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["I can't help with that."]);
    }

    /// The chat-completions protocol carries a refusal in its own `refusal`
    /// field, on `delta` for a stream and on `message` for a cumulative
    /// snapshot, while `content` stays null. The dispatch read `content` and
    /// `reasoning_content` but not `refusal`, so a refused call broke down as
    /// an empty answer even though the message view showed the refusal.
    #[test]
    fn sse_chat_refusal_delta_reaches_the_answer() {
        let events = vec![
            sse(r#"{"choices":[{"index":0,"delta":{"refusal":"I can't "}}]}"#),
            sse(r#"{"choices":[{"index":0,"delta":{"refusal":"help with that."}}]}"#),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["I can't ", "help with that."]);
    }

    /// A cumulative `message` snapshot repeats the refusal so far; the final
    /// snapshot is the answer and must not be counted once per frame.
    #[test]
    fn sse_chat_refusal_snapshot_is_not_accumulated_per_frame() {
        let events = vec![
            sse(
                r#"{"choices":[{"index":0,"message":{"role":"assistant","refusal":"I can't help"}}]}"#,
            ),
            sse(
                r#"{"choices":[{"index":0,"message":{"role":"assistant","refusal":"I can't help with that."}}]}"#,
            ),
            sse("[DONE]"),
        ];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&events);
        assert_eq!(resp.content, vec!["I can't help with that."]);
    }

    /// A message item mixes a `text` part and a `refusal` part; both belong
    /// to the answer, in the order the item lists them.
    #[test]
    fn sse_responses_closing_item_reads_text_and_refusal_parts() {
        let only_item = vec![sse(
            r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"First. "},{"type":"refusal","refusal":"I can't help with that."}]}}"#,
        )];
        let resp = AnalyzeChatmlCommand::extract_response_from_sse(&only_item);
        assert_eq!(resp.content, vec!["First. I can't help with that."]);
    }
}
