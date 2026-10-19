//! OpenAI token data extraction

use super::super::data::{MessageTokenData, ResponseTokenData, TokenData};
use super::utils::extract_model_from_json;
use serde_json::Value;

/// Extract token data from OpenAI format JSON
pub fn extract_token_data(
    request_json: Option<&Value>,
    response_json: Option<&Value>,
) -> Option<TokenData> {
    let model = extract_model_from_json(request_json, response_json)
        .unwrap_or_else(|| "unknown".to_string());

    let mut token_data = TokenData::new("openai", model);
    let mut has_content = false;

    // Extract from request
    if let Some(req) = request_json {
        // Extract messages
        if let Some(messages) = req.get("messages").and_then(|m| m.as_array()) {
            for msg in messages {
                if let Some((role, content)) = extract_message(msg) {
                    token_data
                        .request_messages
                        .push(MessageTokenData { role, content });
                    has_content = true;
                }
            }
        }

        // Extract tools
        if let Some(tools) = crate::parser::llm::extract_tools_view(req) {
            for tool in tools {
                if let Ok(tool_str) = serde_json::to_string(&tool) {
                    token_data.tools.push(tool_str);
                    has_content = true;
                }
            }
        }
    }

    // Extract from response using shared logic
    if let Some((content, reasoning, tool_calls)) = extract_response_content(response_json) {
        if !content.is_empty() {
            token_data
                .response_content
                .push(ResponseTokenData { content });
            has_content = true;
        }
        if let Some(r) = reasoning {
            token_data.reasoning_content = Some(r);
            has_content = true;
        }
        for tool_call in tool_calls {
            token_data.tool_calls.push(tool_call);
            has_content = true;
        }
    }

    if has_content { Some(token_data) } else { None }
}

