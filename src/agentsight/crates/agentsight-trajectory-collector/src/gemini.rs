//! Gemini CLI chat JSONL → ATIF v1.7 converter.
//!
//! Gemini CLI (>= 0.39) stores each session under
//! `~/.gemini/tmp/<project>/chats/session-<timestamp>-<id8>.jsonl` as an
//! append-only record stream: a leading `PartialMetadataRecord`
//! (`{sessionId, projectHash, ...}`), then `MessageRecord`s (`{id, timestamp,
//! type: user|gemini|info|error|warning, content, ...}`, with `toolCalls?`/
//! `thoughts?`/`tokens?`/`model?` on `gemini` records) and the correction
//! records `{$rewindTo}`, `{$patch}` and `{$set}`. Older CLIs (<= 0.38) wrote
//! one pretty-printed JSON document with an embedded `messages` array — no
//! line parses as JSONL, so [`legacy_document`] absorbs the whole file and
//! the replay ingests it through the partial-metadata path.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use agentsight_atif::{
    Agent, AtifTrajectory, FinalMetrics, Metrics, Observation, ObservationResult, Step, StepSource,
    ToolCall, ATIF_SCHEMA_VERSION, EXTRA_IS_ERROR,
};

/// Return `true` when the events look like a Gemini CLI chat stream: the
/// leading partial-metadata record, a `$`-prefixed correction record, or a
/// message record (string `id` plus a chat `type`) — a combination
/// Claude-style events, which nest their payload under `message`, never hit.
pub fn is_gemini_chat(events: &[serde_json::Value]) -> bool {
    events.iter().any(|e| {
        is_partial_metadata(e)
            || e.get("$rewindTo").is_some_and(|v| v.is_string())
            || e.get("$patch").is_some_and(|v| v.is_object())
            || e.get("$set").is_some_and(|v| v.is_object())
            || (is_message_record(e)
                && matches!(
                    e.get("type").and_then(|v| v.as_str()),
                    Some("user") | Some("gemini")
                ))
    })
}

/// Parse a legacy (<= 0.38) conversation document from raw file content.
///
/// Those CLIs wrote the conversation with `JSON.stringify(conversation,
/// null, 2)`: multi-line pretty-printed JSON where no single line parses, so
/// the JSONL pass comes back empty. Mirrors the CLI's own whole-file
/// fallback: the content must parse as one partial-metadata record carrying
/// an embedded `messages` array.
pub fn legacy_document(content: &str) -> Option<serde_json::Value> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content).trim();
    let doc = serde_json::from_str::<serde_json::Value>(content).ok()?;
    (is_partial_metadata(&doc) && doc.get("messages").is_some_and(|m| m.is_array())).then_some(doc)
}

/// Conversation state after replaying the correction records.
struct Replay {
    metadata: serde_json::Map<String, serde_json::Value>,
    messages: Vec<serde_json::Value>,
}

/// Convert Gemini CLI chat events into an ATIF v1.7 trajectory.
///
/// One replayed `MessageRecord` becomes one step (`user` → user step,
/// `gemini` → agent step with thoughts, tool calls and tokens); Gemini never
/// splits one model turn across records the way Claude-style streams do.
pub fn convert_gemini_events(
    events: &[serde_json::Value],
    agent_name: &str,
) -> Result<AtifTrajectory> {
    let replay = replay_conversation(events);

    let mut steps: Vec<Step> = Vec::new();
    let mut step_id: usize = 0;
    let mut agent_model: Option<String> = None;

    for msg in &replay.messages {
        let ts = msg.get("timestamp").and_then(|v| v.as_str());
        match msg.get("type").and_then(|v| v.as_str()) {
            Some("user") => {
                let text = joined_text(msg.get("content"));
                if is_injected_user_content(&text) {
                    continue;
                }
                step_id += 1;
                steps.push(user_step(step_id, ts, text));
            }
            Some("gemini") => {
                let model = msg.get("model").and_then(|v| v.as_str()).map(String::from);
                if agent_model.is_none() {
                    agent_model = model.clone();
                }
                if gemini_record_is_empty(msg) {
                    continue;
                }
                step_id += 1;
                steps.push(gemini_step(msg, step_id, ts, model));
            }
            // info/error/warning records are CLI notices surfaced in the UI,
            // not conversation turns.
            _ => {}
        }
    }

    let final_metrics = sum_step_usage(&steps).map(|(pt, ct, cache)| FinalMetrics {
        total_prompt_tokens: Some(pt),
        total_completion_tokens: Some(ct),
        total_cached_tokens: Some(cache),
        total_cost_usd: None,
        total_steps: Some(steps.len()),
        extra: None,
    });

    Ok(AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.into(),
        session_id: replay
            .metadata
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(String::from),
        agent: Agent {
            name: agent_name.to_string(),
            // Chat files carry no CLI version.
            version: "unknown".into(),
            model_name: agent_model,
            tool_definitions: None,
            extra: None,
        },
        steps,
        trajectory_id: None,
        notes: None,
        final_metrics,
        continued_trajectory_ref: None,
        subagent_trajectories: None,
        extra: None,
    })
}

