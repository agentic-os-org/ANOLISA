//! Converter from AgentSight SQLite GenAI data to the shared ATIF schema.
//!
//! Reconstructs an incremental step sequence from a series of LLM call records.
//! Each LLM call maps to an agent step, with system/user steps extracted from
//! the first call's context. Tool call observations are correlated across
//! consecutive calls.
//!
//! The output model comes from the `agentsight-atif` crate, so eBPF exports and
//! collector-ingested trajectories share one wire format.

use std::collections::HashMap;

use agentsight_atif::{
    ATIF_SCHEMA_VERSION, Agent, AtifTrajectory, EXTRA_IS_ERROR, FinalMetrics, Metrics, Observation,
    ObservationResult, Step, StepSource, ToolCall, same_call_id,
};

use crate::genai::semantic::{
    GenAISemanticEvent, InputMessage, LLMCall, MessagePart, OutputMessage,
};
use crate::storage::sqlite::genai::TraceEventDetail;

/// Build a plain text step (system prompt or user query) with no agent payload.
fn text_step(step_id: usize, source: StepSource, timestamp_ns: i64, message: String) -> Step {
    Step {
        step_id,
        source,
        message,
        timestamp: Some(ns_to_iso8601(timestamp_ns as u64)),
        model_name: None,
        reasoning_effort: None,
        reasoning_content: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        extra: None,
        llm_call_count: None,
        is_copied_context: None,
    }
}

/// Convert a single trace (one user query's call chain) into an ATIF document.
pub fn convert_trace_to_atif(
    trace_id: &str,
    events: Vec<TraceEventDetail>,
) -> Result<AtifTrajectory, Box<dyn std::error::Error>> {
    if events.is_empty() {
        return Err("no events found for trace".into());
    }

    let parsed = parse_all_events(&events);
    let agent = build_agent_metadata(&events, &parsed);

    let mut step_counter: usize = 0;
    let mut steps = Vec::new();

    // 1. System prompt step
    if let Some(system_text) = extract_system_prompt(&events[0], parsed[0].as_ref()) {
        if !system_text.is_empty() {
            step_counter += 1;
            steps.push(text_step(
                step_counter,
                StepSource::System,
                events[0].start_timestamp_ns,
                system_text,
            ));
        }
    }

    // 2. User query step
    if let Some(user_text) = extract_user_query(&events[0], parsed[0].as_ref()) {
        if !user_text.is_empty() {
            step_counter += 1;
            steps.push(text_step(
                step_counter,
                StepSource::User,
                events[0].start_timestamp_ns,
                user_text,
            ));
        }
    }

    // 3. Agent steps
    for i in 0..events.len() {
        step_counter += 1;
        let step = build_agent_step(
            step_counter,
            &events[i],
            parsed.get(i).and_then(|p| p.as_ref()),
            // Next event for observation correlation
            if i + 1 < events.len() {
                Some((&events[i + 1], parsed.get(i + 1).and_then(|p| p.as_ref())))
            } else {
                None
            },
        );
        steps.push(step);
    }

    // 4. Final metrics
    let final_metrics = compute_final_metrics(&events, steps.len());

    Ok(trajectory(trace_id, agent, steps, final_metrics))
}

/// Convert a full session (all traces) into an ATIF document.
pub fn convert_session_to_atif(
    session_id: &str,
    events: Vec<TraceEventDetail>,
) -> Result<AtifTrajectory, Box<dyn std::error::Error>> {
    if events.is_empty() {
        return Err("no events found for session".into());
    }

    let parsed = parse_all_events(&events);
    let agent = build_agent_metadata(&events, &parsed);

    let mut step_counter: usize = 0;
    let mut steps = Vec::new();
    let mut last_system_text: Option<String> = None;

    // Group events into conversation turns, preserving order
    let trace_groups = group_by_conversation(&events, &parsed);

    for (trace_events, trace_parsed) in &trace_groups {
        if trace_events.is_empty() {
            continue;
        }

        // System prompt: emit if changed or first time
        if let Some(system_text) = extract_system_prompt(
            trace_events[0],
            trace_parsed.first().and_then(|p| p.as_ref()),
        ) {
            if !system_text.is_empty() && last_system_text.as_deref() != Some(&system_text) {
                step_counter += 1;
                steps.push(text_step(
                    step_counter,
                    StepSource::System,
                    trace_events[0].start_timestamp_ns,
                    system_text.clone(),
                ));
                last_system_text = Some(system_text);
            }
        }

        // User query step
        if let Some(user_text) = extract_user_query(
            trace_events[0],
            trace_parsed.first().and_then(|p| p.as_ref()),
        ) {
            if !user_text.is_empty() {
                step_counter += 1;
                steps.push(text_step(
                    step_counter,
                    StepSource::User,
                    trace_events[0].start_timestamp_ns,
                    user_text,
                ));
            }
        }

        // Agent steps
        for i in 0..trace_events.len() {
            step_counter += 1;
            let next = if i + 1 < trace_events.len() {
                Some((
                    trace_events[i + 1],
                    trace_parsed.get(i + 1).and_then(|p| p.as_ref()),
                ))
            } else {
                None
            };
            let step = build_agent_step(
                step_counter,
                trace_events[i],
                trace_parsed.get(i).and_then(|p| p.as_ref()),
                next,
            );
            steps.push(step);
        }
    }

    let final_metrics = compute_final_metrics(&events, steps.len());

    Ok(trajectory(session_id, agent, steps, final_metrics))
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Assemble the root document. Only the fields AgentSight can populate are set;
/// the rest of the ATIF surface (trajectory_id, notes, continuation refs) stays
/// absent rather than empty.
fn trajectory(
    session_id: &str,
    agent: Agent,
    steps: Vec<Step>,
    final_metrics: FinalMetrics,
) -> AtifTrajectory {
    AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.to_string(),
        agent,
        steps,
        session_id: Some(session_id.to_string()),
        trajectory_id: None,
        notes: None,
        final_metrics: Some(final_metrics),
        continued_trajectory_ref: None,
        subagent_trajectories: None,
        extra: None,
    }
}

/// Parse event_json for all events upfront. Returns a Vec of Option<LLMCall>.
fn parse_all_events(events: &[TraceEventDetail]) -> Vec<Option<LLMCall>> {
    events.iter().map(parse_event_json).collect()
}

/// Try to deserialize event_json into an LLMCall.
fn parse_event_json(event: &TraceEventDetail) -> Option<LLMCall> {
    let json_str = event.event_json.as_deref()?;
    // event_json is stored as GenAISemanticEvent enum
    let semantic: GenAISemanticEvent = serde_json::from_str(json_str).ok()?;
    match semantic {
        GenAISemanticEvent::LLMCall(call) => Some(call),
        _ => None,
    }
}

/// Group events into conversation turns, preserving chronological order.
/// Returns Vec of (events_in_turn, parsed_in_turn).
///
/// The grouping key is `conversation_id` — the user-query fingerprint that
/// ties every LLM call of one agent turn together. It is deliberately NOT
/// `trace_id`: that is the per-call API response id (see
/// `TraceEventDetail::trace_id`), so keying on it would isolate every call
/// in its own group and sever the cross-call tool-observation correlation
/// the caller performs within each group.
///
/// Events with no conversation id cannot be attributed to a turn; each
/// becomes its own singleton group rather than merging under one shared
/// key, which would correlate unrelated calls with each other merely
/// because both ids are missing.
fn group_by_conversation<'a>(
    events: &'a [TraceEventDetail],
    parsed: &'a [Option<LLMCall>],
) -> Vec<(Vec<&'a TraceEventDetail>, Vec<&'a Option<LLMCall>>)> {
    // Group index per conversation id, so turns keep first-appearance order
    // while their events accumulate.
    let mut conv_index: HashMap<&str, usize> = HashMap::new();
    let mut index_groups: Vec<Vec<usize>> = Vec::new();

    for (i, event) in events.iter().enumerate() {
        match event.conversation_id.as_deref() {
            Some(id) => {
                let idx = match conv_index.get(id) {
                    Some(&idx) => idx,
                    None => {
                        index_groups.push(Vec::new());
                        let idx = index_groups.len() - 1;
                        conv_index.insert(id, idx);
                        idx
                    }
                };
                index_groups[idx].push(i);
            }
            // Unknown turn: never assume two id-less events belong together.
            None => index_groups.push(vec![i]),
        }
    }

    index_groups
        .into_iter()
        .map(|indices| {
            let evts: Vec<_> = indices.iter().map(|&i| &events[i]).collect();
            let prs: Vec<_> = indices.iter().map(|&i| &parsed[i]).collect();
            (evts, prs)
        })
        .collect()
}

