//! Codex CLI rollout JSONL → ATIF v1.7 converter.
//!
//! Codex stores sessions under `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`
//! (and `~/.codex/archived_sessions/`). Unlike the Claude-style schema, each
//! line is an envelope `{"timestamp","type","payload"}` where:
//!   - `session_meta`: session_id / cwd / cli_version / model_provider
//!   - `turn_context`: per-turn model + reasoning effort
//!   - `response_item`: payload.type ∈ `message` (role user/assistant/developer,
//!     content blocks `input_text`/`output_text`), `reasoning`,
//!     `custom_tool_call`/`function_call`/`local_shell_call` and their
//!     `*_output` counterparts
//!   - `event_msg`: payload.type ∈ `user_message` (the authoritative user
//!     input; the role=user `response_item` also carries injected context),
//!     `token_count`, `agent_message` (duplicate of assistant response_item)

use std::collections::HashMap;

use anyhow::Result;

use agentsight_atif::{
    Agent, AtifTrajectory, FinalMetrics, Metrics, Observation, ObservationResult, Step, StepSource,
    ToolCall, ATIF_SCHEMA_VERSION,
};

/// Return `true` when the events look like a Codex rollout stream
/// (envelope records carrying a `payload` object).
pub fn is_codex_rollout(events: &[serde_json::Value]) -> bool {
    events.iter().any(|e| {
        matches!(
            e.get("type").and_then(|v| v.as_str()),
            Some("session_meta") | Some("response_item") | Some("turn_context")
        ) && e.get("payload").is_some_and(|p| p.is_object())
    })
}

/// Accumulates one agent turn (reasoning + tool calls + assistant text)
/// before it is flushed into a single ATIF agent step.
#[derive(Default)]
struct AgentTurn {
    timestamp: Option<String>,
    model: Option<String>,
    effort: Option<serde_json::Value>,
    reasoning: Vec<String>,
    messages: Vec<String>,
    tool_calls: Vec<ToolCall>,
    observations: Vec<ObservationResult>,
    metrics: Option<Metrics>,
}

impl AgentTurn {
    fn is_empty(&self) -> bool {
        self.reasoning.is_empty()
            && self.messages.is_empty()
            && self.tool_calls.is_empty()
            && self.observations.is_empty()
            && self.metrics.is_none()
    }

    fn into_step(self, step_id: usize) -> Step {
        Step {
            step_id,
            timestamp: self.timestamp,
            source: StepSource::Agent,
            message: self.messages.join("\n"),
            model_name: self.model,
            reasoning_effort: self.effort,
            reasoning_content: if self.reasoning.is_empty() {
                None
            } else {
                Some(self.reasoning.join("\n"))
            },
            tool_calls: if self.tool_calls.is_empty() {
                None
            } else {
                Some(self.tool_calls)
            },
            observation: if self.observations.is_empty() {
                None
            } else {
                Some(Observation {
                    results: self.observations,
                })
            },
            metrics: self.metrics,
            extra: None,
            llm_call_count: None,
            is_copied_context: None,
        }
    }
}

