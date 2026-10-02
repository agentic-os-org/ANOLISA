//! Qoder/QoderWork JSONL → ATIF v1.7 converter.
//!
//! Ported from AgentOpt's `converters/claude_code.rs` (Claude Code, QoderWork
//! and Qoder share the same JSONL structure):
//!   - Events have a `type` field: `user`, `assistant`, `runtime-config`,
//!     `session_meta`, `progress`, `last-prompt`
//!   - Assistant messages have content blocks: `thinking`, `text`, `tool_use`
//!   - User messages may have content blocks: `text`, `tool_result`

use std::collections::HashMap;

use anyhow::Result;

use agentsight_atif::{
    Agent, AtifTrajectory, FinalMetrics, Metrics, Observation, ObservationResult, Step, StepSource,
    ToolCall, ATIF_SCHEMA_VERSION, EXTRA_IS_ERROR,
};

/// Event types to skip entirely.
const SKIP_TYPES: &[&str] = &["runtime-config", "session_meta", "progress", "last-prompt"];

/// Convert raw Qoder JSONL events into an ATIF v1.7 trajectory.
pub fn convert_qoder_events(
    events: &[serde_json::Value],
    agent_name: &str,
) -> Result<AtifTrajectory> {
    let (session_id, model_name, version) = extract_agent_info(events);

    let agent = Agent {
        name: agent_name.to_string(),
        version: version.unwrap_or_else(|| "unknown".into()),
        model_name,
        tool_definitions: None,
        extra: None,
    };

    let mut steps: Vec<Step> = Vec::new();
    let mut step_id: usize = 0;
    let mut total_prompt_tokens: u64 = 0;
    let mut total_completion_tokens: u64 = 0;
    let mut total_cached_tokens: u64 = 0;

    let mut i: usize = 0;
    while i < events.len() {
        let e = &events[i];
        let t = e.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Skip metadata events
        if SKIP_TYPES.contains(&t) {
            i += 1;
            continue;
        }

        // --- User message ---
        if t == "user" {
            let content = e
                .get("message")
                .and_then(|m| m.get("content"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let tool_results = extract_tool_results(&content);

            // If this user event only has tool_results, attach to previous step
            if !tool_results.is_empty() && !has_text_block(&content) {
                if let Some(prev) = steps.last_mut() {
                    let result_ts = e.get("timestamp").and_then(|v| v.as_str());
                    let mut obs_results = Vec::new();
                    for tr in &tool_results {
                        let extra = if tr.is_error {
                            let mut m = HashMap::new();
                            m.insert(EXTRA_IS_ERROR.into(), serde_json::Value::Bool(true));
                            Some(m)
                        } else {
                            None
                        };
                        obs_results.push(ObservationResult {
                            source_call_id: Some(tr.tool_use_id.clone()),
                            content: Some(serde_json::Value::String(tr.content.clone())),
                            subagent_trajectory_ref: None,
                            extra,
                        });
                        // Write result_timestamp into matching ToolCall.extra
                        if let (Some(ts), Some(tcs)) = (result_ts, prev.tool_calls.as_mut()) {
                            for tc in tcs.iter_mut() {
                                if tc.tool_call_id == tr.tool_use_id {
                                    let mut extra = tc.extra.take().unwrap_or_default();
                                    extra.insert(
                                        "result_timestamp".into(),
                                        serde_json::Value::String(ts.to_string()),
                                    );
                                    tc.extra = Some(extra);
                                }
                            }
                        }
                    }
                    if !obs_results.is_empty() {
                        prev.observation = Some(Observation {
                            results: obs_results,
                        });
                    }
                }
                i += 1;
                continue;
            }

            // Regular user message
            step_id += 1;
            let text = extract_text_from_content(&content);
            steps.push(Step {
                step_id,
                timestamp: e
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                source: StepSource::User,
                message: text,
                model_name: None,
                reasoning_effort: None,
                reasoning_content: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                extra: None,
                llm_call_count: None,
                is_copied_context: None,
            });
            i += 1;
            continue;
        }

        // --- Assistant message ---
        if t == "assistant" {
            // Collect all consecutive assistant events (same LLM turn)
            let mut turn_events: Vec<&serde_json::Value> = vec![e];
            let mut j = i + 1;
            while j < events.len() {
                let ne = &events[j];
                let nt = ne.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if nt == "assistant" {
                    turn_events.push(ne);
                    j += 1;
                } else if SKIP_TYPES.contains(&nt) {
                    j += 1;
                } else {
                    break;
                }
            }

            // Merge turn events into a single step
            step_id += 1;
            let mut reasoning_parts: Vec<String> = Vec::new();
            let mut message_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let step_timestamp = turn_events[0]
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut step_model: Option<String> = None;
            let mut step_metrics: Option<Metrics> = None;

            for te in &turn_events {
                let msg = match te.get("message") {
                    Some(m) => m,
                    None => continue,
                };
                if step_model.is_none() {
                    step_model = msg.get("model").and_then(|v| v.as_str()).map(String::from);
                }

                // Extract usage/metrics — accumulate across multiple usage
                // events within the same LLM turn (some providers emit partial
                // usage per chunk); only keeping the last would under-count.
                if let Some(usage) = msg.get("usage") {
                    let pt = usage.get("input_tokens").and_then(|v| v.as_u64());
                    let ct = usage.get("output_tokens").and_then(|v| v.as_u64());
                    let cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_creation = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    // Claude-style usage reports `input_tokens` EXCLUDING the
                    // cache portions; normalize to the OpenAI convention
                    // (prompt includes cached) so all sources display alike.
                    let pt = pt.map(|v| v + cache_read + cache_creation);
                    let cache = (cache_read > 0).then_some(cache_read);
                    if pt.is_some() || ct.is_some() {
                        let m = step_metrics.get_or_insert_with(|| Metrics {
                            prompt_tokens: Some(0),
                            completion_tokens: Some(0),
                            cached_tokens: Some(0),
                            cost_usd: None,
                            logprobs: None,
                            completion_token_ids: None,
                            prompt_token_ids: None,
                            extra: None,
                        });
                        if let Some(v) = pt {
                            *m.prompt_tokens.get_or_insert(0) += v;
                        }
                        if let Some(v) = ct {
                            *m.completion_tokens.get_or_insert(0) += v;
                        }
                        if let Some(v) = cache {
                            *m.cached_tokens.get_or_insert(0) += v;
                        }
                    }
                }

                // Process content blocks
                let content_blocks = match msg.get("content").and_then(|v| v.as_array()) {
                    Some(arr) => arr,
                    None => continue,
                };
                for block in content_blocks {
                    let bt = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match bt {
                        "thinking" => {
                            let thinking_text = block
                                .get("thinking")
                                .or_else(|| block.get("text"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if !thinking_text.is_empty() {
                                reasoning_parts.push(thinking_text.to_string());
                            }
                        }
                        "text" => {
                            if let Some(text_val) = block.get("text").and_then(|v| v.as_str()) {
                                if !text_val.is_empty() {
                                    message_parts.push(text_val.to_string());
                                }
                            }
                        }
                        "tool_use" => {
                            let mut tool_input: serde_json::Value = block
                                .get("input")
                                .cloned()
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            if let Some(s) = tool_input.as_str() {
                                tool_input = serde_json::from_str(s)
                                    .unwrap_or_else(|_| serde_json::json!({"raw": s}));
                            }
                            if !tool_input.is_object() {
                                tool_input = serde_json::json!({"value": tool_input});
                            }
                            tool_calls.push(ToolCall {
                                tool_call_id: block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                function_name: block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                arguments: tool_input,
                                extra: None,
                            });
                        }
                        _ => {}
                    }
                }
            }

            let reasoning = if reasoning_parts.is_empty() {
                None
            } else {
                Some(reasoning_parts.join("\n"))
            };
            let message = message_parts.join("\n");

            if let Some(ref m) = step_metrics {
                total_prompt_tokens += m.prompt_tokens.unwrap_or(0);
                total_completion_tokens += m.completion_tokens.unwrap_or(0);
                total_cached_tokens += m.cached_tokens.unwrap_or(0);
            }

            steps.push(Step {
                step_id,
                timestamp: step_timestamp,
                source: StepSource::Agent,
                model_name: step_model,
                message,
                reasoning_effort: None,
                reasoning_content: reasoning,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls.clone())
                },
                observation: None,
                metrics: step_metrics,
                extra: None,
                llm_call_count: None,
                is_copied_context: None,
            });

            // Collect tool_results that follow
            let mut k = j;
            let mut obs_results: Vec<ObservationResult> = Vec::new();
            let mut result_timestamps: HashMap<String, String> = HashMap::new();
            while k < events.len() {
                let ne = &events[k];
                let nt = ne.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if SKIP_TYPES.contains(&nt) {
                    k += 1;
                    continue;
                }
                if nt == "user" {
                    let nc = ne
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let trs = extract_tool_results(&nc);
                    if !trs.is_empty() {
                        let result_ts = ne.get("timestamp").and_then(|v| v.as_str());
                        for tr in &trs {
                            let extra = if tr.is_error {
                                let mut m = HashMap::new();
                                m.insert(EXTRA_IS_ERROR.into(), serde_json::Value::Bool(true));
                                Some(m)
                            } else {
                                None
                            };
                            obs_results.push(ObservationResult {
                                source_call_id: Some(tr.tool_use_id.clone()),
                                content: Some(serde_json::Value::String(tr.content.clone())),
                                subagent_trajectory_ref: None,
                                extra,
                            });
                            if let Some(ts) = result_ts {
                                result_timestamps.insert(tr.tool_use_id.clone(), ts.to_string());
                            }
                        }
                        k += 1;
                        continue;
                    }
                }
                break;
            }

            // Write result_timestamp into ToolCall.extra
            if !result_timestamps.is_empty() {
                if let Some(last_step) = steps.last_mut() {
                    if let Some(tcs) = last_step.tool_calls.as_mut() {
                        for tc in tcs.iter_mut() {
                            if let Some(ts) = result_timestamps.get(&tc.tool_call_id) {
                                let mut extra = tc.extra.take().unwrap_or_default();
                                extra.insert(
                                    "result_timestamp".into(),
                                    serde_json::Value::String(ts.clone()),
                                );
                                tc.extra = Some(extra);
                            }
                        }
                    }
                }
            }

            if !obs_results.is_empty() {
                if let Some(last_step) = steps.last_mut() {
                    last_step.observation = Some(Observation {
                        results: obs_results,
                    });
                }
            }

            i = k;
            continue;
        }

        // Unknown type, skip
        i += 1;
    }

    // Build final metrics
    let final_metrics = if total_prompt_tokens > 0 || total_completion_tokens > 0 {
        Some(FinalMetrics {
            total_prompt_tokens: Some(total_prompt_tokens),
            total_completion_tokens: Some(total_completion_tokens),
            total_cached_tokens: Some(total_cached_tokens),
            total_cost_usd: None,
            total_steps: Some(steps.len()),
            extra: None,
        })
    } else {
        None
    };

    Ok(AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.into(),
        session_id,
        agent,
        steps,
        trajectory_id: None,
        notes: None,
        final_metrics,
        continued_trajectory_ref: None,
        subagent_trajectories: None,
        extra: None,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract `(session_id, model_name, version)` from the raw events.
fn extract_agent_info(
    events: &[serde_json::Value],
) -> (Option<String>, Option<String>, Option<String>) {
    let mut session_id: Option<String> = None;
    let mut model_name: Option<String> = None;
    let mut version: Option<String> = None;

    for e in events {
        let t = e.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if t == "runtime-config" {
            if session_id.is_none() {
                session_id = e
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            if model_name.is_none() {
                model_name = e.get("model").and_then(|v| v.as_str()).map(String::from);
            }
        }
        if session_id.is_none() {
            session_id = e
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(String::from);
        }
        if version.is_none() {
            version = e.get("version").and_then(|v| v.as_str()).map(String::from);
        }
        if t == "assistant" && model_name.is_none() {
            if let Some(msg) = e.get("message") {
                model_name = msg.get("model").and_then(|v| v.as_str()).map(String::from);
            }
        }
    }

    (session_id, model_name, version)
}

/// Extract plain text from the various `content` shapes seen in JSONL events.
///
/// Handles:
///   - `String` → returned as-is
///   - `Array` of content blocks → concatenates `text`-type blocks
///   - Anything else → `""` or stringified form
fn extract_text_from_content(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => {
            let mut parts: Vec<&str> = Vec::new();
            for block in blocks {
                if let Some(obj) = block.as_object() {
                    match obj.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                                parts.push(t);
                            }
                        }
                        Some("tool_result") => {
                            let rc = obj.get("content").unwrap_or(&serde_json::Value::Null);
                            match rc {
                                serde_json::Value::String(s) => parts.push(s.as_str()),
                                serde_json::Value::Array(subs) => {
                                    for sub in subs {
                                        if let Some(sub_obj) = sub.as_object() {
                                            if sub_obj.get("type").and_then(|t| t.as_str())
                                                == Some("text")
                                            {
                                                if let Some(t) =
                                                    sub_obj.get("text").and_then(|v| v.as_str())
                                                {
                                                    parts.push(t);
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
            parts.join("\n")
        }
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A tool_result block extracted from a user-message `content` array.
#[derive(Debug, Clone)]
struct ExtractedToolResult {
    tool_use_id: String,
    content: String,
    is_error: bool,
}

/// Extract `tool_result` blocks from a user-message `content` array.
fn extract_tool_results(content: &serde_json::Value) -> Vec<ExtractedToolResult> {
    let blocks = match content.as_array() {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let mut results = Vec::new();
    for block in blocks {
        let obj = match block.as_object() {
            Some(o) => o,
            None => continue,
        };
        if obj.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }
        let tool_use_id = obj
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let rc = obj.get("content").unwrap_or(&serde_json::Value::Null);
        let content_str = match rc {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(subs) => {
                let text_parts: Vec<&str> = subs
                    .iter()
                    .filter_map(|sub| {
                        let so = sub.as_object()?;
                        if so.get("type").and_then(|t| t.as_str()) == Some("text") {
                            so.get("text").and_then(|v| v.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                text_parts.join("\n")
            }
            other => other.to_string(),
        };

        let is_error = obj
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        results.push(ExtractedToolResult {
            tool_use_id,
            content: content_str,
            is_error,
        });
    }
    results
}

/// Return `true` if a content array contains any `text`-type block.
fn has_text_block(content: &serde_json::Value) -> bool {
    if let Some(arr) = content.as_array() {
        arr.iter().any(|b| {
            b.as_object()
                .and_then(|o| o.get("type"))
                .and_then(|t| t.as_str())
                == Some("text")
        })
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qoder::load_jsonl_events;

    fn fixture_events() -> Vec<serde_json::Value> {
        let content = concat!(
            "{\"type\":\"runtime-config\",\"sessionId\":\"abc-123\",\"model\":\"qwen-max\"}\n",
            "{\"type\":\"user\",\"timestamp\":\"2026-07-25T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"list files\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-07-25T10:00:02Z\",\"message\":{\"role\":\"assistant\",\"model\":\"qwen-max\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20},\"content\":[{\"type\":\"thinking\",\"thinking\":\"need ls\"},{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"bash\",\"input\":{\"cmd\":\"ls\"}}]}}\n",
            "{\"type\":\"user\",\"timestamp\":\"2026-07-25T10:00:03Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"a.txt\"}]}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-07-25T10:00:05Z\",\"message\":{\"role\":\"assistant\",\"model\":\"qwen-max\",\"usage\":{\"input_tokens\":150,\"output_tokens\":10},\"content\":[{\"type\":\"text\",\"text\":\"Only a.txt exists.\"}]}}\n",
        );
        load_jsonl_events(content)
    }

    #[test]
    fn test_convert_basic_flow() {
        let events = fixture_events();
        let traj = convert_qoder_events(&events, "qoder").unwrap();

        assert_eq!(traj.schema_version, ATIF_SCHEMA_VERSION);
        assert_eq!(traj.session_id.as_deref(), Some("abc-123"));
        assert_eq!(traj.agent.name, "qoder");
        assert_eq!(traj.agent.model_name.as_deref(), Some("qwen-max"));
        traj.validate_step_ids().unwrap();

        // user + agent(tool) + agent(final answer)
        assert_eq!(traj.steps.len(), 3);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "list files");

        let tool_step = &traj.steps[1];
        assert_eq!(tool_step.source, StepSource::Agent);
        assert_eq!(tool_step.reasoning_content.as_deref(), Some("need ls"));
        let tcs = tool_step.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function_name, "bash");
        // tool_result attached as observation on the tool step
        let obs = tool_step.observation.as_ref().unwrap();
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("t1"));

        assert_eq!(traj.steps[2].message, "Only a.txt exists.");

        // Token totals aggregated into final_metrics
        let fm = traj.final_metrics.as_ref().unwrap();
        assert_eq!(fm.total_prompt_tokens, Some(250));
        assert_eq!(fm.total_completion_tokens, Some(30));
    }

    #[test]
    fn test_convert_error_tool_result_marks_extra() {
        let content = concat!(
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t9\",\"name\":\"bash\",\"input\":{}}]}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t9\",\"content\":\"boom\",\"is_error\":true}]}}\n",
        );
        let events = load_jsonl_events(content);
        let traj = convert_qoder_events(&events, "qoder").unwrap();
        let obs = traj.steps[0].observation.as_ref().unwrap();
        let extra = obs.results[0].extra.as_ref().unwrap();
        assert_eq!(extra["is_error"], serde_json::Value::Bool(true));
    }

    #[test]
    fn test_convert_normalizes_claude_cached_prompt_tokens() {
        // Claude usage excludes cache tokens from input_tokens; the converter
        // must fold them back so prompt >= cached always holds in the UI.
        let content = concat!(
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude\",",
            "\"usage\":{\"input_tokens\":3,\"cache_creation_input_tokens\":7000,\"cache_read_input_tokens\":19000,\"output_tokens\":452},",
            "\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
        );
        let events = load_jsonl_events(content);
        let traj = convert_qoder_events(&events, "claude-code").unwrap();
        let m = traj.steps[0].metrics.as_ref().unwrap();
        assert_eq!(m.prompt_tokens, Some(3 + 7000 + 19000));
        assert_eq!(m.completion_tokens, Some(452));
        assert_eq!(m.cached_tokens, Some(19000));
    }

    #[test]
    fn test_convert_empty_events_yields_empty_steps() {
        let traj = convert_qoder_events(&[], "qoder").unwrap();
        assert!(traj.steps.is_empty());
        assert!(traj.final_metrics.is_none());
    }
}
