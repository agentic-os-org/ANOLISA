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

    // Group events by trace_id, preserving order
    let trace_groups = group_by_trace(&events, &parsed);

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

/// Group events by trace_id, preserving chronological order.
/// Returns Vec of (events_in_trace, parsed_in_trace).
fn group_by_trace<'a>(
    events: &'a [TraceEventDetail],
    parsed: &'a [Option<LLMCall>],
) -> Vec<(Vec<&'a TraceEventDetail>, Vec<&'a Option<LLMCall>>)> {
    // Maintain insertion order using a Vec of (trace_id, indices)
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, event) in events.iter().enumerate() {
        let tid = event.conversation_id.clone().unwrap_or_default();
        if !groups.contains_key(&tid) {
            order.push(tid.clone());
        }
        groups.entry(tid).or_default().push(i);
    }

    order
        .into_iter()
        .filter_map(|tid| {
            let indices = groups.remove(&tid)?;
            let evts: Vec<_> = indices.iter().map(|&i| &events[i]).collect();
            let prs: Vec<_> = indices.iter().map(|&i| &parsed[i]).collect();
            Some((evts, prs))
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
        version: "1.0.0".to_string(),
        model_name,
        tool_definitions,
        extra: None,
    }
}

/// Find the most frequently used model across events.
fn most_frequent_model(events: &[TraceEventDetail]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for e in events {
        if let Some(ref m) = e.model {
            *counts.entry(m.as_str()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, c)| c)
        .map(|(m, _)| m.to_string())
}

/// Collect unique tool definitions from all parsed LLM calls.
fn collect_tool_definitions(parsed: &[Option<LLMCall>]) -> Option<Vec<serde_json::Value>> {
    let mut seen_names = std::collections::HashSet::new();
    let mut defs = Vec::new();

    for call in parsed.iter().filter_map(|p| p.as_ref()) {
        if let Some(ref tools) = call.request.tools {
            for tool in tools {
                // Extract tool name for deduplication
                let name = tool
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .or_else(|| tool.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                if seen_names.insert(name) {
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
        if let Ok(msgs) = serde_json::from_str::<Vec<InputMessage>>(json) {
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
        if let Ok(msgs) = serde_json::from_str::<Vec<InputMessage>>(json) {
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

    // Strategy 3: from input_messages column (already incremental — latest round)
    if results.is_empty() {
        if let Some(ref json) = next_event.input_messages {
            if let Ok(msgs) = serde_json::from_str::<Vec<InputMessage>>(json) {
                collect_tool_responses(&msgs, &tc_ids, &mut results, &mut matched_by_id);
            }
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(Observation { results })
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
fn collect_tool_responses(
    messages: &[InputMessage],
    tc_ids: &HashMap<&str, usize>,
    results: &mut Vec<ObservationResult>,
    matched: &mut [bool],
) {
    let mut positional_idx: usize = 0;
    for msg in messages {
        for part in &msg.parts {
            if let MessagePart::ToolCallResponse { id, response } = part {
                // Providers report tool failure out of band: Anthropic wraps a
                // failed result as `{"content": …, "is_error": true}`. Keep the
                // flag as structured data so consumers never have to re-derive
                // failure from the flattened text.
                let is_error = response.get(EXTRA_IS_ERROR).and_then(|v| v.as_bool());
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
                    response.get("content").unwrap_or(response)
                } else {
                    response
                };
                let content_str = match payload {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };

                // Match by ID first, tolerating the separator differences some
                // agents introduce when echoing a call id back.
                if let Some(tc_id) = id {
                    let found = tc_ids
                        .iter()
                        .find(|(known, _)| same_call_id(known, tc_id))
                        .map(|(_, idx)| *idx);
                    if let Some(idx) = found {
                        if !matched[idx] {
                            matched[idx] = true;
                            results.push(ObservationResult {
                                source_call_id: Some(tc_id.clone()),
                                content: Some(serde_json::Value::String(content_str)),
                                subagent_trajectory_ref: None,
                                extra: extra.clone(),
                            });
                            continue;
                        }
                    }
                }

                // Fallback: positional matching
                while positional_idx < matched.len() && matched[positional_idx] {
                    positional_idx += 1;
                }
                if positional_idx < matched.len() {
                    let source_call_id = id.clone().or_else(|| {
                        tc_ids.iter().find_map(|(known, idx)| {
                            (*idx == positional_idx).then(|| (*known).to_string())
                        })
                    });
                    matched[positional_idx] = true;
                    results.push(ObservationResult {
                        source_call_id,
                        content: Some(serde_json::Value::String(content_str)),
                        subagent_trajectory_ref: None,
                        extra: extra.clone(),
                    });
                    positional_idx += 1;
                }
            }
        }
    }
}

/// Compute aggregated final metrics.
fn compute_final_metrics(events: &[TraceEventDetail], total_steps: usize) -> FinalMetrics {
    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut total_cached: u64 = 0;

    for e in events {
        total_prompt += e.input_tokens as u64;
        total_completion += e.output_tokens as u64;
        if let Some(c) = e.cache_read_tokens {
            total_cached += c as u64;
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
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::genai::semantic::{InputMessage, MessagePart};

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
}