/// Gemini-private session info destined for the ATIF `extra` field:
/// replayed message counts plus the fallback project, mirroring
/// `qoder::extract_private_metadata`.
pub fn extract_private_metadata(
    events: &[serde_json::Value],
    fallback_project: &str,
) -> HashMap<String, serde_json::Value> {
    let replay = replay_conversation(events);
    let mut user_count = 0u64;
    let mut assistant_count = 0u64;

    for msg in &replay.messages {
        match msg.get("type").and_then(|v| v.as_str()) {
            Some("user") => {
                let text = joined_text(msg.get("content"));
                if !is_injected_user_content(&text) {
                    user_count += 1;
                }
            }
            // The converter skips empty gemini records, so a counted record
            // always claims a step of the trajectory it rides on.
            Some("gemini") => {
                if !gemini_record_is_empty(msg) {
                    assistant_count += 1;
                }
            }
            _ => {}
        }
    }

    let mut extra = HashMap::new();
    extra.insert(
        "user_message_count".into(),
        serde_json::Value::from(user_count),
    );
    extra.insert(
        "assistant_message_count".into(),
        serde_json::Value::from(assistant_count),
    );
    extra.insert(
        "project".into(),
        serde_json::Value::String(fallback_project.to_string()),
    );
    extra
}

/// Replay the append-only record stream into its final conversation state.
///
/// Discrimination and ordering mirror the CLI's own replay — rewind → patch →
/// message → `$set` → partial metadata — so a collected trajectory always
/// matches what the CLI would resume.
fn replay_conversation(events: &[serde_json::Value]) -> Replay {
    let mut replay = Replay {
        metadata: serde_json::Map::new(),
        messages: Vec::new(),
    };

    for event in events {
        if let Some(rewind_id) = event.get("$rewindTo").and_then(|v| v.as_str()) {
            match replay
                .messages
                .iter()
                .position(|m| message_id(m) == Some(rewind_id))
            {
                // The rewind target itself is removed too: the CLI rewinds to
                // the state *before* that message.
                Some(pos) => replay.messages.truncate(pos),
                None => replay.messages.clear(),
            }
        } else if let Some(patch) = event.get("$patch").filter(|v| v.is_object()) {
            apply_patch(&mut replay.messages, patch);
        } else if is_message_record(event) {
            upsert_message(&mut replay.messages, event);
        } else if let Some(set) = event.get("$set").filter(|v| v.is_object()) {
            if let Some(messages) = set.get("messages").and_then(|v| v.as_array()) {
                replay.messages.clear();
                for msg in messages {
                    if is_message_record(msg) {
                        upsert_message(&mut replay.messages, msg);
                    }
                }
            }
            merge_metadata(&mut replay.metadata, set);
        } else if is_partial_metadata(event) {
            if let Some(messages) = event.get("messages").and_then(|v| v.as_array()) {
                for msg in messages {
                    if is_message_record(msg) {
                        upsert_message(&mut replay.messages, msg);
                    }
                }
            }
            merge_metadata(&mut replay.metadata, event);
        }
    }
    replay
}