/// Convert Codex rollout events into an ATIF v1.7 trajectory.
pub fn convert_codex_events(
    events: &[serde_json::Value],
    agent_name: &str,
) -> Result<AtifTrajectory> {
    let mut session_id: Option<String> = None;
    let mut version: Option<String> = None;
    let mut agent_model: Option<String> = None;

    // Newer CLIs emit `event_msg/user_message` for the real user input; when
    // present, role=user response_items (which also carry injected
    // environment/AGENTS.md context) are skipped to avoid duplicates.
    let has_user_event_msg = events
        .iter()
        .any(|e| envelope_type(e) == "event_msg" && payload_type(e) == "user_message");

    let mut steps: Vec<Step> = Vec::new();
    let mut step_id: usize = 0;
    let mut turn: Option<AgentTurn> = None;
    // Latest turn_context values, applied to the next agent turn.
    let mut current_model: Option<String> = None;
    let mut current_effort: Option<serde_json::Value> = None;
    // Cumulative usage from the last token_count event (authoritative totals).
    let mut last_total_usage: Option<(u64, u64, u64)> = None;

    let flush = |turn: &mut Option<AgentTurn>, steps: &mut Vec<Step>, step_id: &mut usize| {
        if let Some(t) = turn.take() {
            if !t.is_empty() {
                *step_id += 1;
                steps.push(t.into_step(*step_id));
            }
        }
    };

    for e in events {
        let ts = e.get("timestamp").and_then(|v| v.as_str());
        let payload = e.get("payload").unwrap_or(&serde_json::Value::Null);

        match envelope_type(e) {
            "session_meta" => {
                if session_id.is_none() {
                    session_id = payload
                        .get("session_id")
                        .or_else(|| payload.get("id"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                if version.is_none() {
                    version = payload
                        .get("cli_version")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
            "turn_context" => {
                current_model = payload
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                current_effort = payload.get("effort").cloned().filter(|v| !v.is_null());
                if agent_model.is_none() {
                    agent_model = current_model.clone();
                }
            }
            "event_msg" => match payload_type(e) {
                "user_message" => {
                    flush(&mut turn, &mut steps, &mut step_id);
                    let text = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    step_id += 1;
                    steps.push(user_step(step_id, ts, text.to_string()));
                }
                "token_count" => {
                    if let Some(info) = payload.get("info") {
                        if let Some(total) = usage_triple(info.get("total_token_usage")) {
                            last_total_usage = Some(total);
                        }
                        if let Some((pt, ct, cache)) = usage_triple(info.get("last_token_usage")) {
                            let t = ensure_turn(&mut turn, ts, &current_model, &current_effort);
                            let m = t.metrics.get_or_insert_with(zero_metrics);
                            *m.prompt_tokens.get_or_insert(0) += pt;
                            *m.completion_tokens.get_or_insert(0) += ct;
                            *m.cached_tokens.get_or_insert(0) += cache;
                        }
                    }
                }
                // `agent_message` duplicates the assistant response_item.
                _ => {}
            },
            "response_item" => match payload_type(e) {
                "message" => {
                    let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    match role {
                        "assistant" => {
                            let text = joined_text(payload.get("content"));
                            if !text.is_empty() {
                                let t = ensure_turn(&mut turn, ts, &current_model, &current_effort);
                                t.messages.push(text);
                            }
                        }
                        "user" if !has_user_event_msg => {
                            flush(&mut turn, &mut steps, &mut step_id);
                            let text = joined_text(payload.get("content"));
                            if !text.is_empty() {
                                step_id += 1;
                                steps.push(user_step(step_id, ts, text));
                            }
                        }
                        // developer/system prompts and injected user context
                        _ => {}
                    }
                }
                "reasoning" => {
                    // Only the plaintext summary is usable; encrypted_content
                    // cannot be decoded offline.
                    let summary = joined_text(payload.get("summary"));
                    if !summary.is_empty() {
                        let t = ensure_turn(&mut turn, ts, &current_model, &current_effort);
                        t.reasoning.push(summary);
                    }
                }
                "custom_tool_call" | "function_call" | "local_shell_call" => {
                    let t = ensure_turn(&mut turn, ts, &current_model, &current_effort);
                    t.tool_calls.push(extract_tool_call(payload));
                }
                "custom_tool_call_output" | "function_call_output" | "local_shell_call_output" => {
                    let call_id = payload
                        .get("call_id")
                        .or_else(|| payload.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let content = joined_output(payload.get("output"));
                    let t = ensure_turn(&mut turn, ts, &current_model, &current_effort);
                    t.observations.push(ObservationResult {
                        source_call_id: Some(call_id),
                        content: Some(serde_json::Value::String(content)),
                        subagent_trajectory_ref: None,
                        extra: None,
                    });
                }
                _ => {}
            },
            // world_state / compacted / unknown types
            _ => {}
        }
    }
    flush(&mut turn, &mut steps, &mut step_id);

    // Prefer the cumulative totals reported by the last token_count event;
    // fall back to summing per-step metrics.
    let final_metrics =
        last_total_usage
            .or_else(|| sum_step_usage(&steps))
            .map(|(pt, ct, cache)| FinalMetrics {
                total_prompt_tokens: Some(pt),
                total_completion_tokens: Some(ct),
                total_cached_tokens: Some(cache),
                total_cost_usd: None,
                total_steps: Some(steps.len()),
                extra: None,
            });

    Ok(AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.into(),
        session_id,
        agent: Agent {
            name: agent_name.to_string(),
            version: version.unwrap_or_else(|| "unknown".into()),
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

/// Codex-private session info destined for the ATIF `extra` field.
///
/// Mirrors `qoder::extract_private_metadata`: returns `cwd` (from
/// session_meta), message counts and `project`. The project name is derived
/// from the cwd basename because the flat sessions layout has no per-project
/// directories. Paths are assumed POSIX: the collector only runs on Linux
/// hosts and `cwd` is produced by the same machine.
pub fn extract_private_metadata(
    events: &[serde_json::Value],
    fallback_project: &str,
) -> HashMap<String, serde_json::Value> {
    let mut cwd: Option<String> = None;
    let mut user_count: u64 = 0;
    let mut assistant_count: u64 = 0;

    for e in events {
        let payload = e.get("payload").unwrap_or(&serde_json::Value::Null);
        match envelope_type(e) {
            "session_meta" => {
                if cwd.is_none() {
                    cwd = payload
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
            "event_msg" if payload_type(e) == "user_message" => user_count += 1,
            "response_item" if payload_type(e) == "message" => {
                if let Some("assistant") = payload.get("role").and_then(|v| v.as_str()) {
                    assistant_count += 1
                }
            }
            _ => {}
        }
    }

    let project = cwd
        .as_deref()
        .and_then(|c| std::path::Path::new(c).file_name())
        .map(|n| n.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_project.to_string());

    let mut extra = HashMap::new();
    if let Some(cwd) = cwd {
        extra.insert("cwd".to_string(), serde_json::Value::String(cwd));
    }
    extra.insert(
        "user_message_count".to_string(),
        serde_json::Value::from(user_count),
    );
    extra.insert(
        "assistant_message_count".to_string(),
        serde_json::Value::from(assistant_count),
    );
    extra.insert("project".to_string(), serde_json::Value::String(project));
    extra
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn envelope_type(e: &serde_json::Value) -> &str {
    e.get("type").and_then(|v| v.as_str()).unwrap_or("")
}

fn payload_type(e: &serde_json::Value) -> &str {
    e.get("payload")
        .and_then(|p| p.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
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

fn ensure_turn<'a>(
    turn: &'a mut Option<AgentTurn>,
    ts: Option<&str>,
    model: &Option<String>,
    effort: &Option<serde_json::Value>,
) -> &'a mut AgentTurn {
    turn.get_or_insert_with(|| AgentTurn {
        timestamp: ts.map(String::from),
        model: model.clone(),
        effort: effort.clone(),
        ..Default::default()
    })
}

fn zero_metrics() -> Metrics {
    Metrics {
        prompt_tokens: Some(0),
        completion_tokens: Some(0),
        cached_tokens: Some(0),
        cost_usd: None,
        logprobs: None,
        completion_token_ids: None,
        prompt_token_ids: None,
        extra: None,
    }
}

/// `(input_tokens, output_tokens, cached_input_tokens)` from a usage object.
fn usage_triple(usage: Option<&serde_json::Value>) -> Option<(u64, u64, u64)> {
    let u = usage?;
    let pt = u.get("input_tokens").and_then(|v| v.as_u64());
    let ct = u.get("output_tokens").and_then(|v| v.as_u64());
    if pt.is_none() && ct.is_none() {
        return None;
    }
    let cache = u
        .get("cached_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Some((pt.unwrap_or(0), ct.unwrap_or(0), cache))
}

fn sum_step_usage(steps: &[Step]) -> Option<(u64, u64, u64)> {
    let mut pt = 0u64;
    let mut ct = 0u64;
    let mut cache = 0u64;
    let mut seen = false;
    for s in steps {
        if let Some(m) = &s.metrics {
            seen = true;
            pt += m.prompt_tokens.unwrap_or(0);
            ct += m.completion_tokens.unwrap_or(0);
            cache += m.cached_tokens.unwrap_or(0);
        }
    }
    seen.then_some((pt, ct, cache))
}

/// Join text from a content/summary array (`input_text` / `output_text` /
/// `summary_text` / `text` blocks) or return a plain string as-is.
fn joined_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Array(blocks)) => {
            let parts: Vec<&str> = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                .collect();
            parts.join("\n").trim().to_string()
        }
        _ => String::new(),
    }
}

/// Tool output can be a plain string or an array of `{type,text}` blocks.
fn joined_output(output: Option<&serde_json::Value>) -> String {
    joined_text(output)
}

/// Map a Codex tool-call payload to an ATIF `ToolCall`.
fn extract_tool_call(payload: &serde_json::Value) -> ToolCall {
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            if payload_type_str(payload) == "local_shell_call" {
                "shell"
            } else {
                ""
            }
        })
        .to_string();

    // `custom_tool_call.input` / `function_call.arguments` are JSON strings;
    // `local_shell_call.action` is already an object.
    let raw = payload
        .get("input")
        .or_else(|| payload.get("arguments"))
        .or_else(|| payload.get("action"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let mut arguments = match raw {
        serde_json::Value::String(s) => {
            serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({ "raw": s }))
        }
        other => other,
    };
    if !arguments.is_object() {
        arguments = serde_json::json!({ "value": arguments });
    }

    ToolCall {
        tool_call_id: payload
            .get("call_id")
            .or_else(|| payload.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        function_name: name,
        arguments,
        extra: None,
    }
}

fn payload_type_str(payload: &serde_json::Value) -> &str {
    payload.get("type").and_then(|v| v.as_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qoder::load_jsonl_events;

    /// Fixture modeled on a real `~/.codex/sessions/**/rollout-*.jsonl` file.
    fn fixture_events() -> Vec<serde_json::Value> {
        let content = concat!(
            "{\"timestamp\":\"2026-08-03T09:56:48.054Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"019fc70d-ebc4-77f2-9a5c-937b8ff496c5\",\"cwd\":\"/Users/u/vscode/sysom-dev\",\"originator\":\"Codex Desktop\",\"cli_version\":\"0.146.0\",\"model_provider\":\"openai\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:48.055Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t-1\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:52.360Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"<app-context>injected</app-context>\"}]}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:52.360Z\",\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t-1\",\"cwd\":\"/Users/u/vscode/sysom-dev\",\"model\":\"gpt-5.6-sol\",\"effort\":\"medium\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:52.360Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>injected</environment_context>\"}]}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:52.374Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"会话记录存在哪个文件里面\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:57.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"check codex dirs\"}],\"encrypted_content\":\"gAAA...\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:58.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"id\":\"ctc_1\",\"call_id\":\"call_1\",\"name\":\"exec\",\"input\":\"{\\\"cmd\\\":\\\"ls ~/.codex\\\"}\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:59.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"call_id\":\"call_1\",\"output\":[{\"type\":\"input_text\",\"text\":\"sessions\\n\"}]}}\n",
            "{\"timestamp\":\"2026-08-03T09:57:00.153Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"记录在 ~/.codex/sessions 下\"}]}}\n",
            "{\"timestamp\":\"2026-08-03T09:57:00.200Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"记录在 ~/.codex/sessions 下\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:57:00.500Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":22161,\"cached_input_tokens\":11008,\"output_tokens\":188,\"total_tokens\":22349},\"last_token_usage\":{\"input_tokens\":22161,\"cached_input_tokens\":11008,\"output_tokens\":188,\"total_tokens\":22349}}}}\n",
            "{\"timestamp\":\"2026-08-03T09:57:00.600Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t-1\",\"last_agent_message\":\"记录在 ~/.codex/sessions 下\"}}\n",
        );
        load_jsonl_events(content)
    }

    #[test]
    fn test_is_codex_rollout() {
        assert!(is_codex_rollout(&fixture_events()));
        // Claude-style events must not be detected as Codex
        let claude = load_jsonl_events(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );
        assert!(!is_codex_rollout(&claude));
    }

    #[test]
    fn test_convert_basic_flow() {
        let events = fixture_events();
        let traj = convert_codex_events(&events, "codex").unwrap();

        assert_eq!(traj.schema_version, ATIF_SCHEMA_VERSION);
        assert_eq!(
            traj.session_id.as_deref(),
            Some("019fc70d-ebc4-77f2-9a5c-937b8ff496c5")
        );
        assert_eq!(traj.agent.name, "codex");
        assert_eq!(traj.agent.version, "0.146.0");
        assert_eq!(traj.agent.model_name.as_deref(), Some("gpt-5.6-sol"));
        traj.validate_step_ids().unwrap();

        // user + one merged agent turn; injected developer/user context and
        // the duplicate event_msg/agent_message must not create steps.
        assert_eq!(traj.steps.len(), 2, "steps: {:?}", traj.steps);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "会话记录存在哪个文件里面");

        let agent_step = &traj.steps[1];
        assert_eq!(agent_step.source, StepSource::Agent);
        assert_eq!(agent_step.message, "记录在 ~/.codex/sessions 下");
        assert_eq!(agent_step.model_name.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(
            agent_step.reasoning_content.as_deref(),
            Some("check codex dirs")
        );
        let tcs = agent_step.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function_name, "exec");
        assert_eq!(tcs[0].tool_call_id, "call_1");
        assert_eq!(tcs[0].arguments["cmd"], "ls ~/.codex");
        let obs = agent_step.observation.as_ref().unwrap();
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("call_1"));

        // Metrics: per-step from last_token_usage, totals from
        // total_token_usage of the last token_count event.
        let m = agent_step.metrics.as_ref().unwrap();
        assert_eq!(m.prompt_tokens, Some(22161));
        assert_eq!(m.completion_tokens, Some(188));
        let fm = traj.final_metrics.as_ref().unwrap();
        assert_eq!(fm.total_prompt_tokens, Some(22161));
        assert_eq!(fm.total_completion_tokens, Some(188));
        assert_eq!(fm.total_cached_tokens, Some(11008));
    }

    #[test]
    fn test_fallback_user_from_response_item_without_event_msg() {
        // Older rollouts without event_msg/user_message must fall back to
        // role=user response_items for user steps.
        let content = concat!(
            "{\"timestamp\":\"2026-08-03T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"s-1\",\"cwd\":\"/w/app\",\"cli_version\":\"0.1.0\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hello\"}]}}\n",
            "{\"timestamp\":\"2026-08-03T09:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\n",
        );
        let events = load_jsonl_events(content);
        let traj = convert_codex_events(&events, "codex").unwrap();
        assert_eq!(traj.steps.len(), 2);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "hello");
        assert_eq!(traj.steps[1].message, "hi");
    }

    #[test]
    fn test_tool_call_variants() {
        // function_call carries a JSON-string `arguments`; local_shell_call
        // has an `action` object and no name (defaults to "shell");
        // function_call_output may be a plain string; local_shell_call_output
        // may carry only `id` instead of `call_id`.
        let content = concat!(
            "{\"timestamp\":\"2026-08-03T10:00:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"fc_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"/tmp/a\\\"}\"}}\n",
            "{\"timestamp\":\"2026-08-03T10:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"fc_1\",\"output\":\"file contents\"}}\n",
            "{\"timestamp\":\"2026-08-03T10:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"local_shell_call\",\"call_id\":\"lsc_1\",\"action\":{\"type\":\"exec\",\"command\":[\"ls\",\"-l\"]}}}\n",
            "{\"timestamp\":\"2026-08-03T10:00:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"local_shell_call_output\",\"id\":\"lsc_1\",\"output\":\"total 0\\n\"}}\n",
        );
        let events = load_jsonl_events(content);
        let traj = convert_codex_events(&events, "codex").unwrap();

        assert_eq!(traj.steps.len(), 1);
        let tcs = traj.steps[0].tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0].function_name, "read_file");
        assert_eq!(tcs[0].arguments["path"], "/tmp/a");
        assert_eq!(tcs[1].function_name, "shell");
        assert_eq!(tcs[1].tool_call_id, "lsc_1");
        assert_eq!(tcs[1].arguments["command"][0], "ls");
        let obs = traj.steps[0].observation.as_ref().unwrap();
        assert_eq!(obs.results.len(), 2);
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("fc_1"));
        assert_eq!(
            obs.results[0].content,
            Some(serde_json::Value::String("file contents".into()))
        );
        // Shell output resolves its source id from `id` when `call_id` is absent.
        assert_eq!(obs.results[1].source_call_id.as_deref(), Some("lsc_1"));
        assert_eq!(
            obs.results[1].content,
            Some(serde_json::Value::String("total 0".into()))
        );
    }

    #[test]
    fn test_extract_private_metadata() {
        let events = fixture_events();
        let extra = extract_private_metadata(&events, "(default)");
        assert_eq!(extra["cwd"], "/Users/u/vscode/sysom-dev");
        assert_eq!(extra["project"], "sysom-dev");
        assert_eq!(extra["user_message_count"], 1);
        assert_eq!(extra["assistant_message_count"], 1);
    }
}