/// Extract response content from OpenAI format response JSON
///
/// Returns a tuple of (content, reasoning_content, tool_calls)
/// - content: The main response text
/// - reasoning_content: Optional reasoning/thinking content
/// - tool_calls: Vec of formatted tool call strings "name: arguments"
pub fn extract_response_content(
    response_json: Option<&Value>,
) -> Option<(String, Option<String>, Vec<String>)> {
    let resp = response_json?;

    let mut content = String::new();
    let mut reasoning = None;
    let mut tool_calls = Vec::new();
    let mut has_data = false;

    if let Some(choices) = resp.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            // Support both "message" (standard response) and "delta" (SSE streaming) formats
            let msg_or_delta = choice.get("message").or_else(|| choice.get("delta"));

            if let Some(msg) = msg_or_delta {
                // Extract content
                if let Some(c) = msg.get("content").and_then(|c| c.as_str()) {
                    if !c.is_empty() {
                        content.push_str(c);
                        has_data = true;
                    }
                }

                // A refusal answer arrives in `refusal` while `content` stays
                // null (`delta.refusal` on a stream, `message.refusal` in a
                // whole body), so without this arm a refused call counted no
                // output tokens in the manual counters and the drain estimate.
                if let Some(r) = msg.get("refusal").and_then(|r| r.as_str()) {
                    if !r.is_empty() {
                        content.push_str(r);
                        has_data = true;
                    }
                }

                // Extract reasoning_content
                if let Some(r) = msg.get("reasoning_content").and_then(|r| r.as_str()) {
                    if !r.is_empty() {
                        // For SSE chunks, accumulate reasoning content
                        reasoning = match reasoning {
                            Some(existing) => Some(existing + r),
                            None => Some(r.to_string()),
                        };
                        has_data = true;
                    }
                }

                // Extract tool_calls - only extract function name and arguments
                if let Some(calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                    for tool_call in calls {
                        if let Some(func) = tool_call.get("function") {
                            let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let arguments =
                                func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                            let tool_content = format!("{name}: {arguments}");
                            if !tool_content.is_empty() {
                                tool_calls.push(tool_content);
                                has_data = true;
                            }
                        }
                    }
                }

                // SysOM's Copilot streams spell the same tool calls
                // `message.tool_use` — an array of {index, id, type,
                // function: {name, arguments}} — instead of `tool_calls`.
                // Without this arm a SysOM tool call counted zero tool
                // tokens in the manual counters and the drain estimate;
                // the calls arrive whole on cumulative snapshots, whose
                // repetition the merge layer already handles like the
                // snapshot's text.
                if let Some(calls) = msg.get("tool_use").and_then(|t| t.as_array()) {
                    for tool_call in calls {
                        if let Some(func) = tool_call.get("function") {
                            let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let arguments =
                                func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                            let tool_content = format!("{name}: {arguments}");
                            if !tool_content.is_empty() {
                                tool_calls.push(tool_content);
                                has_data = true;
                            }
                        }
                    }
                }
            } else if let Some(text) = choice.get("text").and_then(|t| t.as_str()) {
                // Legacy `/v1/completions` answers put the generated text
                // directly on the choice, without a `message` or `delta`
                // object. The path is an admitted LLM endpoint, so without
                // this arm a stream that reported no usage counted zero
                // output tokens for the text it generated.
                if !text.is_empty() {
                    content.push_str(text);
                    has_data = true;
                }
            }
        }
    }

    if has_data {
        return Some((content, reasoning, tool_calls));
    }

    // Anthropic streams carry the assistant output in `delta` events and in
    // `content[]` blocks. Without these arms the manual counters recorded a
    // real answer as zero output tokens (and the drain estimate as empty),
    // because nothing here recognised the protocol.
    if let Some(delta) = resp.get("delta") {
        match delta.get("type").and_then(|t| t.as_str()) {
            Some("text_delta") => {
                if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        content.push_str(text);
                        has_data = true;
                    }
                }
            }
            Some("thinking_delta") => {
                if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                    if !thinking.is_empty() {
                        reasoning = match reasoning {
                            Some(existing) => Some(existing + thinking),
                            None => Some(thinking.to_string()),
                        };
                        has_data = true;
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(blocks) = resp.get("content").and_then(|c| c.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            content.push_str(text);
                            has_data = true;
                        }
                    }
                }
                Some("tool_use") => {
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let arguments = block
                        .get("input")
                        .map(|i| i.to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        tool_calls.push(format!("{name}: {arguments}"));
                        has_data = true;
                    }
                }
                _ => {}
            }
        }
    }

    if has_data {
        return Some((content, reasoning, tool_calls));
    }

    // OpenAI Responses API SSE chunks have a different shape — top-level
    // "type" tags such as `response.output_text.delta` carry text in
    // `delta` / `text`, while `response.output_item.done` embeds the
    // assistant message under `item.content[].text`. Extract text from
    // the kinds that contribute to assistant output tokens.
    if let Some(ty) = resp.get("type").and_then(|t| t.as_str()) {
        match ty {
            "response.output_text.delta" => {
                if let Some(d) = resp.get("delta").and_then(|d| d.as_str()) {
                    if !d.is_empty() {
                        return Some((d.to_string(), None, Vec::new()));
                    }
                }
            }
            "response.output_text.done" => {
                if let Some(t) = resp.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        return Some((t.to_string(), None, Vec::new()));
                    }
                }
            }
            // A refusal is the model's answer for that call: it arrives in
            // `delta` while `content` stays empty, and the closing event
            // repeats it in `refusal` (the same shape the message parser
            // reads). Without these arms a refused Responses call counted no
            // output tokens in the manual counters and the drain estimate.
            "response.refusal.delta" => {
                if let Some(d) = resp.get("delta").and_then(|d| d.as_str()) {
                    if !d.is_empty() {
                        return Some((d.to_string(), None, Vec::new()));
                    }
                }
            }
            "response.refusal.done" => {
                if let Some(r) = resp.get("refusal").and_then(|r| r.as_str()) {
                    if !r.is_empty() {
                        return Some((r.to_string(), None, Vec::new()));
                    }
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                if let Some(d) = resp.get("delta").and_then(|d| d.as_str()) {
                    if !d.is_empty() {
                        return Some((String::new(), Some(d.to_string()), Vec::new()));
                    }
                }
            }
            "response.output_item.done" => {
                if let Some(item) = resp.get("item") {
                    let (answer, thinking) = read_output_item(item);
                    if !answer.is_empty() || !thinking.is_empty() {
                        return Some((
                            answer,
                            (!thinking.is_empty()).then_some(thinking),
                            Vec::new(),
                        ));
                    }
                }
            }
            // Anthropic Messages streams text, thinking and tool input as
            // `content_block_delta` events, each carrying its own
            // `delta.type`. The drained-stream and analyze-chatml paths
            // aggregate the same shape, so leaving it unextracted makes the
            // callers count a response with content as zero output tokens.
            "content_block_delta" => {
                if let Some(delta) = resp.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    return Some((text.to_string(), None, Vec::new()));
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                                if !thinking.is_empty() {
                                    return Some((
                                        String::new(),
                                        Some(thinking.to_string()),
                                        Vec::new(),
                                    ));
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|t| t.as_str())
                            {
                                if !partial.is_empty() {
                                    return Some((String::new(), None, vec![partial.to_string()]));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            // A Responses function call is announced by
            // `response.output_item.added` (which carries the name) and then
            // streamed as `response.function_call_arguments.delta` fragments.
            // The chat-completions and Anthropic arms already produce the
            // `name: arguments` text the manual counters tokenize, so these
            // two events keep that convention: the announcement contributes
            // `name: `, each fragment its raw partial JSON. Without them a
            // truncated stream that only made a tool call counted zero output
            // tokens. `response.function_call_arguments.done` and the
            // function-call `response.output_item.done` repeat the full
            // arguments and are deliberately not read here: the deltas already
            // delivered them, and counting both would double the call.
            "response.output_item.added" => {
                if let Some(item) = resp.get("item") {
                    // A custom (freeform) tool is announced the same way as a
                    // function call; only the kind differs. The message parser
                    // and the chatml reader both accept it, so a captured
                    // turn that called one must count its name here too.
                    if matches!(
                        item.get("type").and_then(|t| t.as_str()),
                        Some("function_call" | "custom_tool_call")
                    ) {
                        if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                            if !name.is_empty() {
                                return Some((String::new(), None, vec![format!("{name}: ")]));
                            }
                        }
                    }
                }
            }
            // The two protocols stream a call's body under different event
            // names: `arguments` fragments for a function call, `input`
            // fragments for a custom one. Reading only the first meant a
            // truncated custom call kept its name and lost its body.
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                if let Some(delta) = resp.get("delta").and_then(|d| d.as_str()) {
                    if !delta.is_empty() {
                        return Some((String::new(), None, vec![delta.to_string()]));
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Merge the assistant output of one SSE capture into
/// `(content, reasoning, tool_call_fragments)`.
///
/// [`extract_response_content`] returns the whole answer for the closing
/// `response.output_text.done` and `response.output_item.done` events in
/// addition to every `response.output_text.delta`, so a plain fold over a
/// complete capture counts the answer two or three times. Deltas are always
/// counted; a closing event is read only when the deltas for the buffer it
/// repeats are missing, which keeps late-joined captures (only the closing
/// events present) working.
pub(crate) fn merge_response_output_text(chunks: &[Value]) -> (String, String, Vec<String>) {
    // An empty delta carries no text, so it must not suppress the closing
    // event's full-text fallback.
    let saw_delta = |kind: &str| {
        chunks.iter().any(|chunk| {
            chunk.get("type").and_then(|t| t.as_str()) == Some(kind)
                && chunk
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .is_some_and(|delta| !delta.is_empty())
        })
    };
    let saw_text_delta = saw_delta("response.output_text.delta");
    // A reasoning item repeats the thinking, not the answer, and the two
    // deltas are separate events: a capture cut after the reasoning item but
    // before the first answer delta has a reasoning delta and no text delta,
    // so the text rule alone let the item's thinking land in the answer buffer
    // while the deltas had already counted it as reasoning.
    let saw_reasoning_delta = saw_delta("response.reasoning_text.delta")
        || saw_delta("response.reasoning_summary_text.delta");
    // A refusal is the call's answer too, and it repeats the same way: the
    // `response.refusal.done` event closes the deltas, so it is read only
    // when no refusal delta was captured.
    let saw_refusal_delta = saw_delta("response.refusal.delta");
    let repeats_delivered_text = |chunk: &Value| -> bool {
        match chunk.get("type").and_then(|t| t.as_str()) {
            Some("response.output_text.done") => saw_text_delta,
            Some("response.refusal.done") => saw_refusal_delta,
            Some("response.output_item.done") => {
                if chunk.get("item").is_some_and(is_reasoning_item) {
                    saw_reasoning_delta
                } else {
                    // A message item repeats its refusal part too.
                    saw_text_delta || saw_refusal_delta
                }
            }
            _ => false,
        }
    };

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    // A snapshot's tool calls are whole and repeat with every snapshot chunk,
    // so the last one's calls are kept aside and used only when no delta
    // fragment carried any — the same delta-wins rule the closing Responses
    // events follow.
    let mut snapshot_tool_calls: Vec<String> = Vec::new();
    for chunk in chunks {
        if repeats_delivered_text(chunk) {
            continue;
        }
        // A choice carrying `message` instead of `delta` speaks in cumulative
        // snapshots: each frame repeats the answer so far rather than an
        // increment (SysOM's Copilot stream sends nothing else, and an
        // OpenAI-compatible gateway's final frame is a whole `message`).
        // Appending such a frame once per captured chunk counted the answer
        // N times; accumulate it the way the SSE merger does — replace when
        // the snapshot extends what we have.
        let snapshot = carries_message_snapshot(chunk);
        // A closing event replays the complete answer, and `output_text.done`
        // and the message in `output_item.done` repeat each other when no
        // delta was captured. Treat it as a replacement rather than another
        // fragment so the same answer is not appended twice.
        let closing = is_closing_response_event(chunk);
        if let Some((chunk_content, chunk_reasoning, chunk_tool_calls)) =
            extract_response_content(Some(chunk))
        {
            if !chunk_content.is_empty() {
                if closing {
                    content = chunk_content;
                } else if snapshot {
                    accumulate_text(&mut content, &chunk_content);
                } else {
                    content.push_str(&chunk_content);
                }
            }
            if let Some(reasoning_chunk) = chunk_reasoning {
                if !reasoning_chunk.is_empty() {
                    if closing {
                        reasoning = reasoning_chunk;
                    } else if snapshot {
                        accumulate_text(&mut reasoning, &reasoning_chunk);
                    } else {
                        reasoning.push_str(&reasoning_chunk);
                    }
                }
            }
            if snapshot {
                snapshot_tool_calls = chunk_tool_calls;
            } else {
                for tool_call in chunk_tool_calls {
                    if !tool_call.is_empty() {
                        tool_calls.push(tool_call);
                    }
                }
            }
        }
    }
    if tool_calls.is_empty() {
        tool_calls = snapshot_tool_calls;
    }

    (content, reasoning, tool_calls)
}

/// Read a finished output item as `(answer text, thinking text)`.
///
/// A `message` item carries the answer in its `content[]` parts (a refusal
/// part keeps it in `refusal`), while a `reasoning` item carries its thinking
/// there (parts typed `reasoning_text`) and, for the summary-only shape, in
/// `summary[]`. The live message parser
/// keeps the two buffers apart; reading every part as answer text made a
/// drained capture count the thinking as answer text and, once the closing
/// item repeated the deltas, twice over.
fn read_output_item(item: &Value) -> (String, String) {
    let text_of = |field: &str| -> String {
        item.get(field)
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    // A refusal part keeps the answer in `refusal`, not `text`.
                    .filter_map(|part| {
                        part.get("text")
                            .or_else(|| part.get("refusal"))
                            .and_then(Value::as_str)
                    })
                    .collect::<String>()
            })
            .unwrap_or_default()
    };
    if is_reasoning_item(item) {
        // Content first, then the summary, matching the message parser.
        (String::new(), text_of("content") + &text_of("summary"))
    } else {
        (text_of("content"), String::new())
    }
}

/// Whether an `output_item` payload is a reasoning item.
fn is_reasoning_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("reasoning")
}

/// Whether a chunk is a Responses closing event that carries the complete
/// answer rather than an incremental delta.
fn is_closing_response_event(chunk: &Value) -> bool {
    matches!(
        chunk.get("type").and_then(Value::as_str),
        Some("response.output_text.done" | "response.refusal.done" | "response.output_item.done")
    )
}

/// A choice that carries `message` instead of `delta` speaks in cumulative
/// snapshots: each frame repeats the answer so far rather than an increment.
fn carries_message_snapshot(chunk: &Value) -> bool {
    chunk
        .get("choices")
        .and_then(|c| c.as_array())
        .is_some_and(|choices| {
            choices
                .iter()
                .any(|choice| choice.get("message").is_some() && choice.get("delta").is_none())
        })
}

/// Accumulate cumulative-or-incremental text into `buf`, mirroring the SSE
/// merger's helper: a frame that repeats the whole text so far replaces the
/// buffer; a frame that carries only an increment appends.
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

/// Reconstruct complete tool calls before discarding their protocol identity.
pub(crate) fn merge_response_tool_calls(chunks: &[Value]) -> Vec<String> {
    use crate::analyzer::message::ResponsesToolCalls;
    use std::collections::BTreeMap;

    // Maps bound the number of calls, without allocating slots up to a wire
    // index. Choice identity matters too: each choice starts its indices at 0.
    let mut chat: BTreeMap<(u64, u64), (String, String)> = BTreeMap::new();
    let mut current: BTreeMap<u64, u64> = BTreeMap::new();
    let mut responses = ResponsesToolCalls::default();
    let mut compatible: Vec<String> = Vec::new();
    for chunk in chunks {
        responses.observe(chunk);
        if let Some(choices) = chunk.get("choices").and_then(Value::as_array) {
            for (choice_pos, choice) in choices.iter().enumerate() {
                if choice.get("index").is_some_and(|v| v.as_u64().is_none()) {
                    continue;
                }
                let choice_index = choice
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(choice_pos as u64);
                let snapshot = choice.get("message");
                let message = snapshot.or_else(|| choice.get("delta"));
                let Some(message) = message else {
                    continue;
                };
                // SysOM uses tool_use for the same Chat call shape.
                let calls = ["tool_calls", "tool_use"]
                    .into_iter()
                    .filter_map(|field| message.get(field).and_then(Value::as_array))
                    .flatten();
                for (call_pos, call) in calls.enumerate() {
                    if call.get("index").is_some_and(|v| v.as_u64().is_none()) {
                        continue;
                    }
                    let Some(function) = call.get("function") else {
                        continue;
                    };
                    let index = call
                        .get("index")
                        .and_then(Value::as_u64)
                        .unwrap_or_else(|| {
                            if snapshot.is_some() {
                                call_pos as u64
                            } else if function
                                .get("name")
                                .and_then(Value::as_str)
                                .is_some_and(|name| !name.is_empty())
                            {
                                // Unindexed compatible streams retain the sequential
                                // fallback: a name starts a call, continuations use it.
                                chat.keys()
                                    .filter(|(choice, _)| *choice == choice_index)
                                    .map(|(_, index)| *index)
                                    .max()
                                    .map_or(0, |index| index.saturating_add(1))
                            } else {
                                current.get(&choice_index).copied().unwrap_or(0)
                            }
                        });
                    let key = (choice_index, index);
                    if !chat.contains_key(&key) && chat.len() >= 256 {
                        continue;
                    }
                    current.insert(choice_index, index);
                    let (name, arguments) = chat.entry(key).or_default();
                    if snapshot.is_some() {
                        // A message carries the whole call, not another delta.
                        *name = function
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        *arguments = function
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                    } else {
                        if let Some(fragment) = function.get("name").and_then(Value::as_str) {
                            name.push_str(fragment);
                        }
                        if let Some(fragment) = function.get("arguments").and_then(Value::as_str) {
                            arguments.push_str(fragment);
                        }
                    }
                }
            }
            continue;
        }
        if chunk
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with("response."))
        {
            continue;
        }
        // Retain the legacy fragment contract for other compatible providers;
        // Responses and Chat calls never pass through this unnamed stream.
        if let Some((_, _, fragments)) = extract_response_content(Some(chunk)) {
            for fragment in fragments {
                let starts_call = matches!(fragment.split_once(": "), Some((name, _)) if !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.'));
                if starts_call || compatible.is_empty() {
                    compatible.push(fragment);
                } else if let Some(last) = compatible.last_mut() {
                    last.push_str(fragment.strip_prefix(": ").unwrap_or(&fragment));
                }
            }
        }
    }
    chat.into_values()
        .filter(|(name, arguments)| !name.is_empty() || !arguments.is_empty())
        .map(|(name, arguments)| format!("{name}: {arguments}"))
        .chain(
            responses
                .into_calls()
                .map(|(_, name, arguments)| format!("{name}: {arguments}")),
        )
        .chain(compatible)
        .collect()
}

/// Extract role and content from OpenAI message JSON
fn extract_message(msg: &Value) -> Option<(String, String)> {
    let role = msg.get("role").and_then(|r| r.as_str())?;
    let content = extract_content(msg.get("content"))?;

    if content.is_empty() {
        None
    } else {
        Some((role.to_string(), content))
    }
}

/// Extract text content from OpenAI content field (string or array)
fn extract_content(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Value::Array(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                        p.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");

            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_tools_replace_cumulative_chat_snapshots() {
        let chunks = [
            serde_json::json!({"choices":[{"message":{"tool_calls":[{"function":{"name":"lookup","arguments":"{\"x\":"}}]}}]}),
            serde_json::json!({"choices":[{"message":{"tool_calls":[{"function":{"name":"lookup","arguments":"{\"x\":1}"}}]}}]}),
        ];
        assert_eq!(merge_response_tool_calls(&chunks), ["lookup: {\"x\":1}"]);
        assert_eq!(
            merge_response_tool_calls(&[
                serde_json::json!({"choices":[{"message":{"tool_calls":[{"function":{}}]}}]})
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn manual_tools_ignore_invalid_identities_and_keep_sparse_calls() {
        let chunks = [
            serde_json::json!({"choices":[{"index":-1,"delta":{"tool_calls":[{"function":{"name":"invalid"}}]}}]}),
            serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":-1,"function":{"name":"invalid"}},{"index":0}]}}]}),
            serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":u64::MAX,"function":{"name":"lookup","arguments":"{}"}}]}},{"index":1,"delta":{"content":"hello"}}]}),
        ];
        assert_eq!(merge_response_tool_calls(&chunks), ["lookup: {}"]);
        let mut chunks = Vec::new();
        for index in 0..257 {
            chunks.push(serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":index,"function":{"name":"lookup","arguments":"a"}}]}}]}));
        }
        chunks.push(serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"b"}}]}}]}));
        let calls = merge_response_tool_calls(&chunks);
        assert_eq!(calls.len(), 256);
        assert_eq!(calls[0], "lookup: ab");
        assert!(calls[1..].iter().all(|call| call == "lookup: a"));
    }

    #[test]
    fn manual_tools_keep_unindexed_sequential_continuations() {
        let chunks = [
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"orphan"}}]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"first","arguments":"a"}}]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"b"}}]}}]}),
            serde_json::json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"second","arguments":"c"}}]}}]}),
        ];
        assert_eq!(
            merge_response_tool_calls(&chunks),
            [": orphan", "first: ab", "second: c"]
        );
    }

    #[test]
    fn manual_tools_keep_compatible_provider_fragments() {
        let chunks = [
            serde_json::json!({"content":[{"type":"tool_use","name":"lookup","input":{}}]}),
            serde_json::json!({"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"x\":"}}),
            serde_json::json!({"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"1}"}}),
            serde_json::json!({"content":[{"type":"tool_use","name":"other","input":{"y":2}}]}),
            serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}),
        ];
        assert_eq!(
            merge_response_tool_calls(&chunks),
            ["lookup: {}{\"x\":1}", "other: {\"y\":2}"]
        );
        assert_eq!(merge_response_tool_calls(&chunks[1..3]), ["{\"x\":1}"]);
    }

    #[test]
    fn test_extract_openai_request() {
        let request = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.provider, "openai");
        assert_eq!(data.model, "gpt-4");
        assert_eq!(data.request_messages.len(), 2);
        assert_eq!(data.request_messages[0].role, "system");
        assert_eq!(data.request_messages[0].content, "You are helpful");
    }

    #[test]
    fn test_extract_openai_response() {
        let response = serde_json::json!({
            "model": "gpt-4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hi there!"
                }
            }]
        });

        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.response_content.len(), 1);
        assert_eq!(data.response_content[0].content, "Hi there!");
    }

    #[test]
    fn test_extract_with_tools() {
        let request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "What's the weather?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather info"
                }
            }]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.tools.len(), 1);
    }

    /// DashScope/Bailian native requests carry their tool definitions under
    /// the top-level `parameters` object, so a top-level-only read reports no
    /// tools at all for that protocol.
    #[test]
    fn test_extract_native_parameters_tools() {
        let request = serde_json::json!({
            "model": "qwen3-max",
            "input": {"messages": [{"role": "user", "content": "What's the weather?"}]},
            "parameters": {
                "temperature": 0.5,
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get weather info"
                    }
                }]
            }
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.tools.len(), 1, "native parameters.tools must be read");
    }

    #[test]
    fn test_extract_reasoning_content() {
        let response = serde_json::json!({
            "model": "qwen",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42",
                    "reasoning_content": "Let me think about this..."
                }
            }]
        });

        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(
            data.reasoning_content,
            Some("Let me think about this...".to_string())
        );
    }

    #[test]
    fn test_extract_content_array() {
        let request = serde_json::json!({
            "model": "gpt-4-vision",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {"type": "image_url", "image_url": {"url": "http://example.com/image.jpg"}}
                ]
            }]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.request_messages.len(), 1);
        assert_eq!(data.request_messages[0].content, "What's in this image?");
    }

    #[test]
    fn test_empty_content_returns_none() {
        let request = serde_json::json!({"model": "gpt-4"});
        let result = extract_token_data(Some(&request), None);
        assert!(result.is_none());
    }

    /// Legacy `/v1/completions` streams put each fragment on `choices[].text`
    /// instead of a `message`/`delta` object, so the shared extractor used to
    /// return nothing and the manual counter reported zero output tokens.
    #[test]
    fn test_legacy_completion_choice_text_counts_as_output() {
        let first = serde_json::json!({"choices": [{"index": 0, "text": "hello "}]});
        let second = serde_json::json!({"choices": [{"index": 0, "text": "there"}]});

        let (content, _, tools) =
            extract_response_content(Some(&first)).expect("legacy text must extract");
        assert_eq!(content, "hello ");
        assert!(tools.is_empty());

        assert_eq!(
            merge_response_output_text(&[first, second]).0,
            "hello there",
            "the manual counter must join the streamed fragments"
        );
    }

    #[test]
    fn test_responses_api_output_text_delta() {
        let chunk = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "hello world",
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract delta text");
        assert_eq!(content, "hello world");
        assert!(reasoning.is_none());
        assert!(tools.is_empty());
    }

    #[test]
    fn test_responses_api_output_text_done() {
        let chunk = serde_json::json!({
            "type": "response.output_text.done",
            "text": "complete output text",
        });
        let (content, _, _) =
            extract_response_content(Some(&chunk)).expect("should extract final text");
        assert_eq!(content, "complete output text");
    }

    #[test]
    fn test_responses_api_output_item_done() {
        let chunk = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "first"},
                    {"type": "output_text", "text": " second"},
                ],
            },
        });
        let (content, _, _) =
            extract_response_content(Some(&chunk)).expect("should extract item content");
        assert_eq!(content, "first second");
    }

    /// The closing `*.done` events replay the whole answer; merging must count
    /// the deltas once and read the closing events only as a late-join
    /// fallback.
    #[test]
    fn test_merge_response_output_text_counts_deltas_once() {
        let chunks = vec![
            serde_json::json!({"type": "response.output_text.delta", "delta": "hello "}),
            serde_json::json!({"type": "response.output_text.delta", "delta": "there"}),
            serde_json::json!({"type": "response.output_text.done", "text": "hello there"}),
            serde_json::json!({"type": "response.output_item.done",
                "item": {"type": "message", "content": [{"text": "hello there"}]}}),
        ];
        let (content, reasoning, tool_calls) = merge_response_output_text(&chunks);
        assert_eq!(content, "hello there");
        assert!(reasoning.is_empty());
        assert!(tool_calls.is_empty());
    }

    /// A capture that joined late only has the closing events; they must still
    /// provide the answer. An empty delta carries no text and must not
    /// suppress that fallback.
    #[test]
    fn test_merge_response_output_text_falls_back_to_closing_events() {
        let only_done =
            vec![serde_json::json!({"type": "response.output_text.done", "text": "hello there"})];
        assert_eq!(merge_response_output_text(&only_done).0, "hello there");

        let only_item_done = vec![serde_json::json!({"type": "response.output_item.done",
            "item": {"type": "message", "content": [{"text": "hello there"}]}})];
        assert_eq!(merge_response_output_text(&only_item_done).0, "hello there");

        let empty_delta = vec![
            serde_json::json!({"type": "response.output_text.delta", "delta": ""}),
            serde_json::json!({"type": "response.output_text.done", "text": "hello there"}),
        ];
        assert_eq!(merge_response_output_text(&empty_delta).0, "hello there");
    }

    /// A late-join capture can hold both closing events and no delta at all:
    /// `response.output_text.done` and the message inside
    /// `response.output_item.done` each replay the whole answer. Both passed
    /// the "no delta" gate independently, so the merge appended the answer
    /// twice and doubled the estimated output tokens.
    #[test]
    fn test_merge_response_output_text_two_closing_events_count_once() {
        let chunks = vec![
            serde_json::json!({"type": "response.output_text.done", "text": "hello there"}),
            serde_json::json!({"type": "response.output_item.done",
                "item": {"type": "message", "content": [{"text": "hello there"}]}}),
        ];
        assert_eq!(merge_response_output_text(&chunks).0, "hello there");
    }

    /// A `choices[].message` snapshot repeats the answer so far (SysOM's
    /// Copilot stream sends nothing else), so the fold must count the answer
    /// once, not once per captured chunk.
    #[test]
    fn test_merge_response_output_text_counts_message_snapshots_once() {
        let chunks = vec![
            serde_json::json!({"choices":[{"message":{"content":"Hello"}}]}),
            serde_json::json!({"choices":[{"message":{"content":"Hello there"}}]}),
            serde_json::json!({"choices":[{"message":{"content":"Hello there!"},"finish_reason":"stop"}]}),
        ];
        let (content, reasoning, tool_calls) = merge_response_output_text(&chunks);
        assert_eq!(content, "Hello there!");
        assert!(reasoning.is_empty());
        assert!(tool_calls.is_empty());
    }

    /// An OpenAI-compatible gateway's final frame can be a whole `message`
    /// after a run of deltas; the snapshot completes the text instead of
    /// appending to it.
    #[test]
    fn test_merge_response_output_text_snapshot_completes_delta_text() {
        let chunks = vec![
            serde_json::json!({"choices":[{"delta":{"content":"Do"},"finish_reason":null}]}),
            serde_json::json!({"choices":[{"message":{"content":"Done"},"finish_reason":"stop"}]}),
        ];
        let (content, _, _) = merge_response_output_text(&chunks);
        assert_eq!(content, "Done");
    }

    /// A snapshot's tool calls are whole and repeat with every chunk; the
    /// last snapshot's calls must be counted once, not once per chunk.
    #[test]
    fn test_merge_response_output_text_snapshot_tool_calls_do_not_repeat() {
        let second = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [
                        {"index":0,"id":"c1","function":{
                            "name":"get_weather",
                            "arguments": serde_json::json!({"city":"Beijing"}).to_string()}}
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let chunks = vec![
            serde_json::json!({"choices":[{"message":{"content":"","tool_calls":[
                {"index":0,"id":"c1","function":{"name":"get_weather","arguments":"{}"}}]}}]}),
            second,
        ];
        let (content, _, tool_calls) = merge_response_output_text(&chunks);
        assert!(content.is_empty());
        assert_eq!(
            tool_calls,
            vec![format!(
                "get_weather: {}",
                serde_json::json!({"city":"Beijing"})
            )]
        );
    }

    /// Reasoning and tool fragments pass through the same merge unchanged.
    #[test]
    fn test_merge_response_output_text_keeps_reasoning_and_tool_fragments() {
        let chunks = vec![
            serde_json::json!({"type": "response.reasoning_text.delta", "delta": "think "}),
            serde_json::json!({"type": "response.reasoning_text.delta", "delta": "again"}),
            serde_json::json!({"type": "response.output_text.delta", "delta": "answer"}),
            serde_json::json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
            serde_json::json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "\"/tmp/a\"}"}}),
        ];
        let (content, reasoning, tool_calls) = merge_response_output_text(&chunks);
        assert_eq!(content, "answer");
        assert_eq!(reasoning, "think again");
        assert_eq!(tool_calls, vec!["{\"path\":", "\"/tmp/a\"}"]);
    }

    /// A reasoning item repeats the thinking, not the answer: it carries its
    /// text in `content[]` parts typed `reasoning_text` and, for the
    /// summary-only shape, in `summary[]` (the item shape the live message
    /// parser normalizes, `analyzer::message::openai`). Reading every part as
    /// answer text put the thinking in the answer buffer and, because the
    /// closing-event rule is keyed on the answer deltas, did not suppress it.
    #[test]
    fn test_responses_api_reasoning_item_done_is_reasoning() {
        let chunk = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "rs_101",
                "summary": [{"type": "summary_text", "text": " pondered"}],
                "content": [{"type": "reasoning_text", "text": "Think"}],
            },
        });
        let (content, reasoning, tool_calls) =
            extract_response_content(Some(&chunk)).expect("the item must extract");
        assert!(content.is_empty(), "a reasoning item is not answer text");
        assert_eq!(reasoning.as_deref(), Some("Think pondered"));
        assert!(tool_calls.is_empty());
    }

    /// A capture cut after the reasoning item and before the first answer
    /// delta — the case the drained and manual estimates exist for — has
    /// reasoning deltas plus the closing item and no answer delta. The item
    /// repeats what the deltas delivered, so it must be skipped rather than
    /// counted again; a late-joined capture has only the item and must still
    /// read it.
    #[test]
    fn test_merge_response_output_text_counts_a_reasoning_closing_item_once() {
        let cut = vec![
            serde_json::json!({"type": "response.reasoning_text.delta", "delta": "Think "}),
            serde_json::json!({"type": "response.reasoning_text.delta", "delta": "step by step"}),
            serde_json::json!({"type": "response.output_item.done",
                "item": {"type": "reasoning", "id": "rs_1",
                         "content": [{"type": "reasoning_text", "text": "Think step by step"}]}}),
        ];
        let (content, reasoning, _) = merge_response_output_text(&cut);
        assert!(content.is_empty(), "the thinking is not answer text");
        assert_eq!(reasoning, "Think step by step", "the thinking counts once");

        let closing_only = vec![serde_json::json!({"type": "response.output_item.done",
            "item": {"type": "reasoning", "id": "rs_2",
                     "content": [{"type": "reasoning_text", "text": "Think"}]}})];
        assert_eq!(merge_response_output_text(&closing_only).1, "Think");

        // The summary-only shape repeats its `summary[]` and its deltas are
        // the summary spelling.
        let summary_cut = vec![
            serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "concise"}),
            serde_json::json!({"type": "response.output_item.done",
                "item": {"type": "reasoning", "id": "rs_4",
                         "summary": [{"type": "summary_text", "text": "concise"}]}}),
        ];
        let (content, reasoning, _) = merge_response_output_text(&summary_cut);
        assert!(content.is_empty());
        assert_eq!(reasoning, "concise");

        // Answer deltas do not suppress the reasoning item's thinking.
        let with_answer = vec![
            serde_json::json!({"type": "response.output_item.done",
                "item": {"type": "reasoning", "id": "rs_3",
                         "content": [{"type": "reasoning_text", "text": "Think"}]}}),
            serde_json::json!({"type": "response.output_text.delta", "delta": "42"}),
        ];
        let (content, reasoning, _) = merge_response_output_text(&with_answer);
        assert_eq!(content, "42");
        assert_eq!(reasoning, "Think");
    }

    /// A refusal is the model's answer for that call: the Responses protocol
    /// streams it as `response.refusal.delta` and closes it with
    /// `response.refusal.done`, whose `refusal` field carries the finalized
    /// text (the shape the message parser reads). The extractor knew only the
    /// `output_text` events, so a refused call counted no output tokens.
    #[test]
    fn test_responses_api_refusal_events_yield_content() {
        let delta = serde_json::json!({"type": "response.refusal.delta", "delta": "I can't help"});
        let (content, reasoning, _) =
            extract_response_content(Some(&delta)).expect("the refusal delta must extract");
        assert_eq!(content, "I can't help");
        assert!(reasoning.is_none());

        let done = serde_json::json!({"type": "response.refusal.done", "refusal": "I can't help with that."});
        let (content, _, _) =
            extract_response_content(Some(&done)).expect("the closing refusal must extract");
        assert_eq!(content, "I can't help with that.");

        // The empty shapes keep yielding nothing.
        for chunk in [
            serde_json::json!({"type": "response.refusal.delta", "delta": ""}),
            serde_json::json!({"type": "response.refusal.done", "refusal": ""}),
            serde_json::json!({"type": "response.refusal.done"}),
        ] {
            assert!(
                extract_response_content(Some(&chunk)).is_none(),
                "{chunk} must stay unextracted"
            );
        }
    }

    /// The closing refusal repeats what the deltas delivered, so the merge
    /// must count it once; a late-joined capture (only the closing event)
    /// must still read it.
    #[test]
    fn test_merge_response_output_text_counts_a_refusal_once() {
        let complete = vec![
            serde_json::json!({"type": "response.refusal.delta", "delta": "I can't help"}),
            serde_json::json!({"type": "response.refusal.delta", "delta": " with that."}),
            serde_json::json!({"type": "response.refusal.done",
                "refusal": "I can't help with that."}),
        ];
        let (content, reasoning, tool_calls) = merge_response_output_text(&complete);
        assert_eq!(content, "I can't help with that.");
        assert!(reasoning.is_empty());
        assert!(tool_calls.is_empty());

        let closing_only = vec![serde_json::json!({"type": "response.refusal.done",
            "refusal": "I can't help with that."})];
        assert_eq!(
            merge_response_output_text(&closing_only).0,
            "I can't help with that."
        );
    }

    /// A capture that joined after `response.refusal.done` still sees the
    /// closing message item, whose content part is typed `refusal` and keeps
    /// the answer in `refusal` rather than `text`; reading only `text` counted
    /// that call as no output. When the deltas were captured, the item repeats
    /// them and must not count again.
    #[test]
    fn test_output_item_done_reads_a_refusal_part() {
        let item_done = serde_json::json!({"type": "response.output_item.done",
            "item": {"type": "message", "role": "assistant", "content": [
                {"type": "refusal", "refusal": "I can't help with that."}]}});

        let (content, _, _) = extract_response_content(Some(&item_done))
            .expect("the closing refusal item must extract");
        assert_eq!(content, "I can't help with that.");
        assert_eq!(
            merge_response_output_text(std::slice::from_ref(&item_done)).0,
            "I can't help with that."
        );

        let complete = vec![
            serde_json::json!({"type": "response.refusal.delta", "delta": "I can't help"}),
            serde_json::json!({"type": "response.refusal.delta", "delta": " with that."}),
            item_done,
        ];
        assert_eq!(
            merge_response_output_text(&complete).0,
            "I can't help with that."
        );
    }

    /// The Chat Completions protocols spell the same answer `refusal` next to
    /// a null `content`: `choices[].delta.refusal` on a stream and
    /// `choices[].message.refusal` in a whole body/snapshot.
    #[test]
    fn test_chat_refusal_is_read_as_content() {
        let response = serde_json::json!({
            "model": "gpt-4o",
            "choices": [{"message": {"role": "assistant", "content": null,
                                     "refusal": "I can't help with that."}}],
        });
        let (content, _, _) =
            extract_response_content(Some(&response)).expect("a refused body must extract");
        assert_eq!(content, "I can't help with that.");

        let chunks = vec![
            serde_json::json!({"choices": [{"delta": {"content": null, "refusal": "I can't"}}]}),
            serde_json::json!({"choices": [{"delta": {"content": null, "refusal": " help"}}]}),
            serde_json::json!({"choices": [{"message": {"content": null,
                                                       "refusal": "I can't help"}}]}),
        ];
        let (content, _, _) = merge_response_output_text(&chunks);
        assert_eq!(content, "I can't help");
    }

    #[test]
    fn test_responses_api_unknown_type_returns_none() {
        let chunk = serde_json::json!({
            "type": "response.created",
            "response": {"id": "abc"},
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    /// Responses streams a function call as `response.output_item.added`
    /// (carrying the name) followed by `response.function_call_arguments.delta`
    /// fragments. The chat-completions and Anthropic arms already produce the
    /// `name: arguments` text the manual counters tokenize; the Responses arm
    /// recognised neither event, so a tool-only turn counted zero output
    /// tokens.
    #[test]
    fn test_responses_api_function_call_events_yield_tool_call_fragments() {
        let added = serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "read_file",
            },
        });
        let (content, reasoning, tool_calls) =
            extract_response_content(Some(&added)).expect("the call's name must be extracted");
        assert_eq!(content, "");
        assert!(reasoning.is_none());
        assert_eq!(tool_calls, vec!["read_file: ".to_string()]);

        let first = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_1",
            "output_index": 0,
            "delta": "{\"path\":",
        });
        let (_, _, tool_calls) =
            extract_response_content(Some(&first)).expect("delta fragments must be extracted");
        assert_eq!(tool_calls, vec!["{\"path\":".to_string()]);

        let second = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_1",
            "output_index": 0,
            "delta": "\"/tmp/a.md\"}",
        });
        let (_, _, tool_calls) =
            extract_response_content(Some(&second)).expect("delta fragments must be extracted");
        assert_eq!(tool_calls, vec!["\"/tmp/a.md\"}".to_string()]);

        // The consumers join the fragments and split on the first ": " to
        // recover `name` and `arguments`; the pieces above must reconstruct the
        // exact string they expect.
        let joined = "read_file: ".to_string() + "{\"path\":" + "\"/tmp/a.md\"}";
        assert_eq!(joined, "read_file: {\"path\":\"/tmp/a.md\"}");
    }

    /// A custom (freeform) tool call is announced and streamed exactly like a
    /// function call, under `custom_tool_call` and
    /// `response.custom_tool_call_input.delta`. The arm only claimed
    /// `function_call`, so a truncated capture that caught the call counted
    /// nothing for it and the drained turn's output tokens came up short.
    #[test]
    fn test_responses_api_custom_tool_call_events_yield_tool_call_fragments() {
        let added = serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "custom_tool_call",
                "id": "ctc_1",
                "call_id": "call_patch",
                "name": "apply_patch",
            },
        });
        let (content, reasoning, tool_calls) =
            extract_response_content(Some(&added)).expect("the call's name must be extracted");
        assert_eq!(content, "");
        assert!(reasoning.is_none());
        assert_eq!(tool_calls, vec!["apply_patch: ".to_string()]);

        let delta = serde_json::json!({
            "type": "response.custom_tool_call_input.delta",
            "item_id": "ctc_1",
            "output_index": 0,
            "delta": "*** Begin Patch\n",
        });
        let (_, _, tool_calls) =
            extract_response_content(Some(&delta)).expect("input fragments must be extracted");
        assert_eq!(tool_calls, vec!["*** Begin Patch\n".to_string()]);
    }

    /// A message item that arrives after the call is announced must keep
    /// counting as text: the new arm only claims function-call items.
    #[test]
    fn test_responses_api_output_item_added_message_is_not_a_tool_call() {
        let chunk = serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "message", "role": "assistant", "content": []},
        });
        let extracted = extract_response_content(Some(&chunk));
        assert!(
            extracted.is_none_or(|(_, _, calls)| calls.is_empty()),
            "a message item must not be reported as a tool call"
        );
    }

    /// Anthropic streams content as `content_block_delta` events; the drained
    /// and analyze-chatml paths aggregate them, so the shared extractor must
    /// not count the whole response as zero output tokens.
    #[test]
    fn test_anthropic_text_delta() {
        let chunk = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hel"},
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract the text delta");
        assert_eq!(content, "Hel");
        assert!(reasoning.is_none());
        assert!(tools.is_empty());
    }

    #[test]
    fn test_anthropic_thinking_delta() {
        let chunk = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "step by step"},
        });
        let (content, reasoning, _) =
            extract_response_content(Some(&chunk)).expect("should extract the thinking delta");
        assert!(content.is_empty());
        assert_eq!(reasoning.as_deref(), Some("step by step"));
    }

    #[test]
    fn test_anthropic_input_json_delta() {
        let chunk = serde_json::json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"},
        });
        let (content, _, tools) =
            extract_response_content(Some(&chunk)).expect("should extract the tool input fragment");
        assert!(content.is_empty());
        assert_eq!(tools, vec!["{\"city\":".to_string()]);
    }

    #[test]
    fn test_anthropic_non_content_events_are_not_extracted() {
        // Guard: a message_start/message_delta/ping carries no assistant text
        // and must keep returning None instead of a fabricated empty payload.
        for chunk in [
            serde_json::json!({"type": "message_start", "message": {"id": "msg_1"}}),
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            serde_json::json!({"type": "ping"}),
        ] {
            assert!(
                extract_response_content(Some(&chunk)).is_none(),
                "{chunk} must stay unextracted"
            );
        }
    }

    #[test]
    fn test_responses_api_reasoning_text_delta() {
        let chunk = serde_json::json!({
            "type": "response.reasoning_text.delta",
            "delta": "thinking...",
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract reasoning");
        assert!(content.is_empty());
        assert_eq!(reasoning, Some("thinking...".to_string()));
        assert!(tools.is_empty());
    }

    #[test]
    fn test_responses_api_reasoning_summary_text_delta() {
        let chunk = serde_json::json!({
            "type": "response.reasoning_summary_text.delta",
            "delta": "summary...",
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract reasoning summary");
        assert!(content.is_empty());
        assert_eq!(reasoning, Some("summary...".to_string()));
        assert!(tools.is_empty());
    }

    #[test]
    fn test_responses_api_output_text_delta_empty() {
        let chunk = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "",
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    #[test]
    fn test_responses_api_output_text_done_empty() {
        let chunk = serde_json::json!({
            "type": "response.output_text.done",
            "text": "",
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    #[test]
    fn test_responses_api_output_item_done_empty() {
        let chunk = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [],
            },
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    #[test]
    fn test_extract_response_with_tool_calls() {
        let response = serde_json::json!({
            "model": "gpt-4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "tc_1",
                        "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}
                    }]
                }
            }]
        });
        let token_data = extract_token_data(None, Some(&response)).unwrap();
        assert_eq!(token_data.tool_calls.len(), 1);
        assert!(token_data.tool_calls[0].contains("search"));
    }

    #[test]
    fn test_extract_sse_reasoning_content_accumulation() {
        let chunk = serde_json::json!({
            "model": "qwen",
            "choices": [
                {"delta": {"content": "a", "reasoning_content": "think1"}},
                {"delta": {"content": "b", "reasoning_content": "think2"}}
            ]
        });
        let (content, reasoning, _) =
            extract_response_content(Some(&chunk)).expect("should extract");
        assert_eq!(content, "ab");
        assert_eq!(reasoning, Some("think1think2".to_string()));
    }

    #[test]
    fn test_extract_tool_calls_skips_missing_function() {
        let response = serde_json::json!({
            "model": "gpt-4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "tool_calls": [{"id": "tc_1"}]
                }
            }]
        });
        let (content, _, tool_calls) =
            extract_response_content(Some(&response)).expect("should extract content");
        assert_eq!(content, "answer");
        assert!(
            tool_calls.is_empty(),
            "tool_call without function should be skipped"
        );
    }

    /// SysOM's Copilot streams spell the assistant's tool calls
    /// `choices[].message.tool_use` — an array of
    /// `{index, id, type, function: {name, arguments}}` — instead of the
    /// OpenAI `tool_calls`. The extractor read only the OpenAI spelling, so
    /// a SysOM tool call counted zero tool tokens in the manual counters and
    /// the drain estimate.
    #[test]
    fn sysom_tool_use_snapshots_yield_tool_calls() {
        let response = serde_json::json!({
            "model": "qwen3-coder-plus",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_use": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"file_path\":\"/tmp/a.md\"}"}
                    }]
                }
            }]
        });
        let (_, _, tool_calls) =
            extract_response_content(Some(&response)).expect("a SysOM tool call must count");
        assert_eq!(
            tool_calls,
            vec!["read_file: {\"file_path\":\"/tmp/a.md\"}".to_string()]
        );
    }

    /// A SysOM snapshot can carry text and tool calls together: both feed the
    /// counters (the text as content, the call as its `name: arguments`
    /// string), exactly like an OpenAI message carrying both.
    #[test]
    fn sysom_tool_use_alongside_content_is_extracted() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Reading the file first.",
                    "tool_use": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"file_path\":\"/tmp/a.md\"}"}
                    }]
                }
            }]
        });
        let (content, _, tool_calls) =
            extract_response_content(Some(&response)).expect("text and calls must both count");
        assert_eq!(content, "Reading the file first.");
        assert_eq!(tool_calls.len(), 1);
        assert!(tool_calls[0].starts_with("read_file: "));
    }

    /// Anthropic streams deliver the answer as `content_block_delta` events and
    /// complete bodies as `content[]` blocks; neither shape was recognised, so
    /// the manual counters recorded a real answer as zero output tokens.
    #[test]
    fn anthropic_shapes_yield_content_reasoning_and_tool_calls() {
        let delta = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "hello there"}
        });
        let (content, reasoning, tool_calls) =
            extract_response_content(Some(&delta)).expect("text delta extracts");
        assert_eq!(content, "hello there");
        assert!(reasoning.is_none());
        assert!(tool_calls.is_empty());

        let thinking = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "let me think"}
        });
        let (_, reasoning, _) =
            extract_response_content(Some(&thinking)).expect("thinking extracts");
        assert_eq!(reasoning.as_deref(), Some("let me think"));

        let body = serde_json::json!({
            "id": "msg_1",
            "content": [
                {"type": "text", "text": "the answer"},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                 "input": {"city": "Paris"}}
            ]
        });
        let (content, _, tool_calls) =
            extract_response_content(Some(&body)).expect("complete body extracts");
        assert_eq!(content, "the answer");
        assert_eq!(tool_calls.len(), 1);
        assert!(tool_calls[0].starts_with("get_weather: "));
    }
}