fn message_id(msg: &serde_json::Value) -> Option<&str> {
    msg.get("id").and_then(|v| v.as_str())
}

/// A chat message record: string `id` and no `$patch` key (the CLI's own
/// discriminator).
fn is_message_record(event: &serde_json::Value) -> bool {
    message_id(event).is_some() && event.get("$patch").is_none()
}

/// The leading/conversation metadata record.
fn is_partial_metadata(event: &serde_json::Value) -> bool {
    event.get("sessionId").and_then(|v| v.as_str()).is_some()
        && event.get("projectHash").and_then(|v| v.as_str()).is_some()
}

fn merge_metadata(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    source: &serde_json::Value,
) {
    if let Some(fields) = source.as_object() {
        metadata.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
}

/// Insert or replace a message by id, keeping its position (CLI id-keyed map).
fn upsert_message(messages: &mut Vec<serde_json::Value>, msg: &serde_json::Value) {
    let Some(id) = message_id(msg) else {
        return;
    };
    if let Some(existing) = messages.iter_mut().find(|m| message_id(m) == Some(id)) {
        *existing = msg.clone();
    } else {
        messages.push(msg.clone());
    }
}

/// Apply a `{$patch: {...}}` record in all four shapes the CLI writes:
/// one `{id, content?, toolCalls?}` patch, `updates`, `removeIds`, `orderIds`.
fn apply_patch(messages: &mut Vec<serde_json::Value>, patch: &serde_json::Value) {
    if patch.get("id").and_then(|v| v.as_str()).is_some() {
        apply_single_patch(messages, patch);
    }
    if let Some(updates) = patch.get("updates").and_then(|v| v.as_array()) {
        for update in updates {
            apply_single_patch(messages, update);
        }
    }
    if let Some(remove_ids) = patch.get("removeIds").and_then(|v| v.as_array()) {
        let ids: HashSet<&str> = remove_ids.iter().filter_map(|v| v.as_str()).collect();
        messages.retain(|m| !ids.contains(message_id(m).unwrap_or("")));
    }
    if let Some(order_ids) = patch.get("orderIds").and_then(|v| v.as_array()) {
        let ordered: Vec<&str> = order_ids.iter().filter_map(|v| v.as_str()).collect();
        let ordered_set: HashSet<&&str> = ordered.iter().collect();
        let mut result: Vec<serde_json::Value> = messages
            .iter()
            .filter(|m| !ordered_set.contains(&message_id(m).unwrap_or("")))
            .cloned()
            .collect();
        let mut seen = HashSet::new();
        for id in ordered {
            // Duplicate ids would otherwise duplicate the message; the CLI's
            // id-keyed map collapses them.
            if !seen.insert(id) {
                continue;
            }
            if let Some(msg) = messages.iter().find(|m| message_id(m) == Some(id)) {
                result.push(msg.clone());
            }
        }
        *messages = result;
    }
}

/// Patch one message: replace `content`, merge tool-call `result`s by id.
fn apply_single_patch(messages: &mut [serde_json::Value], patch: &serde_json::Value) {
    let Some(id) = patch.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(target) = messages.iter_mut().find(|m| message_id(m) == Some(id)) else {
        return;
    };
    if let Some(content) = patch.get("content").filter(|v| !v.is_null()) {
        if let Some(fields) = target.as_object_mut() {
            fields.insert("content".to_string(), content.clone());
        }
    }
    if let Some(call_patches) = patch.get("toolCalls").and_then(|v| v.as_array()) {
        if let Some(calls) = target.get_mut("toolCalls").and_then(|v| v.as_array_mut()) {
            for call_patch in call_patches {
                let Some(call_id) = call_patch.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(result) = call_patch.get("result") else {
                    continue;
                };
                if let Some(call) = calls
                    .iter_mut()
                    .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(call_id))
                {
                    if let Some(fields) = call.as_object_mut() {
                        fields.insert("result".to_string(), result.clone());
                    }
                }
            }
        }
    }
}