/// Build agent metadata from events.
fn build_agent_metadata(events: &[TraceEventDetail], parsed: &[Option<LLMCall>]) -> Agent {
    // Agent name: first non-None agent_name, fallback to process_name
    let name = events
        .iter()
        .find_map(|e| e.agent_name.clone())
        .or_else(|| events.iter().find_map(|e| e.process_name.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    // Model name: most frequent model
    let model_name = most_frequent_model(events);

    // Collect tool definitions from parsed calls
    let tool_definitions = collect_tool_definitions(parsed);

    Agent {
        name,
        // The trace records no agent version, so the export states the
        // schema's own fallback (`Agent::default_version`) rather than
        // asserting a number nothing measured — the ATIF viewer renders this
        // field as the observed agent's version.
        version: "unknown".to_string(),
        model_name,
        tool_definitions,
        extra: None,
    }
}

/// Find the most frequently used model across events.
///
/// Counts come from a `HashMap`, whose iteration order is randomized per map:
/// a single-key max reported a different model for equally used ones, so ties
/// are broken by name.
fn most_frequent_model(events: &[TraceEventDetail]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for e in events {
        if let Some(ref m) = e.model {
            *counts.entry(m.as_str()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .min_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)))
        .map(|(m, _)| m.to_string())
}

/// Collect unique tool definitions from all parsed LLM calls.
///
/// Deduplication is keyed on the tool's name when the request carries one
/// (OpenAI's `function.name`, Anthropic's `name`), and on the definition
/// itself otherwise: a definition need not have a name at all — the Responses
/// API's built-in tools are `{"type": "web_search"}` / `{"type": "file_search"}`
/// (platform.openai.com/docs/guides/tools) — and every nameless definition
/// used to key on the empty string, so the first one took the slot and the
/// rest were dropped from the exported agent's tool list. A definition whose
/// text is genuinely identical still collapses to one entry.
fn collect_tool_definitions(parsed: &[Option<LLMCall>]) -> Option<Vec<serde_json::Value>> {
    let mut seen = std::collections::HashSet::new();
    let mut defs = Vec::new();

    for call in parsed.iter().filter_map(|p| p.as_ref()) {
        if let Some(ref tools) = call.request.tools {
            for tool in tools {
                // Extract tool name for deduplication
                let key = tool
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .or_else(|| tool.get("name"))
                    .and_then(|n| n.as_str())
                    .map_or_else(|| tool.to_string(), str::to_string);
                if seen.insert(key) {
                    defs.push(tool.clone());
                }
            }
        }
    }

    if defs.is_empty() { None } else { Some(defs) }
}

/// Extract system prompt text from event data.
fn extract_system_prompt(event: &TraceEventDetail, parsed: Option<&LLMCall>) -> Option<String> {
    // Strategy 1: from parsed LLMCall request messages
    if let Some(call) = parsed {
        let text = extract_text_by_role(&call.request.messages, "system");
        if !text.is_empty() {
            return Some(text);
        }
    }

    // Strategy 2: from system_instructions column (JSON array of InputMessage)
    if let Some(ref json) = event.system_instructions {
        if let Some(msgs) = crate::genai::semantic::input_messages_from_column(json) {
            let text = extract_text_from_input_messages(&msgs, "system");
            if !text.is_empty() {
                return Some(text);
            }
        }
        // Might be a plain string
        if let Ok(s) = serde_json::from_str::<String>(json) {
            if !s.is_empty() {
                return Some(s);
            }
        }
    }

    None
}

/// Extract user query text from event data.
fn extract_user_query(event: &TraceEventDetail, parsed: Option<&LLMCall>) -> Option<String> {
    // Strategy 1: user_query column (already cleaned by builder)
    if let Some(ref q) = event.user_query {
        if !q.is_empty() {
            return Some(q.clone());
        }
    }

    // Strategy 2: from parsed LLMCall — last user message text
    if let Some(call) = parsed {
        let text = extract_last_user_text(&call.request.messages);
        if let Some(t) = text {
            return Some(t);
        }
    }

    // Strategy 3: from input_messages column
    if let Some(ref json) = event.input_messages {
        if let Some(msgs) = crate::genai::semantic::input_messages_from_column(json) {
            let text = extract_last_user_text_from_input(&msgs);
            if let Some(t) = text {
                return Some(t);
            }
        }
    }

    None
}

/// Build an agent step from a single LLM call event.
fn build_agent_step(
    step_id: usize,
    event: &TraceEventDetail,
    parsed: Option<&LLMCall>,
    next: Option<(&TraceEventDetail, Option<&LLMCall>)>,
) -> Step {
    let mut message_text = String::new();
    let mut reasoning_text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    // Extract from parsed response messages
    if let Some(call) = parsed {
        for msg in &call.response.messages {
            for part in &msg.parts {
                match part {
                    MessagePart::Text { content } => {
                        if !content.is_empty() {
                            if !message_text.is_empty() {
                                message_text.push('\n');
                            }
                            message_text.push_str(content);
                        }
                    }
                    MessagePart::Reasoning { content } => {
                        if !content.is_empty() {
                            if !reasoning_text.is_empty() {
                                reasoning_text.push('\n');
                            }
                            reasoning_text.push_str(content);
                        }
                    }
                    MessagePart::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        let tc_id = id
                            .clone()
                            .unwrap_or_else(|| format!("auto_{step_id}_{}", tool_calls.len()));
                        tool_calls.push(ToolCall {
                            tool_call_id: tc_id,
                            function_name: name.clone(),
                            arguments: arguments
                                .clone()
                                .unwrap_or(serde_json::Value::Object(Default::default())),
                            extra: None,
                        });
                    }
                    _ => {}
                }
            }
        }
    } else {
        // Fallback: parse output_messages column directly
        if let Some(ref json) = event.output_messages {
            if let Ok(msgs) = serde_json::from_str::<Vec<OutputMessage>>(json) {
                for msg in &msgs {
                    for part in &msg.parts {
                        match part {
                            MessagePart::Text { content } => {
                                if !content.is_empty() {
                                    if !message_text.is_empty() {
                                        message_text.push('\n');
                                    }
                                    message_text.push_str(content);
                                }
                            }
                            MessagePart::Reasoning { content } => {
                                if !content.is_empty() {
                                    if !reasoning_text.is_empty() {
                                        reasoning_text.push('\n');
                                    }
                                    reasoning_text.push_str(content);
                                }
                            }
                            MessagePart::ToolCall {
                                id,
                                name,
                                arguments,
                            } => {
                                let tc_id = id.clone().unwrap_or_else(|| {
                                    format!("auto_{step_id}_{}", tool_calls.len())
                                });
                                tool_calls.push(ToolCall {
                                    tool_call_id: tc_id,
                                    function_name: name.clone(),
                                    arguments: arguments
                                        .clone()
                                        .unwrap_or(serde_json::Value::Object(Default::default())),
                                    extra: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Observation: correlate tool_calls with responses from next event
    let observation = if !tool_calls.is_empty() {
        build_observation(&tool_calls, next)
    } else {
        None
    };

    // Metrics
    let metrics = Some(Metrics {
        prompt_tokens: if event.input_tokens > 0 {
            Some(event.input_tokens as u64)
        } else {
            None
        },
        completion_tokens: if event.output_tokens > 0 {
            Some(event.output_tokens as u64)
        } else {
            None
        },
        cached_tokens: event
            .cache_read_tokens
            .and_then(|v| if v > 0 { Some(v as u64) } else { None }),
        cost_usd: None,
        logprobs: None,
        completion_token_ids: None,
        prompt_token_ids: None,
        extra: None,
    });

    // Timestamp: prefer end_timestamp (when response arrived)
    let timestamp_ns = event.end_timestamp_ns.unwrap_or(event.start_timestamp_ns);

    // Request start time lets ATIF consumers derive per-step model inference
    // time (end − start) and tool windows (next start − prev end).
    let extra = HashMap::from([(
        "start_timestamp".to_string(),
        serde_json::Value::String(ns_to_iso8601(event.start_timestamp_ns as u64)),
    )]);

    Step {
        step_id,
        source: StepSource::Agent,
        message: message_text,
        timestamp: Some(ns_to_iso8601(timestamp_ns as u64)),
        model_name: event.model.clone(),
        reasoning_effort: None,
        reasoning_content: if reasoning_text.is_empty() {
            None
        } else {
            Some(reasoning_text)
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        observation,
        metrics,
        extra: Some(extra),
        llm_call_count: None,
        is_copied_context: None,
    }
}

/// Extract only the latest-round tool responses from a full message history.
/// In an agentic loop the pattern is:
///   ... | assistant(tool_calls) | tool | tool | ... | assistant(tool_calls) | tool | tool
/// The tool responses for the *current* step are the `tool`-role messages that
/// follow the last `assistant` message which contains ToolCall parts.
fn latest_round_messages(messages: &[InputMessage]) -> &[InputMessage] {
    // Find the last assistant message that contains a ToolCall part.
    let last_assistant_with_tc = messages.iter().rposition(|m| {
        m.role == "assistant"
            && m.parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCall { .. }))
    });

    if let Some(idx) = last_assistant_with_tc {
        // Return messages after that assistant message (the tool responses).
        &messages[idx + 1..]
    } else {
        // No assistant-with-tool-call found; fall back to messages after last user.
        if let Some(idx) = messages.iter().rposition(|m| m.role == "user") {
            &messages[idx + 1..]
        } else {
            messages
        }
    }
}

/// The tail of a replayed history from its last tool call on — the round whose
/// responses that history carries. Unlike [`latest_round_messages`] there is no
/// user-message fallback: a column that carries only the responses themselves
/// (the measured live shape, where the result rides on the user turn) holds no
/// assistant turn and must be scanned whole.
fn messages_after_the_last_tool_call(messages: &[InputMessage]) -> &[InputMessage] {
    let last_tool_call = messages.iter().rposition(|m| {
        m.role == "assistant"
            && m.parts
                .iter()
                .any(|p| matches!(p, MessagePart::ToolCall { .. }))
    });
    match last_tool_call {
        Some(idx) => &messages[idx + 1..],
        None => messages,
    }
}

/// Build observation by looking for ToolCallResponse in the next event's input.
fn build_observation(
    tool_calls: &[ToolCall],
    next: Option<(&TraceEventDetail, Option<&LLMCall>)>,
) -> Option<Observation> {
    let (next_event, next_parsed) = next?;

    let tc_ids: HashMap<&str, usize> = tool_calls
        .iter()
        .enumerate()
        .map(|(i, tc)| (tc.tool_call_id.as_str(), i))
        .collect();

    let mut results: Vec<ObservationResult> = Vec::new();
    let mut matched_by_id = vec![false; tool_calls.len()];

    // Strategy 1: from parsed LLMCall's full request messages (latest round only)
    if let Some(call) = next_parsed {
        let latest = latest_round_messages(&call.request.messages);
        collect_tool_responses(latest, &tc_ids, &mut results, &mut matched_by_id);
    } else {
        // Strategy 2: from event_json parsed as LLMCall (latest round only)
        if let Some(call) = parse_event_json(next_event) {
            let latest = latest_round_messages(&call.request.messages);
            collect_tool_responses(latest, &tc_ids, &mut results, &mut matched_by_id);
        }
    }

    // Strategy 3: from input_messages column. Narrowed to the round it
    // carries: a completed row keeps only the current round there, but a row
    // that never completed keeps the whole replayed history, and a multi-round
    // turn keeps every round of the turn — an earlier round's id-less response
    // would otherwise take this step's call through the positional fallback.
    if results.is_empty() {
        if let Some(ref json) = next_event.input_messages {
            if let Some(msgs) = crate::genai::semantic::input_messages_from_column(json) {
                let latest = messages_after_the_last_tool_call(&msgs);
                collect_tool_responses(latest, &tc_ids, &mut results, &mut matched_by_id);
            }
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(Observation { results })
    }
}

/// Flatten a tool-result payload into the text downstream consumers read.
///
/// Anthropic writes `tool_result.content` as an array of content blocks as soon
/// as a result carries more than one piece of text — an MCP server, or a
/// command whose streams were captured separately, returns
/// `[{"type":"text","text":…},{"type":"text","text":…}]` — and
/// `GenAIBuilder::anthropic_content_block_to_part` passes that array through
/// unchanged.
///
/// Serialising it whole left the observation as JSON punctuation:
/// `outcome::classify_call` looks for a line that *starts* with an error token
/// and saw `[{"type":"text"`, so a marked failure fell through to the weak text
/// heuristics, and the evidence pool's haystack carried the encoding instead of
/// the output, so a value the tool printed could not ground a later claim. The
/// local collector's converter already flattens this shape in
/// `flatten_tool_result_content`; both converters must produce the same text
/// for the same capture.
///
/// Non-text blocks (an image, a document) have no textual form to contribute
/// and are skipped rather than rendered as empty strings, so the joining
/// newlines stay meaningful. Anything that is not a block array keeps its
/// previous rendering, including a string payload, which is returned verbatim.
fn flatten_result_content(payload: &serde_json::Value) -> String {
    if let serde_json::Value::Array(blocks) = payload {
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|block| {
                let block = block.as_object()?;
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    match payload {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Scan input messages for ToolCallResponse parts, matching by tool_call_id.
///
/// Role is deliberately not filtered. Agents disagree about where a tool result
/// belongs: some emit a `tool`-role message, others attach the response to the
/// following `user` turn. Measured against a live capture, every
/// `tool_call_response` sat on a `user` message and none on a `tool` message, so
/// gating on role dropped all of them and left every observation empty. The part
/// type is the reliable discriminator — `ToolCallResponse` is the only variant
/// carrying a result, and assistant turns only ever carry `ToolCall`.
///
/// Matching runs in two passes: every response that names a call through its id
/// claims that call first, and only the responses the ids could not place are
/// matched positionally. A single in-order pass lets an id-less response steal
/// the slot of a call whose id-bearing response has not been seen yet — parallel
/// tools complete out of call order, so the replayed round cannot be assumed to
/// follow it — swapping the two results while both still claim the stolen id.
fn collect_tool_responses(
    messages: &[InputMessage],
    tc_ids: &HashMap<&str, usize>,
    results: &mut Vec<ObservationResult>,
    matched: &mut [bool],
) {
    // Flatten the responses in message order once; both passes work over the
    // same list.
    let mut pending: Vec<(
        Option<&str>,
        String,
        Option<HashMap<String, serde_json::Value>>,
    )> = Vec::new();
    for msg in messages {
        for part in &msg.parts {
            if let MessagePart::ToolCallResponse { id, response } = part {
                // Providers report tool failure out of band: Anthropic wraps a
                // failed result as `{"content": …, "is_error": true}`, real
                // agent traces carry the camelCase `isError`, and some tools
                // report an `error` status instead. Keep the flag as structured
                // data so consumers never have to re-derive failure from the
                // flattened text — the interruption detector and the grader
                // read the same spellings and this was the only reader that
                // knew just the first two.
                //
                // The third spelling is the opposite polarity: some tools
                // report `success` instead of a flag, and it is read the same
                // way `declared_failure` reads it — `success: false` is a
                // failure, and the error status still overrides either
                // polarity.
                //
                // An `error` status is read before the boolean rather than
                // after it: a payload that says `is_error: false` and
                // `status: error` contradicts itself, and both of those
                // readers resolve the contradiction towards the failure. A
                // plain `or_else` chain never reached the status arm once the
                // boolean answered, so the certain failure arrived at the
                // classifier carrying an explicit success flag.
                // The Responses API copies `function_call_output.output`
                // through unchanged and `parse_request_body` clones it into the
                // part without re-parsing, so the envelope can arrive as a
                // JSON-encoded *string* rather than an object. The detector and
                // the grader both re-parse that shape before judging it; read
                // the same envelope here so the same bytes get the same
                // outcome. Only an object envelope is looked through — a
                // plain string payload keeps its previous rendering, which is
                // what the flattening below expects.
                let encoded = match response {
                    serde_json::Value::String(text) => {
                        serde_json::from_str::<serde_json::Value>(text.trim())
                            .ok()
                            .filter(serde_json::Value::is_object)
                    }
                    _ => None,
                };
                let envelope = encoded.as_ref().unwrap_or(response);
                let status_is_error = envelope
                    .get("status")
                    .and_then(|v| v.as_str())
                    .is_some_and(|status| status.eq_ignore_ascii_case("error"));
                let declared = envelope
                    .get(EXTRA_IS_ERROR)
                    .or_else(|| envelope.get("isError"))
                    .and_then(|v| v.as_bool())
                    .or_else(|| {
                        envelope
                            .get("success")
                            .and_then(|v| v.as_bool())
                            .map(|success| !success)
                    });
                let is_error = declared
                    .map(|is_error| is_error || status_is_error)
                    .or_else(|| status_is_error.then_some(true));
                let extra = is_error.map(|flag| {
                    HashMap::from([(EXTRA_IS_ERROR.to_string(), serde_json::Value::Bool(flag))])
                });

                // ATIF allows any JSON for observation content, but downstream
                // consumers (analyzers, dashboard) render it as text — flatten
                // structured responses to a JSON string rather than nesting.
                //
                // The error envelope is unwrapped rather than serialised whole:
                // classification looks for lines that *start* with an error
                // token, and a `{"content":"Error: …` prefix defeats that check
                // on precisely the results the flag marks as failures.
                let payload = if is_error.is_some() {
                    envelope.get("content").unwrap_or(response)
                } else {
                    response
                };
                let content_str = flatten_result_content(payload);
                pending.push((id.as_deref(), content_str, extra));
            }
        }
    }

    // Pass 1 — an id that names a call is authoritative: the response it rides
    // on belongs to that call whatever order the two arrived in, tolerating the
    // separator differences some agents introduce when echoing a call id back.
    for (id, content_str, extra) in &pending {
        let Some(tc_id) = id else { continue };
        let found = tc_ids
            .iter()
            .find(|(known, _)| same_call_id(known, tc_id))
            .map(|(_, idx)| *idx);
        let Some(idx) = found else { continue };
        if matched[idx] {
            continue;
        }
        matched[idx] = true;
        results.push(ObservationResult {
            source_call_id: Some(tc_id.to_string()),
            content: Some(serde_json::Value::String(content_str.clone())),
            subagent_trajectory_ref: None,
            extra: extra.clone(),
        });
    }

    // Pass 2 — only id-less responses can use positional fallback. Foreign
    // and duplicate ids cannot describe an unmatched current call; consuming
    // a slot for them would discard that call's genuine id-less response.
    let mut positional_idx: usize = 0;
    for (id, content_str, extra) in &pending {
        if id.is_some() {
            continue;
        }
        while positional_idx < matched.len() && matched[positional_idx] {
            positional_idx += 1;
        }
        if positional_idx >= matched.len() {
            // No unmatched call is left; the response cannot be attributed.
            break;
        }
        let source_call_id = tc_ids
            .iter()
            .find_map(|(known, idx)| (*idx == positional_idx).then(|| (*known).to_string()));
        matched[positional_idx] = true;
        results.push(ObservationResult {
            source_call_id,
            content: Some(serde_json::Value::String(content_str.clone())),
            subagent_trajectory_ref: None,
            extra: extra.clone(),
        });
        positional_idx += 1;
    }
}

/// Compute aggregated final metrics.
///
/// The counters come from `genai_events` as `i64` and are only added when they
/// are positive, the guard the per-step [`Metrics`] use: a non-positive value
/// is corrupt data (the events writer stores the provider's `u64` with
/// `as i64`, so a count above `i64::MAX` lands negative), and adding it as a
/// `u64` wrapped `total_prompt_tokens` to about 1.8e19 — or panicked on the
/// next addend in a build with overflow checks.
fn compute_final_metrics(events: &[TraceEventDetail], total_steps: usize) -> FinalMetrics {
    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut total_cached: u64 = 0;

    for e in events {
        if e.input_tokens > 0 {
            total_prompt += e.input_tokens as u64;
        }
        if e.output_tokens > 0 {
            total_completion += e.output_tokens as u64;
        }
        if let Some(c) = e.cache_read_tokens {
            if c > 0 {
                total_cached += c as u64;
            }
        }
    }

    FinalMetrics {
        total_prompt_tokens: Some(total_prompt),
        total_completion_tokens: Some(total_completion),
        total_cached_tokens: if total_cached > 0 {
            Some(total_cached)
        } else {
            None
        },
        total_cost_usd: None,
        total_steps: Some(total_steps),
        extra: None,
    }
}

// ─── Text extraction helpers ─────────────────────────────────────────────────

/// Extract concatenated text from messages with a specific role.
fn extract_text_by_role(messages: &[InputMessage], role: &str) -> String {
    let mut parts = Vec::new();
    for msg in messages {
        if msg.role == role {
            for part in &msg.parts {
                if let MessagePart::Text { content } = part {
                    if !content.is_empty() {
                        parts.push(content.as_str());
                    }
                }
            }
        }
    }
    parts.join("\n")
}

/// Extract text from InputMessage array filtered by role.
fn extract_text_from_input_messages(messages: &[InputMessage], role: &str) -> String {
    extract_text_by_role(messages, role)
}

/// Extract the last user message text from InputMessage array.
fn extract_last_user_text(messages: &[InputMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .find_map(|m| {
            let text: Vec<&str> = m
                .parts
                .iter()
                .filter_map(|p| match p {
                    MessagePart::Text { content } if !content.is_empty() => Some(content.as_str()),
                    _ => None,
                })
                .collect();
            if text.is_empty() {
                None
            } else {
                Some(text.join("\n"))
            }
        })
}

/// Same as extract_last_user_text but for InputMessage slice.
fn extract_last_user_text_from_input(messages: &[InputMessage]) -> Option<String> {
    extract_last_user_text(messages)
}

/// Convert nanosecond timestamp to ISO 8601 string.
fn ns_to_iso8601(ns: u64) -> String {
    use chrono::DateTime;

    let dt = DateTime::from_timestamp_nanos(ns as i64);
    dt.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::genai::semantic::{InputMessage, MessagePart};

    /// Minimal LLM call carrying one request's tool definitions, for the
    /// `collect_tool_definitions` dedup contract.
    fn call_with_tools(tools: serde_json::Value) -> LLMCall {
        LLMCall {
            call_id: "call-tools".into(),
            start_timestamp_ns: 0,
            end_timestamp_ns: 1,
            duration_ns: 1,
            provider: "openai".into(),
            model: "gpt-5".into(),
            request: crate::genai::semantic::LLMRequest {
                messages: Vec::new(),
                temperature: None,
                max_tokens: None,
                frequency_penalty: None,
                presence_penalty: None,
                top_p: None,
                top_k: None,
                seed: None,
                stop_sequences: None,
                stream: false,
                tools: Some(tools.as_array().expect("tools fixture is an array").clone()),
                raw_body: None,
            },
            response: crate::genai::semantic::LLMResponse {
                messages: Vec::new(),
                streamed: false,
                raw_body: None,
            },
            token_usage: None,
            error: None,
            pid: 1,
            process_name: "agent".into(),
            agent_name: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn tool_definitions_keep_distinct_nameless_tools() {
        // Dedup is by tool name, but a definition need not carry one: the
        // Responses API's built-in tools are `{"type": "web_search"}` /
        // `{"type": "file_search"}` with no `name` at all
        // (platform.openai.com/docs/guides/tools), so one request can hold
        // several nameless definitions. An unreadable name collapsed to the
        // empty string, which merged them: the ATIF agent listed the first
        // built-in tool and dropped the others, while a repeated named
        // definition still collapsed as intended.
        let parsed = vec![Some(call_with_tools(serde_json::json!([
            {"type": "web_search"},
            {"type": "file_search"},
            {"type": "function", "function": {"name": "read_file"}},
            {"type": "function", "function": {"name": "read_file"}},
        ])))];
        let defs = collect_tool_definitions(&parsed).expect("definitions are present");
        let names: Vec<String> = defs.iter().map(|d| d.to_string()).collect();
        assert_eq!(
            defs.len(),
            3,
            "the two nameless built-in tools and one named function survive: {names:?}"
        );
        assert!(
            defs.contains(&serde_json::json!({"type": "file_search"})),
            "the second built-in tool must not be deduplicated away: {names:?}"
        );
        assert_eq!(
            defs.iter()
                .filter(|d| d["function"]["name"] == "read_file")
                .count(),
            1,
            "a repeated named definition is still one entry: {names:?}"
        );
        // The same nameless definition repeated twice is still one entry.
        let parsed = vec![Some(call_with_tools(serde_json::json!([
            {"type": "web_search"},
            {"type": "web_search"},
        ])))];
        assert_eq!(collect_tool_definitions(&parsed).map(|d| d.len()), Some(1));
    }

    /// Minimal LLM call record: an agent turn that emits one tool call, plus the
    /// tool response that the *next* call replays back to the model.
    pub(crate) fn call_event(
        id: i64,
        start_ns: i64,
        output_messages: Option<Vec<OutputMessage>>,
        input_messages: Option<Vec<InputMessage>>,
        user_query: Option<&str>,
    ) -> TraceEventDetail {
        TraceEventDetail {
            id,
            call_id: Some(format!("call-{id}")),
            start_timestamp_ns: start_ns,
            end_timestamp_ns: Some(start_ns + 1_000_000_000),
            model: Some("claude-opus-5".into()),
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: 120,
            input_messages: input_messages.map(|m| serde_json::to_string(&m).unwrap()),
            output_messages: output_messages.map(|m| serde_json::to_string(&m).unwrap()),
            system_instructions: Some(
                serde_json::to_string(&vec![InputMessage {
                    role: "system".into(),
                    parts: vec![MessagePart::Text {
                        content: "You are helpful.".into(),
                    }],
                    name: None,
                }])
                .unwrap(),
            ),
            agent_name: Some("Claude".into()),
            process_name: None,
            pid: Some(42),
            user_query: user_query.map(str::to_string),
            event_json: None,
            trace_id: Some("session-1".into()),
            conversation_id: Some("conv-1".into()),
            cache_read_tokens: Some(80),
            status: Some("complete".into()),
            interruption_type: None,
        }
    }

    #[test]
    fn json_array_facts_ground_through_typed_replay() {
        use crate::grounding::evidence::{Grounding, build_index};
        for response in [
            serde_json::json!({"counts": [119,120]}),
            serde_json::json!("{\"counts\":[119,120]}"),
            serde_json::json!("{\"counts\":[119, 120]}"),
        ] {
            for (statement, unresolved) in [
                ("The counts are 119 and 120.", 0),
                ("The count is 119120.", 1),
            ] {
                let events = vec![
                    call_event(
                        1,
                        1_000_000_000,
                        Some(vec![OutputMessage {
                            role: "assistant".into(),
                            name: None,
                            finish_reason: Some("tool_call".into()),
                            parts: vec![MessagePart::ToolCall {
                                id: Some("facts".into()),
                                name: "Read".into(),
                                arguments: Some(serde_json::json!({"file_path":"/tmp/facts.json"})),
                            }],
                        }]),
                        None,
                        None,
                    ),
                    call_event(
                        2,
                        3_000_000_000,
                        Some(vec![OutputMessage {
                            role: "assistant".into(),
                            name: None,
                            finish_reason: Some("stop".into()),
                            parts: vec![MessagePart::Text {
                                content: statement.into(),
                            }],
                        }]),
                        Some(vec![InputMessage {
                            role: "tool".into(),
                            name: None,
                            parts: vec![MessagePart::ToolCallResponse {
                                id: Some("facts".into()),
                                response: response.clone(),
                            }],
                        }]),
                        None,
                    ),
                ];
                let doc = convert_trace_to_atif("array-facts", events).unwrap();
                let index = build_index(&doc, 0..doc.steps.len());
                assert_eq!(
                    index.unresolved_count(),
                    unresolved,
                    "{response}: {statement}"
                );
                assert_eq!(index.has_deterministic_finding(), unresolved != 0);
                if unresolved == 0 {
                    assert!(index.claims.iter().all(|c| matches!(&c.grounding,
                        Grounding::Grounded { source_call_id: Some(id), .. } if id == "facts")));
                }
            }
        }
    }

    /// Two-call chain: system + user + two agent steps, the first correlating a
    /// tool call with the response replayed in the second call's input.
    pub(crate) fn two_call_chain() -> Vec<TraceEventDetail> {
        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![
                MessagePart::Text {
                    content: "Checking the file.".into(),
                },
                MessagePart::ToolCall {
                    id: Some("tc-1".into()),
                    name: "Read".into(),
                    arguments: Some(serde_json::json!({"file_path": "/tmp/a"})),
                },
            ],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];
        let replayed_response = vec![InputMessage {
            role: "tool".into(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("tc-1".into()),
                response: serde_json::json!("file contents"),
            }],
            name: None,
        }];
        vec![
            call_event(
                1,
                1_000_000_000,
                Some(agent_turn),
                None,
                Some("read /tmp/a"),
            ),
            call_event(2, 3_000_000_000, None, Some(replayed_response), None),
        ]
    }

    /// A trace carries no agent version, so the export must not assert one.
    ///
    /// The shared schema's own fallback for an absent version is `unknown`
    /// (`Agent::default_version`), and the collector's exporter uses the same
    /// word when its events carry none. The ATIF viewer renders this field as
    /// the agent's version, so the literal "1.0.0" told every reader that the
    /// observed agent was version 1.0.0 — a fact nothing measured.
    #[test]
    fn exported_agent_version_is_unknown_when_the_trace_carries_none() {
        let doc = convert_session_to_atif("session-1", two_call_chain()).unwrap();

        assert_eq!(doc.agent.version, "unknown");
    }

    #[test]
    fn session_export_uses_shared_schema_and_valid_step_ids() {
        let doc = convert_session_to_atif("session-1", two_call_chain()).unwrap();

        assert_eq!(doc.schema_version, ATIF_SCHEMA_VERSION);
        assert_eq!(doc.session_id.as_deref(), Some("session-1"));
        doc.validate_step_ids()
            .expect("step ids must be contiguous");

        // system + user + one step per LLM call
        let sources: Vec<StepSource> = doc.steps.iter().map(|s| s.source).collect();
        assert_eq!(
            sources,
            vec![
                StepSource::System,
                StepSource::User,
                StepSource::Agent,
                StepSource::Agent
            ]
        );
        assert_eq!(doc.steps[1].message, "read /tmp/a");

        let fm = doc.final_metrics.as_ref().unwrap();
        assert_eq!(fm.total_prompt_tokens, Some(200));
        assert_eq!(fm.total_completion_tokens, Some(40));
        assert_eq!(fm.total_cached_tokens, Some(160));
        assert_eq!(fm.total_steps, Some(4));
    }

    #[test]
    fn final_metrics_ignore_a_non_positive_token_counter() {
        // A stored counter can be non-positive: the events writer stores the
        // provider's u64 with `as i64`, so a count above i64::MAX lands
        // negative, and the summary report clamps these same counters to 0 for
        // that reason. The per-step metrics of this converter already omit a
        // non-positive value; the document totals added it as a wrapped u64, so
        // one such row reported about 1.8e19 prompt tokens while its own step
        // reported none at all.
        let corrupt = || {
            let mut event = call_event(1, 0, None, None, None);
            event.input_tokens = -5;
            event.output_tokens = -7;
            event.cache_read_tokens = Some(-9);
            event
        };
        let good = call_event(2, 1_000_000_000, None, None, None);

        let metrics = compute_final_metrics(&[corrupt(), good], 2);
        assert_eq!(metrics.total_prompt_tokens, Some(100));
        assert_eq!(metrics.total_completion_tokens, Some(20));
        assert_eq!(metrics.total_cached_tokens, Some(80));

        // With nothing positive to sum, the document reports no tokens rather
        // than a wrapped total — the answer the step metrics give for the same
        // row.
        let metrics = compute_final_metrics(&[corrupt()], 1);
        assert_eq!(metrics.total_prompt_tokens, Some(0));
        assert_eq!(metrics.total_completion_tokens, Some(0));
        assert_eq!(metrics.total_cached_tokens, None);
    }

    #[test]
    fn agent_step_carries_tool_call_observation_and_start_timestamp() {
        let doc = convert_trace_to_atif("trace-1", two_call_chain()).unwrap();
        let step = &doc.steps[2];

        assert_eq!(step.message, "Checking the file.");
        assert_eq!(step.model_name.as_deref(), Some("claude-opus-5"));

        let calls = step.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].tool_call_id, "tc-1");
        assert_eq!(calls[0].function_name, "Read");

        // Observation content stays a JSON string so text consumers can render it.
        let results = &step.observation.as_ref().unwrap().results;
        assert_eq!(results[0].source_call_id.as_deref(), Some("tc-1"));
        assert_eq!(
            results[0].content.as_ref().and_then(|c| c.as_str()),
            Some("file contents")
        );

        // extra.start_timestamp is what lets consumers split inference vs tool time.
        let extra = step.extra.as_ref().unwrap();
        assert_eq!(
            extra.get("start_timestamp").and_then(|v| v.as_str()),
            Some("1970-01-01T00:00:01Z")
        );
        assert_eq!(step.metrics.as_ref().unwrap().prompt_tokens, Some(100));
    }

    #[test]
    fn successful_log_read_remains_numeric_grounding_evidence() {
        use crate::grounding::{evidence::build_index, outcome::CallStatus};

        // The Read succeeds; the traceback describes an archived application
        // failure. Its number must still support the assistant's next claim.
        for (flag, expected) in [
            (Some(false), CallStatus::Ok),
            (Some(true), CallStatus::Failed),
            (None, CallStatus::Failed),
        ] {
            let content = "Traceback (most recent call last):\nRuntimeError: archived failure\nArchived entry count: 242391";
            let response = match flag {
                Some(is_error) => serde_json::json!({"content": content, "is_error": is_error}),
                None => serde_json::json!(content),
            };
            let output = vec![OutputMessage {
                role: "assistant".into(),
                parts: vec![MessagePart::ToolCall {
                    id: Some("read-log".into()),
                    name: "Read".into(),
                    arguments: Some(serde_json::json!({"file_path": "/synthetic/archive.log"})),
                }],
                name: None,
                finish_reason: Some("tool_call".into()),
            }];
            let input = vec![InputMessage {
                role: "user".into(),
                parts: vec![MessagePart::ToolCallResponse {
                    id: Some("read-log".into()),
                    response,
                }],
                name: None,
            }];
            let claim = vec![OutputMessage {
                role: "assistant".into(),
                parts: vec![MessagePart::Text {
                    content: "The archived entry count is 242391.".into(),
                }],
                name: None,
                finish_reason: Some("stop".into()),
            }];
            let doc = convert_trace_to_atif(
                "synthetic-read-log",
                vec![
                    call_event(1, 1_000_000_000, Some(output), None, Some("read the log")),
                    call_event(2, 3_000_000_000, Some(claim), Some(input), None),
                ],
            )
            .unwrap();
            assert_eq!(
                doc.steps[2].observation.as_ref().unwrap().results[0].is_error(),
                flag
            );
            let index = build_index(&doc, 0..doc.steps.len());
            assert_eq!(index.call_verdicts[0].verdict.status, expected);
            assert!(!index.claims.is_empty());
            assert_eq!(
                index.unresolved_count(),
                usize::from(expected == CallStatus::Failed)
            );
            assert_eq!(
                index.has_deterministic_finding(),
                expected == CallStatus::Failed
            );
        }
    }

    /// Anthropic writes `tool_result.content` as an array of content blocks
    /// whenever a result has more than one piece of text: MCP servers, a
    /// command whose stdout and stderr were captured separately, and Claude
    /// Code's own multi-block results all arrive as
    /// `[{"type":"text","text":…},{"type":"text","text":…}]`.
    ///
    /// `GenAIBuilder::anthropic_content_block_to_part` passes that array
    /// through unchanged, so the converter is the only place left to read it.
    /// Serialising the array whole turned the observation into
    /// `[{"type":"text","text":"command failed"},…]`, which loses the text the
    /// downstream consumers read: `outcome::classify_call` looks for a line
    /// that *starts* with an error token and found `[{"type":"text"`, and the
    /// evidence pool haystack carried JSON punctuation instead of the output,
    /// so a value the tool actually printed could not ground anything.
    ///
    /// The local collector's converter already flattens this shape
    /// (`flatten_tool_result_content`, "flatten array tool results in the
    /// local ATIF converter"), and the ATIF schema says the content is the
    /// result text. Read the same blocks here.
    #[test]
    fn a_multi_block_tool_result_reaches_the_classifier_as_its_text() {
        use crate::grounding::{evidence::build_index, outcome::CallStatus};

        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![MessagePart::ToolCall {
                id: Some("tc-blocks".into()),
                name: "Bash".into(),
                arguments: Some(serde_json::json!({"command": "false"})),
            }],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];
        // The shape Anthropic returns for a failed multi-block result.
        let replayed = vec![InputMessage {
            role: "user".into(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("tc-blocks".into()),
                response: serde_json::json!({
                    "content": [
                        {"type": "text", "text": "command failed"},
                        {"type": "text", "text": "exit 1"}
                    ],
                    "is_error": true
                }),
            }],
            name: None,
        }];

        let doc = convert_trace_to_atif(
            "trace-blocks",
            vec![
                call_event(1, 1_000_000_000, Some(agent_turn), None, Some("run it")),
                call_event(2, 3_000_000_000, None, Some(replayed), None),
            ],
        )
        .unwrap();
        let result = &doc.steps[2].observation.as_ref().unwrap().results[0];
        let text = result
            .content
            .as_ref()
            .and_then(|c| c.as_str())
            .expect("content is flattened to a string");
        assert_eq!(text, "command failed\nexit 1", "got {text:?}");
        assert_eq!(result.is_error(), Some(true));

        let index = build_index(&doc, 0..doc.steps.len());
        assert_eq!(index.call_verdicts[0].verdict.status, CallStatus::Failed);
    }

    /// The Responses API copies `function_call_output.output` through
    /// unchanged, so a tool result can arrive as a JSON-encoded *string*
    /// holding the envelope — `{\"content\":…,\"isError\":false}` — rather than
    /// as an object. An MCP result is often serialised that way too.
    ///
    /// The interruption detector and the grader both re-parse such a payload
    /// before judging it (`tool_response_failure_text` and its grader twin),
    /// which is what keeps an explicit `isError: false` authoritative over
    /// free-form output that happens to mention a missing path. The converter
    /// read the outer string only, so `declared` was `None`, the success flag
    /// never reached the observation, and the same payload the detector calls
    /// a success reached `classify_call` with no outcome at all: the result
    /// stayed in the evidence pool, where `grep: no such file or directory`
    /// could ground — or refuse to ground — a later claim it never sourced.
    ///
    /// Both spellings must agree on the same bytes; that agreement is what
    /// this pins.
    #[test]
    fn an_encoded_tool_result_envelope_keeps_its_declared_outcome() {
        use crate::grounding::{evidence::build_index, outcome::CallStatus};

        for (encoded, flag, expected) in [
            (
                r#"{"content":"grep: no such file or directory","isError":false}"#,
                Some(false),
                CallStatus::Ok,
            ),
            (
                r#"{"content":"command failed","isError":true}"#,
                Some(true),
                CallStatus::Failed,
            ),
        ] {
            let agent_turn = vec![OutputMessage {
                role: "assistant".into(),
                parts: vec![MessagePart::ToolCall {
                    id: Some("tc-enc".into()),
                    name: "Bash".into(),
                    arguments: Some(serde_json::json!({"command": "grep -r x /root"})),
                }],
                name: None,
                finish_reason: Some("tool_call".into()),
            }];
            let replayed = vec![InputMessage {
                role: "tool".into(),
                parts: vec![MessagePart::ToolCallResponse {
                    id: Some("tc-enc".into()),
                    response: serde_json::json!(encoded),
                }],
                name: None,
            }];

            let doc = convert_trace_to_atif(
                "trace-encoded",
                vec![
                    call_event(1, 1_000_000_000, Some(agent_turn), None, Some("look")),
                    call_event(2, 3_000_000_000, None, Some(replayed), None),
                ],
            )
            .unwrap();
            let result = &doc.steps[2].observation.as_ref().unwrap().results[0];
            assert_eq!(result.is_error(), flag, "payload {encoded}");

            let index = build_index(&doc, 0..doc.steps.len());
            assert_eq!(
                index.call_verdicts[0].verdict.status, expected,
                "payload {encoded}"
            );
        }
    }

    /// A tool result can spell its failure flag `isError` (the camelCase real
    /// agent traces carry) or report an `error` status instead. The ATIF
    /// converter read only `is_error`, so such a result reached the grounding
    /// classifier with no flag at all: a `{"isError": false}` result whose
    /// free-form body mentions a missing path or a traceback was judged a
    /// certain failure, and — the worse direction — an `{"isError": true}`
    /// result with a clean body was read as a success and stayed in the
    /// evidence pool as grounding evidence, while the interruption detector
    /// and the grader both honour the flag.
    #[test]
    fn a_tool_result_flag_spelled_iserror_reaches_the_classifier() {
        use crate::grounding::{evidence::build_index, outcome::CallStatus};

        for (response, flag, expected) in [
            (
                serde_json::json!({"content": "grep: /root/.ssh: no such file or directory", "isError": false}),
                Some(false),
                CallStatus::Ok,
            ),
            (
                serde_json::json!({"content": "done", "isError": true}),
                Some(true),
                CallStatus::Failed,
            ),
            (
                serde_json::json!({"content": "done", "status": "error"}),
                Some(true),
                CallStatus::Failed,
            ),
        ] {
            let output = vec![OutputMessage {
                role: "assistant".into(),
                parts: vec![MessagePart::ToolCall {
                    id: Some("tc-flag".into()),
                    name: "Grep".into(),
                    arguments: Some(serde_json::json!({"pattern": "token"})),
                }],
                name: None,
                finish_reason: Some("tool_call".into()),
            }];
            let input = vec![InputMessage {
                role: "user".into(),
                parts: vec![MessagePart::ToolCallResponse {
                    id: Some("tc-flag".into()),
                    response: response.clone(),
                }],
                name: None,
            }];
            let doc = convert_trace_to_atif(
                "synthetic-flag-spelling",
                vec![
                    call_event(1, 1_000_000_000, Some(output), None, Some("search")),
                    call_event(2, 3_000_000_000, None, Some(input), None),
                ],
            )
            .unwrap();
            assert_eq!(
                doc.steps[2].observation.as_ref().unwrap().results[0].is_error(),
                flag,
                "{response}"
            );
            let index = build_index(&doc, 0..doc.steps.len());
            assert_eq!(
                index.call_verdicts[0].verdict.status, expected,
                "{response}"
            );
        }
    }

    /// An `error` status contradicts the explicit flag of the same payload.
    ///
    /// A provider that answers `{"content": "done", "is_error": false,
    /// "status": "error"}` says two opposite things; the interruption
    /// detector and the grader both let the error status win, and the
    /// converter did not: the boolean settled `is_error` first, so the status
    /// arm never ran and a certain failure reached the grounding classifier
    /// with an explicit success flag, which labels it `Ok` at high confidence
    /// and keeps its body in the evidence pool.
    #[test]
    fn an_error_status_outranks_an_explicit_success_flag() {
        use crate::grounding::{evidence::build_index, outcome::CallStatus};

        for (response, flag) in [
            (
                serde_json::json!({"content": "done", "is_error": false, "status": "error"}),
                true,
            ),
            (
                serde_json::json!({"content": "done", "isError": false, "status": "ERROR"}),
                true,
            ),
            (
                serde_json::json!({"content": "done", "is_error": false, "status": "complete"}),
                false,
            ),
        ] {
            let output = vec![OutputMessage {
                role: "assistant".into(),
                parts: vec![MessagePart::ToolCall {
                    id: Some("tc-status".into()),
                    name: "Bash".into(),
                    arguments: Some(serde_json::json!({"command": "true"})),
                }],
                name: None,
                finish_reason: Some("tool_call".into()),
            }];
            let input = vec![InputMessage {
                role: "user".into(),
                parts: vec![MessagePart::ToolCallResponse {
                    id: Some("tc-status".into()),
                    response: response.clone(),
                }],
                name: None,
            }];
            let doc = convert_trace_to_atif(
                "synthetic-status-override",
                vec![
                    call_event(1, 1_000_000_000, Some(output), None, Some("run it")),
                    call_event(2, 3_000_000_000, None, Some(input), None),
                ],
            )
            .unwrap();
            assert_eq!(
                doc.steps[2].observation.as_ref().unwrap().results[0].is_error(),
                Some(flag),
                "{response}"
            );
            let index = build_index(&doc, 0..doc.steps.len());
            let expected = if flag {
                CallStatus::Failed
            } else {
                CallStatus::Ok
            };
            assert_eq!(
                index.call_verdicts[0].verdict.status, expected,
                "{response}"
            );
        }
    }

    /// A payload may spell its outcome `success` instead of a flag.
    ///
    /// Some tools answer `{"content": "done", "success": false}` (or with the
    /// error status beside a `success: true`). The interruption detector and
    /// the grader both read that spelling; the converter did not, so the
    /// failure reached the classifier with no flag at all and a clean body was
    /// read as a success and stayed in the evidence pool.
    #[test]
    fn a_success_flag_reaches_the_classifier() {
        use crate::grounding::{evidence::build_index, outcome::CallStatus};

        for (response, flag) in [
            (
                serde_json::json!({"content": "done", "success": false}),
                true,
            ),
            (
                serde_json::json!({"content": "done", "success": false, "status": "ok"}),
                true,
            ),
            (
                serde_json::json!({"content": "done", "success": true}),
                false,
            ),
            (
                serde_json::json!({"content": "done", "success": true, "status": "error"}),
                true,
            ),
        ] {
            let output = vec![OutputMessage {
                role: "assistant".into(),
                parts: vec![MessagePart::ToolCall {
                    id: Some("tc-success".into()),
                    name: "Bash".into(),
                    arguments: Some(serde_json::json!({"command": "true"})),
                }],
                name: None,
                finish_reason: Some("tool_call".into()),
            }];
            let input = vec![InputMessage {
                role: "user".into(),
                parts: vec![MessagePart::ToolCallResponse {
                    id: Some("tc-success".into()),
                    response: response.clone(),
                }],
                name: None,
            }];
            let doc = convert_trace_to_atif(
                "synthetic-success-spelling",
                vec![
                    call_event(1, 1_000_000_000, Some(output), None, Some("run it")),
                    call_event(2, 3_000_000_000, None, Some(input), None),
                ],
            )
            .unwrap();
            assert_eq!(
                doc.steps[2].observation.as_ref().unwrap().results[0].is_error(),
                Some(flag),
                "{response}"
            );
            let index = build_index(&doc, 0..doc.steps.len());
            let expected = if flag {
                CallStatus::Failed
            } else {
                CallStatus::Ok
            };
            assert_eq!(
                index.call_verdicts[0].verdict.status, expected,
                "{response}"
            );
        }
    }

    #[test]
    fn tool_failure_flag_survives_as_structured_signal() {
        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![MessagePart::ToolCall {
                id: Some("tc-err".into()),
                name: "Read".into(),
                arguments: Some(serde_json::json!({"file_path": "/tmp/missing"})),
            }],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];
        let failed_response = vec![InputMessage {
            role: "tool".into(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("tc-err".into()),
                response: serde_json::json!({"content": "file not found", "is_error": true}),
            }],
            name: None,
        }];
        let events = vec![
            call_event(1, 1_000_000_000, Some(agent_turn), None, Some("read it")),
            call_event(2, 3_000_000_000, None, Some(failed_response), None),
        ];

        let doc = convert_trace_to_atif("trace-err", events).unwrap();
        let result = &doc.steps[2].observation.as_ref().unwrap().results[0];

        // Readable without string-matching the flattened text, which is what
        // makes "the tool failed" a deterministic judgment downstream.
        assert_eq!(result.is_error(), Some(true));

        // The envelope is unwrapped, not serialised whole. Classification looks
        // for lines that *start* with an error token, so a leftover
        // `{"content":"…` prefix would defeat it on precisely the results the
        // flag marks as failures.
        let text = result
            .content
            .as_ref()
            .and_then(|c| c.as_str())
            .expect("content is flattened to a string");
        assert_eq!(text, "file not found", "got {text:?}");

        // A result nobody flagged is unknown, not passing.
        let ok_doc = convert_trace_to_atif("trace-ok", two_call_chain()).unwrap();
        let ok_result = &ok_doc.steps[2].observation.as_ref().unwrap().results[0];
        assert_eq!(ok_result.is_error(), None);
    }

    #[test]
    fn tool_response_on_a_user_turn_still_populates_observation() {
        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![MessagePart::ToolCall {
                id: Some("tc-u".into()),
                name: "Bash".into(),
                arguments: Some(serde_json::json!({"command": "ls /nope"})),
            }],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];
        // Shape taken from a live capture: the result rides on the *user* turn,
        // not on a `tool`-role message.
        let replayed = vec![InputMessage {
            role: "user".into(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("tc-u".into()),
                response: serde_json::json!({
                    "content": "ls: cannot access '/nope': No such file or directory",
                    "is_error": true
                }),
            }],
            name: None,
        }];
        let events = vec![
            call_event(1, 1_000_000_000, Some(agent_turn), None, Some("list it")),
            call_event(2, 3_000_000_000, None, Some(replayed), None),
        ];

        let doc = convert_trace_to_atif("trace-user-turn", events).unwrap();
        let results = &doc.steps[2]
            .observation
            .as_ref()
            .expect("a user-carried tool response must still produce an observation")
            .results;
        assert_eq!(results[0].source_call_id.as_deref(), Some("tc-u"));
        assert!(
            results[0]
                .content
                .as_ref()
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains("No such file")),
            "content={:?}",
            results[0].content
        );
        assert_eq!(results[0].is_error(), Some(true));
    }

    #[test]
    fn empty_events_are_rejected() {
        assert!(convert_trace_to_atif("t", vec![]).is_err());
        assert!(convert_session_to_atif("s", vec![]).is_err());
    }

    #[test]
    fn strong_literal_facts_ground_through_typed_replay() {
        use crate::grounding::evidence::{Grounding, build_index};
        for (literal, extended) in [
            ("1.2.3", "1.2.30"),
            ("/tmp/report.txt", "/tmp/report.txt.bak"),
            ("https://example.test/api", "https://example.test/apiv2"),
        ] {
            for (observed, unresolved) in [(literal, 0), (extended, 1)] {
                for response in [
                    serde_json::json!(format!("Observed: {observed}")),
                    serde_json::json!({"value": observed}),
                ] {
                    let events = vec![
                        call_event(
                            1,
                            1_000_000_000,
                            Some(vec![OutputMessage {
                                role: "assistant".into(),
                                name: None,
                                finish_reason: Some("tool_call".into()),
                                parts: vec![MessagePart::ToolCall {
                                    id: Some("literal-source".into()),
                                    name: "Read".into(),
                                    arguments: Some(
                                        serde_json::json!({"file_path":"/tmp/facts.json"}),
                                    ),
                                }],
                            }]),
                            None,
                            None,
                        ),
                        call_event(
                            2,
                            3_000_000_000,
                            None,
                            Some(vec![InputMessage {
                                role: "tool".into(),
                                name: None,
                                parts: vec![MessagePart::ToolCallResponse {
                                    id: Some("literal-source".into()),
                                    response: response.clone(),
                                }],
                            }]),
                            None,
                        ),
                        call_event(
                            3,
                            5_000_000_000,
                            Some(vec![OutputMessage {
                                role: "assistant".into(),
                                name: None,
                                finish_reason: Some("stop".into()),
                                parts: vec![MessagePart::Text {
                                    content: format!("The value is {literal}."),
                                }],
                            }]),
                            None,
                            None,
                        ),
                    ];
                    let doc = convert_trace_to_atif("literal-facts", events).unwrap();
                    let last = doc.steps.len() - 1;
                    let index = build_index(&doc, last..doc.steps.len());
                    assert_eq!(
                        index.unresolved_count(),
                        unresolved,
                        "{response}: {literal}"
                    );
                    assert_eq!(index.has_deterministic_finding(), unresolved != 0);
                    if unresolved == 0 {
                        assert!(index.claims.iter().all(|c| matches!(&c.grounding,
                            Grounding::Grounded { source_call_id: Some(id), .. } if id == "literal-source")));
                    }
                }
            }
        }
    }

    #[test]
    fn test_ns_to_iso8601() {
        // 0 ns = Unix epoch
        let s = ns_to_iso8601(0);
        assert_eq!(s, "1970-01-01T00:00:00Z");
        // 1 second in nanoseconds
        let s = ns_to_iso8601(1_000_000_000);
        assert_eq!(s, "1970-01-01T00:00:01Z");
    }

    #[test]
    fn test_extract_text_by_role() {
        let messages = vec![
            InputMessage {
                role: "system".to_string(),
                parts: vec![MessagePart::Text {
                    content: "You are helpful.".to_string(),
                }],
                name: None,
            },
            InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "Hello".to_string(),
                }],
                name: None,
            },
            InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "World".to_string(),
                }],
                name: None,
            },
        ];
        assert_eq!(
            extract_text_by_role(&messages, "system"),
            "You are helpful."
        );
        assert_eq!(extract_text_by_role(&messages, "user"), "Hello\nWorld");
        assert_eq!(extract_text_by_role(&messages, "assistant"), "");
    }

    #[test]
    fn test_extract_last_user_text() {
        let messages = vec![
            InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "First".to_string(),
                }],
                name: None,
            },
            InputMessage {
                role: "assistant".to_string(),
                parts: vec![MessagePart::Text {
                    content: "Response".to_string(),
                }],
                name: None,
            },
            InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "Second".to_string(),
                }],
                name: None,
            },
        ];
        assert_eq!(
            extract_last_user_text(&messages),
            Some("Second".to_string())
        );
    }

    #[test]
    fn test_extract_last_user_text_empty() {
        let messages = vec![InputMessage {
            role: "system".to_string(),
            parts: vec![MessagePart::Text {
                content: "sys".to_string(),
            }],
            name: None,
        }];
        assert_eq!(extract_last_user_text(&messages), None);
    }

    /// A row that never completed keeps the raw protocol messages the crash
    /// drain copied from the request body; its tool result must still become an
    /// observation instead of being dropped by the strict parse.
    #[test]
    fn raw_shaped_columns_still_yield_the_tool_result() {
        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![MessagePart::ToolCall {
                id: Some("tc-raw".into()),
                name: "list_dir".into(),
                arguments: Some(serde_json::json!({})),
            }],
            name: None,
            finish_reason: Some("tool_calls".into()),
        }];
        let mut replayed = call_event(2, 3_000_000_000, None, None, None);
        replayed.input_messages =
            Some(r#"[{"role":"tool","tool_call_id":"tc-raw","content":"raw-a.txt"}]"#.to_string());
        replayed.system_instructions = None;

        let events = vec![
            call_event(1, 1_000_000_000, Some(agent_turn), None, Some("list it")),
            replayed,
        ];

        let doc = convert_trace_to_atif("trace-raw", events).unwrap();
        let results = &doc.steps[2]
            .observation
            .as_ref()
            .expect("raw-shaped columns must still produce an observation")
            .results;
        assert_eq!(results[0].source_call_id.as_deref(), Some("tc-raw"));
        assert_eq!(
            results[0].content.as_ref().and_then(|c| c.as_str()),
            Some("raw-a.txt")
        );
    }

    #[test]
    fn test_extract_text_from_input_messages() {
        let messages = vec![InputMessage {
            role: "user".to_string(),
            parts: vec![MessagePart::Text {
                content: "Query".to_string(),
            }],
            name: None,
        }];
        assert_eq!(extract_text_from_input_messages(&messages, "user"), "Query");
    }

    #[test]
    fn test_extract_last_user_text_from_input() {
        let messages = vec![
            InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "Q1".to_string(),
                }],
                name: None,
            },
            InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "Q2".to_string(),
                }],
                name: None,
            },
        ];
        assert_eq!(
            extract_last_user_text_from_input(&messages),
            Some("Q2".to_string())
        );
    }
    #[test]
    fn generated_call_ids_are_unique_across_steps_and_reused_by_results() {
        let tool_turn = |name: &str| {
            vec![OutputMessage {
                role: "assistant".into(),
                parts: vec![MessagePart::ToolCall {
                    id: None,
                    name: name.into(),
                    arguments: Some(serde_json::json!({"value": name})),
                }],
                name: None,
                finish_reason: Some("tool_call".into()),
            }]
        };
        let response = |text: &str| {
            vec![InputMessage {
                role: "tool".into(),
                parts: vec![MessagePart::ToolCallResponse {
                    id: None,
                    response: serde_json::json!(text),
                }],
                name: None,
            }]
        };
        let events = vec![
            call_event(
                1,
                1_000_000_000,
                Some(tool_turn("first")),
                None,
                Some("run"),
            ),
            call_event(
                2,
                3_000_000_000,
                Some(tool_turn("second")),
                Some(response("first result")),
                None,
            ),
            call_event(
                3,
                5_000_000_000,
                None,
                Some(response("second result")),
                None,
            ),
        ];

        let doc = convert_trace_to_atif("trace-auto", events).unwrap();
        let acting: Vec<&Step> = doc
            .steps
            .iter()
            .filter(|step| {
                step.tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
            })
            .collect();
        assert_eq!(acting.len(), 2);

        let first_id = &acting[0].tool_calls.as_ref().unwrap()[0].tool_call_id;
        let second_id = &acting[1].tool_calls.as_ref().unwrap()[0].tool_call_id;
        assert_ne!(first_id, second_id);
        assert!(first_id.starts_with("auto_"));
        assert!(second_id.starts_with("auto_"));

        for step in acting {
            let call_id = &step.tool_calls.as_ref().unwrap()[0].tool_call_id;
            let result_id = step.observation.as_ref().unwrap().results[0]
                .source_call_id
                .as_deref();
            assert_eq!(result_id, Some(call_id.as_str()));
        }
    }

    #[test]
    fn id_bearing_response_outranks_the_positional_fallback() {
        // Two tool calls in one step, replayed with the results arriving out
        // of call order — parallel tools complete in completion order, not
        // request order — and only the later response carrying an id. The id
        // is the authoritative link, so it must attach its response to the
        // call it names; the id-less response takes the other slot, not the
        // other way around.
        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![
                MessagePart::ToolCall {
                    id: Some("tc-a".into()),
                    name: "Read".into(),
                    arguments: Some(serde_json::json!({"file_path": "/tmp/a"})),
                },
                MessagePart::ToolCall {
                    id: Some("tc-b".into()),
                    name: "Read".into(),
                    arguments: Some(serde_json::json!({"file_path": "/tmp/b"})),
                },
            ],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];
        let replayed = vec![InputMessage {
            role: "tool".into(),
            parts: vec![
                MessagePart::ToolCallResponse {
                    id: None,
                    response: serde_json::json!("output of b"),
                },
                MessagePart::ToolCallResponse {
                    id: Some("tc-a".into()),
                    response: serde_json::json!("output of a"),
                },
            ],
            name: None,
        }];
        let events = vec![
            call_event(1, 1_000_000_000, Some(agent_turn), None, Some("read both")),
            call_event(2, 3_000_000_000, None, Some(replayed), None),
        ];

        let doc = convert_trace_to_atif("trace-swap", events).unwrap();
        let step = &doc.steps[2];
        assert_eq!(step.tool_calls.as_ref().unwrap().len(), 2);

        let results = &step.observation.as_ref().unwrap().results;
        assert_eq!(results.len(), 2);
        let by_id = |id: &str| {
            results
                .iter()
                .find(|r| r.source_call_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("no result carries id {id}: {results:?}"))
        };
        assert_eq!(
            by_id("tc-a").content.as_ref().and_then(|c| c.as_str()),
            Some("output of a"),
        );
        assert_eq!(
            by_id("tc-b").content.as_ref().and_then(|c| c.as_str()),
            Some("output of b"),
        );
    }

    /// A next event replays the conversation, not only the latest round: a row
    /// that never completed keeps the whole history — `build_pending_from_request`
    /// copies every non-system message until the call completes — so an
    /// *earlier* round's id-less tool result is in `input_messages` too. Pass 2
    /// matches an id-less response to the next unmatched call in order, so
    /// without narrowing the scan to the latest round that earlier result
    /// answers this step's call and the current one is dropped.
    #[test]
    fn an_earlier_round_result_does_not_answer_the_current_step() {
        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![MessagePart::ToolCall {
                id: None,
                name: "list_dir".into(),
                arguments: Some(serde_json::json!({})),
            }],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];
        let mut replayed = call_event(2, 3_000_000_000, None, None, None);
        replayed.input_messages = Some(
            serde_json::json!([
                {"role": "user", "content": "list it"},
                {"role": "assistant", "tool_calls": [
                    {"id": "old-call", "type": "function",
                     "function": {"name": "list_dir", "arguments": "{}"}}]},
                {"role": "tool", "content": "old result"},
                {"role": "assistant", "tool_calls": [
                    {"id": "cur-call", "type": "function",
                     "function": {"name": "list_dir", "arguments": "{}"}}]},
                {"role": "tool", "content": "current result"}
            ])
            .to_string(),
        );

        let events = vec![
            call_event(1, 1_000_000_000, Some(agent_turn), None, Some("list it")),
            replayed,
        ];

        let doc = convert_trace_to_atif("trace-earlier-round", events).unwrap();
        let results = &doc.steps[2]
            .observation
            .as_ref()
            .expect("the latest round's tool result must become an observation")
            .results;
        assert_eq!(
            results.len(),
            1,
            "only the current round's result belongs here: {results:?}"
        );
        assert_eq!(
            results[0].content.as_ref().and_then(|c| c.as_str()),
            Some("current result"),
        );
    }

    #[test]
    fn current_results_survive_foreign_and_duplicate_ids() {
        use crate::grounding::{evidence::build_index, outcome::CallStatus};

        // Exercise typed replay -> ATIF -> grounding, including genuinely
        // missing results and legacy replay with no IDs at all.
        for (replay, expected_b) in [
            (
                vec![(Some("old-call"), "old"), (Some("tc-a"), "A"), (None, "B")],
                Some("B"),
            ),
            (
                vec![
                    (Some("tc-a"), "A"),
                    (Some("tc-a"), "duplicate"),
                    (None, "B"),
                ],
                Some("B"),
            ),
            (vec![(None, "B"), (Some("tc-a"), "A")], Some("B")),
            (vec![(None, "A"), (None, "B")], Some("B")),
            (vec![(Some("old-call"), "old"), (Some("tc-a"), "A")], None),
            (vec![(Some("tc-a"), "A"), (Some("tc-a"), "duplicate")], None),
            (
                vec![
                    (Some("old-call"), "old"),
                    (Some("tc-b"), "B"),
                    (Some("tc-a"), "A"),
                ],
                Some("B"),
            ),
        ] {
            let output = vec![OutputMessage {
                role: "assistant".into(),
                parts: ["tc-a", "tc-b"]
                    .into_iter()
                    .map(|id| MessagePart::ToolCall {
                        id: Some(id.into()),
                        name: "Read".into(),
                        arguments: Some(serde_json::json!({"file_path": "/synthetic/log"})),
                    })
                    .collect(),
                name: None,
                finish_reason: Some("tool_call".into()),
            }];
            let input = vec![InputMessage {
                role: "tool".into(),
                parts: replay
                    .into_iter()
                    .map(|(id, content)| MessagePart::ToolCallResponse {
                        id: id.map(str::to_string),
                        response: serde_json::json!(content),
                    })
                    .collect(),
                name: None,
            }];
            let doc = convert_trace_to_atif(
                "synthetic-replay",
                vec![
                    call_event(1, 1_000_000_000, Some(output), None, Some("read both")),
                    call_event(2, 3_000_000_000, None, Some(input), None),
                ],
            )
            .unwrap();
            let results = &doc.steps[2].observation.as_ref().unwrap().results;
            assert_eq!(results.len(), if expected_b.is_some() { 2 } else { 1 });
            let content = |id| {
                results
                    .iter()
                    .find(|r| r.source_call_id.as_deref() == Some(id))
                    .and_then(|r| r.content.as_ref())
                    .and_then(|v| v.as_str())
            };
            assert_eq!(content("tc-a"), Some("A"));
            assert_eq!(content("tc-b"), expected_b);
            let index = build_index(&doc, 0..doc.steps.len());
            assert_eq!(index.call_verdicts.len(), 2);
            assert_eq!(index.call_verdicts[0].verdict.status, CallStatus::Ok);
            assert_eq!(
                index.call_verdicts[1].verdict.status,
                if expected_b.is_some() {
                    CallStatus::Ok
                } else {
                    CallStatus::Unknown
                }
            );
        }
    }

    #[test]
    fn non_string_error_content_remains_reversible_after_flattening() {
        let payload = serde_json::json!({
            "error": {"code": 404, "path": "/tmp/missing"},
            "hints": ["retry", "check path"]
        });
        let agent_turn = vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![MessagePart::ToolCall {
                id: Some("tc-json".into()),
                name: "Read".into(),
                arguments: Some(serde_json::json!({"file_path": "/tmp/missing"})),
            }],
            name: None,
            finish_reason: Some("tool_call".into()),
        }];
        let failed_response = vec![InputMessage {
            role: "tool".into(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some("tc-json".into()),
                response: serde_json::json!({"content": payload, "is_error": true}),
            }],
            name: None,
        }];
        let events = vec![
            call_event(1, 1_000_000_000, Some(agent_turn), None, Some("read it")),
            call_event(2, 3_000_000_000, None, Some(failed_response), None),
        ];

        let doc = convert_trace_to_atif("trace-json-error", events).unwrap();
        let result = &doc.steps[2].observation.as_ref().unwrap().results[0];
        assert_eq!(result.is_error(), Some(true));

        let flattened = result.content.as_ref().and_then(|v| v.as_str()).unwrap();
        let restored: serde_json::Value = serde_json::from_str(flattened).unwrap();
        assert_eq!(restored, payload);
    }

    /// Event with explicit conversation/trace ids, so tests can control the
    /// exact grouping the session exporter sees.
    fn event_with_ids(
        id: i64,
        start_ns: i64,
        conversation_id: Option<&str>,
        trace_id: Option<&str>,
        user_query: Option<&str>,
        output_messages: Option<Vec<OutputMessage>>,
        input_messages: Option<Vec<InputMessage>>,
    ) -> TraceEventDetail {
        TraceEventDetail {
            id,
            call_id: Some(format!("call-{id}")),
            start_timestamp_ns: start_ns,
            end_timestamp_ns: Some(start_ns + 1_000_000_000),
            model: Some("claude-opus-5".into()),
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: 120,
            input_messages: input_messages.map(|m| serde_json::to_string(&m).unwrap()),
            output_messages: output_messages.map(|m| serde_json::to_string(&m).unwrap()),
            system_instructions: None,
            agent_name: Some("Claude".into()),
            process_name: None,
            pid: Some(42),
            user_query: user_query.map(str::to_string),
            event_json: None,
            trace_id: trace_id.map(str::to_string),
            conversation_id: conversation_id.map(str::to_string),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        }
    }

    fn tool_call_turn(tc_id: &str) -> Vec<OutputMessage> {
        vec![OutputMessage {
            role: "assistant".into(),
            parts: vec![MessagePart::ToolCall {
                id: Some(tc_id.into()),
                name: "Read".into(),
                arguments: Some(serde_json::json!({"file_path": "/tmp/a"})),
            }],
            name: None,
            finish_reason: Some("tool_call".into()),
        }]
    }

    fn replayed_response(tc_id: &str) -> Vec<InputMessage> {
        vec![InputMessage {
            role: "tool".into(),
            parts: vec![MessagePart::ToolCallResponse {
                id: Some(tc_id.into()),
                response: serde_json::json!("file contents"),
            }],
            name: None,
        }]
    }

    #[test]
    fn session_export_does_not_correlate_events_of_unknown_conversation() {
        // conversation_id resolution returns None when the request carries no
        // trailing user text or the response id is missing, so turn
        // membership is UNKNOWN for both events. Merging them under one
        // shared key correlates the tool call of one turn with a replayed
        // response that may belong to an unrelated turn, fabricating an
        // observation out of thin air.
        let events = vec![
            event_with_ids(
                1,
                1_000_000_000,
                None,
                Some("resp-1"),
                None,
                Some(tool_call_turn("tc-x")),
                None,
            ),
            event_with_ids(
                2,
                3_000_000_000,
                None,
                Some("resp-2"),
                None,
                None,
                Some(replayed_response("tc-x")),
            ),
        ];

        let doc = convert_session_to_atif("session-x", events).unwrap();

        // Both calls are still exported, one agent step each — dropping data
        // would desync step counts from the token totals in final_metrics.
        let agent_steps: Vec<&Step> = doc
            .steps
            .iter()
            .filter(|s| s.source == StepSource::Agent)
            .collect();
        assert_eq!(agent_steps.len(), 2);

        for step in agent_steps {
            assert!(
                step.observation.is_none(),
                "events of unknown turn must not be cross-correlated: {:?}",
                step.observation
            );
        }
    }

    #[test]
    fn session_export_keeps_one_conversation_together_across_distinct_trace_ids() {
        // trace_id is the per-call API response id: consecutive calls of one
        // agent turn carry DIFFERENT trace ids. Turn grouping must therefore
        // key on conversation_id — keying on trace_id would isolate every
        // call in its own group and sever the tool-call → replayed-response
        // correlation entirely.
        let events = vec![
            event_with_ids(
                1,
                1_000_000_000,
                Some("conv-9"),
                Some("resp-1"),
                Some("read /tmp/a"),
                Some(tool_call_turn("tc-9")),
                None,
            ),
            event_with_ids(
                2,
                3_000_000_000,
                Some("conv-9"),
                Some("resp-2"),
                None,
                None,
                Some(replayed_response("tc-9")),
            ),
        ];

        let doc = convert_session_to_atif("session-9", events).unwrap();

        let sources: Vec<StepSource> = doc.steps.iter().map(|s| s.source).collect();
        assert_eq!(
            sources,
            vec![StepSource::User, StepSource::Agent, StepSource::Agent]
        );

        let first = &doc.steps[1];
        let results = &first
            .observation
            .as_ref()
            .expect("calls of one conversation must stay in one group")
            .results;
        assert_eq!(results[0].source_call_id.as_deref(), Some("tc-9"));
    }

    #[test]
    fn session_export_emits_one_user_step_per_conversation() {
        let events = vec![
            event_with_ids(
                1,
                1_000_000_000,
                Some("conv-a"),
                Some("resp-a1"),
                Some("first question"),
                None,
                None,
            ),
            event_with_ids(
                2,
                3_000_000_000,
                Some("conv-b"),
                Some("resp-b1"),
                Some("second question"),
                None,
                None,
            ),
        ];

        let doc = convert_session_to_atif("session-multi", events).unwrap();

        let queries: Vec<&str> = doc
            .steps
            .iter()
            .filter(|s| s.source == StepSource::User)
            .map(|s| s.message.as_str())
            .collect();
        assert_eq!(queries, vec!["first question", "second question"]);
    }

    #[test]
    fn most_frequent_model_breaks_ties_by_name() {
        // Five models used once each: the counts map is a HashMap, whose
        // iteration order is randomized per map instance, so a single-key max
        // reported a different model from call to call.
        fn calls(models: &[&str]) -> Vec<TraceEventDetail> {
            models
                .iter()
                .enumerate()
                .map(|(i, model)| {
                    let mut e = call_event(
                        i as i64 + 1,
                        1_000_000_000 + i as i64 * 1_000_000,
                        None,
                        None,
                        Some("hi"),
                    );
                    e.model = Some((*model).to_string());
                    e
                })
                .collect()
        }

        let events = calls(&["zeta", "alpha", "delta", "echo", "bravo"]);
        let first = most_frequent_model(&events);
        assert_eq!(first.as_deref(), Some("alpha"));
        for _ in 0..8 {
            assert_eq!(
                most_frequent_model(&events),
                first,
                "a tie must not follow the map's iteration order"
            );
        }

        // The majority still wins over the name order.
        let with_majority = calls(&["zeta", "alpha", "delta", "echo", "bravo", "zeta", "zeta"]);
        assert_eq!(most_frequent_model(&with_majority).as_deref(), Some("zeta"));

        // No model recorded stays None.
        let mut none = calls(&["zeta", "alpha"]);
        for e in none.iter_mut() {
            e.model = None;
        }
        assert_eq!(most_frequent_model(&none), None);
    }
}