fn user_step(step_id: usize, ts: Option<&str>, message: String) -> Step {
    Step {
        step_id,
        timestamp: ts.map(String::from),
        source: StepSource::User,
        message,
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

/// Build the agent step for one `gemini` record: text message, thought
/// summaries as reasoning, tool calls with their (patched-in) results as
/// observations, and the record's token summary as metrics.
fn gemini_step(
    msg: &serde_json::Value,
    step_id: usize,
    ts: Option<&str>,
    model: Option<String>,
) -> Step {
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut observations: Vec<ObservationResult> = Vec::new();
    if let Some(calls) = msg.get("toolCalls").and_then(|v| v.as_array()) {
        for call in calls {
            let call_id = call.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = call
                .get("args")
                .cloned()
                .filter(|v| v.is_object())
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            tool_calls.push(ToolCall {
                tool_call_id: call_id.to_string(),
                function_name: call
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                arguments,
                extra: None,
            });
            if let Some(result) = call.get("result").filter(|v| !v.is_null()) {
                let failed = call.get("status").and_then(|v| v.as_str()) == Some("error");
                observations.push(ObservationResult {
                    source_call_id: Some(call_id.to_string()),
                    content: Some(serde_json::Value::String(joined_text(Some(result)))),
                    subagent_trajectory_ref: None,
                    extra: failed.then(|| {
                        HashMap::from([(EXTRA_IS_ERROR.to_string(), serde_json::Value::Bool(true))])
                    }),
                });
            }
        }
    }

    let reasoning = msg
        .get("thoughts")
        .and_then(|v| v.as_array())
        .map(|thoughts| {
            thoughts
                .iter()
                .filter_map(|t| {
                    ["description", "subject"]
                        .into_iter()
                        .find_map(|k| t.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
                        .map(String::from)
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty());

    Step {
        step_id,
        timestamp: ts.map(String::from),
        source: StepSource::Agent,
        message: joined_text(msg.get("content")),
        model_name: model,
        reasoning_effort: None,
        reasoning_content: reasoning,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        observation: (!observations.is_empty()).then_some(Observation {
            results: observations,
        }),
        metrics: token_metrics(msg.get("tokens")),
        extra: None,
        llm_call_count: None,
        is_copied_context: None,
    }
}

/// Whether a `gemini` record carries none of the mapped content; skipping
/// them keeps step ids contiguous with the visible conversation.
fn gemini_record_is_empty(msg: &serde_json::Value) -> bool {
    joined_text(msg.get("content")).is_empty()
        && msg
            .get("toolCalls")
            .and_then(|v| v.as_array())
            .is_none_or(|calls| calls.is_empty())
        && msg
            .get("thoughts")
            .and_then(|v| v.as_array())
            .is_none_or(|thoughts| thoughts.is_empty())
        && msg.get("tokens").filter(|v| v.is_object()).is_none()
}

/// Token summary `{input, output, cached, ...}` → ATIF step metrics; cached
/// is reported as-is, matching the Codex and Claude converters.
fn token_metrics(tokens: Option<&serde_json::Value>) -> Option<Metrics> {
    let tokens = tokens.filter(|v| v.is_object())?;
    let prompt = tokens.get("input").and_then(|v| v.as_u64());
    let completion = tokens.get("output").and_then(|v| v.as_u64());
    if prompt.is_none() && completion.is_none() {
        return None;
    }
    Some(Metrics {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cached_tokens: tokens.get("cached").and_then(|v| v.as_u64()),
        cost_usd: None,
        logprobs: None,
        completion_token_ids: None,
        prompt_token_ids: None,
        extra: None,
    })
}

/// User content the CLI injects on its own (session/hook context) rather
/// than typed by the human. Slash commands are kept: they are real user
/// input even though the CLI excludes them from resume flows.
fn is_injected_user_content(text: &str) -> bool {
    text.is_empty() || text.starts_with("<session_context>") || text.starts_with("<hook_context>")
}

/// Sum per-step usage into final totals (Gemini reports per-message only).
fn sum_step_usage(steps: &[Step]) -> Option<(u64, u64, u64)> {
    let mut totals = (0u64, 0u64, 0u64);
    let mut seen = false;
    for s in steps {
        if let Some(m) = &s.metrics {
            seen = true;
            totals.0 += m.prompt_tokens.unwrap_or(0);
            totals.1 += m.completion_tokens.unwrap_or(0);
            totals.2 += m.cached_tokens.unwrap_or(0);
        }
    }
    seen.then_some(totals)
}

/// Join text from a content value: a plain string as-is, or an array of
/// parts keeping their `.text` fields (other parts contribute nothing).
fn joined_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Array(parts)) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                .collect();
            texts.join("\n").trim().to_string()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qoder::load_jsonl_events;

    /// Fixture modeled on a real `~/.gemini/tmp/<project>/chats/session-*.jsonl`.
    fn fixture_events() -> Vec<serde_json::Value> {
        let content = concat!(
            "{\"sessionId\":\"e5a1c0de-1234-4abc-9def-aabbccddeeff\",\"projectHash\":\"a1b2c3d4\",\"startTime\":\"2026-10-22T09:00:00.000Z\",\"lastUpdated\":\"2026-10-22T09:01:00.000Z\"}\n",
            "{\"id\":\"msg-1\",\"timestamp\":\"2026-10-22T09:00:01.000Z\",\"type\":\"user\",\"content\":\"Where are session files stored?\"}\n",
            "{\"id\":\"msg-2\",\"timestamp\":\"2026-10-22T09:00:05.000Z\",\"type\":\"gemini\",\"content\":[{\"text\":\"I will look for them.\"}],\"model\":\"gemini-2.5-pro\",\"thoughts\":[{\"subject\":\"plan\",\"description\":\"search the tmp dirs\"}],\"toolCalls\":[{\"id\":\"tc-1\",\"name\":\"run_shell_command\",\"args\":{\"command\":\"ls ~/.gemini/tmp\"},\"status\":\"scheduled\",\"timestamp\":\"2026-10-22T09:00:05.100Z\"}],\"tokens\":{\"input\":120,\"output\":30,\"cached\":10,\"total\":150}}\n",
            "{\"$patch\":{\"updates\":[{\"id\":\"msg-2\",\"toolCalls\":[{\"id\":\"tc-1\",\"result\":\"chats\\n\"}]}]}}\n",
            "{\"id\":\"msg-3\",\"timestamp\":\"2026-10-22T09:00:20.000Z\",\"type\":\"gemini\",\"content\":[{\"text\":\"Under ~/.gemini/tmp/<project>/chats.\"}],\"model\":\"gemini-2.5-pro\",\"tokens\":{\"input\":200,\"output\":12,\"cached\":50,\"total\":212}}\n",
            "{\"id\":\"msg-4\",\"timestamp\":\"2026-10-22T09:00:25.000Z\",\"type\":\"info\",\"content\":\"Model switched\"}\n",
        );
        load_jsonl_events(content)
    }

    #[test]
    fn test_is_gemini_chat() {
        assert!(is_gemini_chat(&fixture_events()));
        // Claude-style and Codex-style streams must not be detected.
        let claude = load_jsonl_events(
            "{\"type\":\"user\",\"timestamp\":\"2026-07-25T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );
        assert!(!is_gemini_chat(&claude));
        let codex = load_jsonl_events(
            "{\"timestamp\":\"2026-08-03T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"s-1\"}}\n",
        );
        assert!(!is_gemini_chat(&codex));
    }

    #[test]
    fn test_convert_basic_flow() {
        let traj = convert_gemini_events(&fixture_events(), "gemini-cli").unwrap();

        assert_eq!(traj.schema_version, ATIF_SCHEMA_VERSION);
        assert_eq!(
            traj.session_id.as_deref(),
            Some("e5a1c0de-1234-4abc-9def-aabbccddeeff")
        );
        assert_eq!(traj.agent.name, "gemini-cli");
        assert_eq!(traj.agent.model_name.as_deref(), Some("gemini-2.5-pro"));
        traj.validate_step_ids().unwrap();

        // user + two agent steps; the info notice creates no step.
        assert_eq!(traj.steps.len(), 3, "steps: {:?}", traj.steps);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "Where are session files stored?");

        let first = &traj.steps[1];
        assert_eq!(first.source, StepSource::Agent);
        assert_eq!(first.message, "I will look for them.");
        assert_eq!(first.model_name.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(
            first.reasoning_content.as_deref(),
            Some("search the tmp dirs")
        );

        // The tool result arrives via $patch and becomes an observation.
        let calls = first.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function_name, "run_shell_command");
        assert_eq!(calls[0].tool_call_id, "tc-1");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"command": "ls ~/.gemini/tmp"})
        );
        let obs = first.observation.as_ref().unwrap();
        assert_eq!(obs.results.len(), 1);
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("tc-1"));
        // The result's trailing newline is trimmed, like every text join.
        assert_eq!(obs.results[0].content, Some(serde_json::json!("chats")));

        let metrics = first.metrics.as_ref().unwrap();
        assert_eq!(metrics.prompt_tokens, Some(120));
        assert_eq!(metrics.completion_tokens, Some(30));
        assert_eq!(metrics.cached_tokens, Some(10));

        let final_metrics = traj.final_metrics.as_ref().unwrap();
        assert_eq!(final_metrics.total_prompt_tokens, Some(320));
        assert_eq!(final_metrics.total_completion_tokens, Some(42));
        assert_eq!(final_metrics.total_cached_tokens, Some(60));
        assert_eq!(final_metrics.total_steps, Some(3));
    }

    #[test]
    fn test_convert_rewind_and_patch_content() {
        // msg-1/2 are appended, then the user message is rewritten by a
        // patch and the turn is rewound before a fresh exchange.
        let content = concat!(
            "{\"sessionId\":\"s-rew\",\"projectHash\":\"h\"}\n",
            "{\"id\":\"m1\",\"timestamp\":\"2026-10-22T09:00:01Z\",\"type\":\"user\",\"content\":\"draft question\"}\n",
            "{\"id\":\"m2\",\"timestamp\":\"2026-10-22T09:00:02Z\",\"type\":\"gemini\",\"content\":\"draft answer\"}\n",
            "{\"$patch\":{\"id\":\"m1\",\"content\":\"edited question\"}}\n",
            "{\"$rewindTo\":\"m1\"}\n",
            "{\"id\":\"m3\",\"timestamp\":\"2026-10-22T09:00:10Z\",\"type\":\"user\",\"content\":\"final question\"}\n",
            "{\"id\":\"m4\",\"timestamp\":\"2026-10-22T09:00:11Z\",\"type\":\"gemini\",\"content\":\"final answer\"}\n",
        );
        let traj = convert_gemini_events(&load_jsonl_events(content), "gemini-cli").unwrap();
        assert_eq!(traj.steps.len(), 2);
        assert_eq!(traj.steps[0].message, "final question");
        assert_eq!(traj.steps[1].message, "final answer");

        // Counts describe the replayed state, not the raw records.
        let extra = extract_private_metadata(&load_jsonl_events(content), "proj");
        assert_eq!(extra["user_message_count"], serde_json::json!(1));
        assert_eq!(extra["assistant_message_count"], serde_json::json!(1));
    }

    #[test]
    fn test_convert_remove_and_order_patch() {
        let content = concat!(
            "{\"sessionId\":\"s-ord\",\"projectHash\":\"h\"}\n",
            "{\"id\":\"u1\",\"timestamp\":\"2026-10-22T09:00:01Z\",\"type\":\"user\",\"content\":\"q1\"}\n",
            "{\"id\":\"g1\",\"timestamp\":\"2026-10-22T09:00:02Z\",\"type\":\"gemini\",\"content\":\"a1\"}\n",
            "{\"id\":\"g2\",\"timestamp\":\"2026-10-22T09:00:03Z\",\"type\":\"gemini\",\"content\":\"a2\"}\n",
            "{\"$patch\":{\"removeIds\":[\"g1\"],\"orderIds\":[\"u1\",\"g2\"]}}\n",
        );
        let traj = convert_gemini_events(&load_jsonl_events(content), "gemini-cli").unwrap();
        assert_eq!(traj.steps.len(), 2);
        assert_eq!(traj.steps[0].message, "q1");
        assert_eq!(traj.steps[1].message, "a2");
    }

    #[test]
    fn test_convert_failed_tool_call_marks_error() {
        let content = concat!(
            "{\"sessionId\":\"s-err\",\"projectHash\":\"h\"}\n",
            "{\"id\":\"g1\",\"timestamp\":\"2026-10-22T09:00:02Z\",\"type\":\"gemini\",\"content\":\"trying\",\"toolCalls\":[{\"id\":\"tc-9\",\"name\":\"run_shell_command\",\"args\":{\"command\":\"boom\"},\"status\":\"error\",\"result\":\"command not found\"}]}\n",
        );
        let traj = convert_gemini_events(&load_jsonl_events(content), "gemini-cli").unwrap();
        let obs = traj.steps[0].observation.as_ref().unwrap();
        assert_eq!(
            obs.results[0].extra.as_ref().unwrap()[EXTRA_IS_ERROR],
            serde_json::json!(true)
        );
    }

    #[test]
    fn test_convert_legacy_single_document() {
        // <= 0.38 wrote `JSON.stringify(conversation, null, 2)`: multi-line
        // JSON where no line parses, so only the whole-file fallback works.
        let content = concat!(
            "{\n",
            "  \"sessionId\": \"s-legacy\",\n",
            "  \"projectHash\": \"h\",\n",
            "  \"messages\": [\n",
            "    {\n",
            "      \"id\": \"m1\",\n",
            "      \"timestamp\": \"2026-10-22T08:00:01Z\",\n",
            "      \"type\": \"user\",\n",
            "      \"content\": \"legacy question\"\n",
            "    },\n",
            "    {\n",
            "      \"id\": \"m2\",\n",
            "      \"timestamp\": \"2026-10-22T08:00:02Z\",\n",
            "      \"type\": \"gemini\",\n",
            "      \"content\": \"legacy answer\"\n",
            "    }\n",
            "  ]\n",
            "}\n",
        );
        assert!(load_jsonl_events(content).is_empty());
        let doc = legacy_document(content).expect("whole-file fallback parses");
        assert!(is_gemini_chat(std::slice::from_ref(&doc)));
        let traj = convert_gemini_events(std::slice::from_ref(&doc), "gemini-cli").unwrap();
        assert_eq!(traj.session_id.as_deref(), Some("s-legacy"));
        assert_eq!(traj.steps.len(), 2);
        assert_eq!(traj.steps[0].message, "legacy question");
        assert_eq!(traj.steps[1].message, "legacy answer");
        // Pretty-printed foreign JSON is not absorbed.
        assert!(legacy_document("{\n  \"type\": \"user\"\n}\n").is_none());
    }

    #[test]
    fn test_injected_user_context_is_skipped() {
        let content = concat!(
            "{\"sessionId\":\"s-ctx\",\"projectHash\":\"h\"}\n",
            "{\"id\":\"c1\",\"timestamp\":\"2026-10-22T09:00:00Z\",\"type\":\"user\",\"content\":\"<session_context>workspace mounted</session_context>\"}\n",
            "{\"id\":\"u1\",\"timestamp\":\"2026-10-22T09:00:01Z\",\"type\":\"user\",\"content\":\"real question\"}\n",
            "{\"id\":\"g1\",\"timestamp\":\"2026-10-22T09:00:02Z\",\"type\":\"gemini\",\"content\":\"answer\"}\n",
        );
        let events = load_jsonl_events(content);
        let traj = convert_gemini_events(&events, "gemini-cli").unwrap();
        assert_eq!(traj.steps.len(), 2);
        assert_eq!(traj.steps[0].message, "real question");

        let extra = extract_private_metadata(&events, "proj");
        assert_eq!(extra["user_message_count"], serde_json::json!(1));
        assert_eq!(extra["project"], serde_json::json!("proj"));
    }
}
