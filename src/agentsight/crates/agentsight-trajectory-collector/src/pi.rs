//! Pi coding agent JSONL → ATIF v1.7 converter.
//!
//! Pi uses a similar structure to Qoder but with different field names:
//!   - Events have `"type": "message"` with `message.role` = `user`/`assistant`/`toolResult`
//!   - Metadata events: `"type": "session"`, `"type": "model_change"`
//!   - Assistant content blocks: `thinking`, `text`, `toolCall` (not `tool_use`)
//!   - Tool results are separate events with `role: "toolResult"` (not embedded in user messages)
//!   - Usage fields: `input`/`output`/`cacheRead` (not `input_tokens`/`output_tokens`)

use std::collections::HashMap;

use anyhow::Result;

use agentsight_atif::{
    Agent, AtifTrajectory, FinalMetrics, Metrics, Observation, ObservationResult, Step, StepSource,
    ToolCall, ATIF_SCHEMA_VERSION,
};

/// Event types to skip entirely.
const SKIP_TYPES: &[&str] = &["model_change", "thinking_level_change"];

/// Detect whether a set of raw events came from Pi (as opposed to Qoder/Claude).
///
/// Heuristic: the first event has `"type": "session"` with a `"version"` integer field,
/// OR any message event uses `role: "toolResult"` or content type `"toolCall"`.
pub fn is_pi_format(events: &[serde_json::Value]) -> bool {
    if let Some(first) = events.first() {
        let t = first.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "session" && first.get("version").and_then(|v| v.as_u64()).is_some() {
            return true;
        }
    }
    // Check first few events for pi-specific markers
    events.iter().take(10).any(|e| {
        let role = e
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if role == "toolResult" {
            return true;
        }
        // Check for toolCall content block type
        if let Some(content) = e
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            return content
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("toolCall"));
        }
        false
    })
}

/// Convert raw Pi JSONL events into an ATIF v1.7 trajectory.
pub fn convert_pi_events(events: &[serde_json::Value], agent_name: &str) -> Result<AtifTrajectory> {
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
        if t == "session" || SKIP_TYPES.contains(&t) {
            i += 1;
            continue;
        }

        if t != "message" {
            i += 1;
            continue;
        }

        let msg = match e.get("message") {
            Some(m) => m,
            None => {
                i += 1;
                continue;
            }
        };
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

        // --- User message ---
        if role == "user" {
            step_id += 1;
            let text = extract_text_from_content(msg);
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
        if role == "assistant" {
            step_id += 1;
            let mut reasoning_parts: Vec<String> = Vec::new();
            let mut message_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();

            let step_timestamp = e
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(String::from);
            let step_model: Option<String> = None;

            // Extract usage/metrics
            let step_metrics = extract_usage(msg);

            // Process content blocks
            if let Some(blocks) = msg.get("content").and_then(|v| v.as_array()) {
                for block in blocks {
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
                        "toolCall" => {
                            let mut tool_input: serde_json::Value = block
                                .get("arguments")
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
                    Some(tool_calls)
                },
                observation: None,
                metrics: step_metrics,
                extra: None,
                llm_call_count: None,
                is_copied_context: None,
            });

            // Collect following toolResult events as observation
            let mut j = i + 1;
            let mut obs_results: Vec<ObservationResult> = Vec::new();
            let mut result_timestamps: HashMap<String, String> = HashMap::new();
            while j < events.len() {
                let ne = &events[j];
                let nt = ne.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if nt != "message" {
                    j += 1;
                    continue;
                }
                let nm = match ne.get("message") {
                    Some(m) => m,
                    None => {
                        j += 1;
                        continue;
                    }
                };
                let nr = nm.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if nr != "toolResult" {
                    break;
                }

                let tool_call_id = nm
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let is_error = nm.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
                let content_str = extract_text_from_content(nm);

                let extra = if is_error {
                    let mut m = HashMap::new();
                    m.insert("is_error".into(), serde_json::Value::Bool(true));
                    Some(m)
                } else {
                    None
                };

                obs_results.push(ObservationResult {
                    source_call_id: Some(tool_call_id.clone()),
                    content: Some(serde_json::Value::String(content_str)),
                    subagent_trajectory_ref: None,
                    extra,
                });

                if let Some(ts) = ne.get("timestamp").and_then(|v| v.as_str()) {
                    result_timestamps.insert(tool_call_id, ts.to_string());
                }

                j += 1;
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

            i = j;
            continue;
        }

        // toolResult without preceding assistant (orphan) — skip
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

/// Pi-private session info destined for the ATIF `extra` field.
pub fn extract_private_metadata(
    events: &[serde_json::Value],
    project: &str,
) -> HashMap<String, serde_json::Value> {
    let mut cwd: Option<String> = None;
    let mut user_count: u64 = 0;
    let mut assistant_count: u64 = 0;

    for e in events {
        let t = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "session" && cwd.is_none() {
            cwd = e.get("cwd").and_then(|v| v.as_str()).map(String::from);
        }
        if t == "message" {
            let role = e
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match role {
                "user" => user_count += 1,
                "assistant" => assistant_count += 1,
                _ => {}
            }
        }
    }

    let mut map = HashMap::new();
    if let Some(c) = cwd {
        map.insert("cwd".into(), serde_json::Value::String(c));
    }
    map.insert(
        "user_message_count".into(),
        serde_json::Value::Number(user_count.into()),
    );
    map.insert(
        "assistant_message_count".into(),
        serde_json::Value::Number(assistant_count.into()),
    );
    map.insert(
        "project".into(),
        serde_json::Value::String(project.to_string()),
    );
    map
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

        if t == "session" {
            if session_id.is_none() {
                session_id = e.get("id").and_then(|v| v.as_str()).map(String::from);
            }
            if version.is_none() {
                version = e
                    .get("version")
                    .and_then(|v| v.as_u64())
                    .map(|v| v.to_string());
            }
        }
        if t == "model_change" && model_name.is_none() {
            model_name = e.get("modelId").and_then(|v| v.as_str()).map(String::from);
        }
    }

    (session_id, model_name, version)
}

/// Extract plain text from a message's `content` field.
fn extract_text_from_content(msg: &serde_json::Value) -> String {
    let content = msg.get("content");
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(blocks)) => {
            let mut parts: Vec<&str> = Vec::new();
            for block in blocks {
                if let Some(obj) = block.as_object() {
                    let bt = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if bt == "text" {
                        if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                            parts.push(t);
                        }
                    }
                }
            }
            parts.join("\n")
        }
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Extract usage metrics from a pi assistant message.
///
/// Pi usage format: `{ input, output, cacheRead, cacheWrite, totalTokens, cost }`
fn extract_usage(msg: &serde_json::Value) -> Option<Metrics> {
    let usage = msg.get("usage")?;
    let input = usage.get("input").and_then(|v| v.as_u64());
    let output = usage.get("output").and_then(|v| v.as_u64());
    let cache_read = usage.get("cacheRead").and_then(|v| v.as_u64()).unwrap_or(0);

    if input.is_none() && output.is_none() {
        return None;
    }

    // Pi reports `input` as the non-cached portion; add cacheRead for
    // consistent prompt_tokens (same normalization as Qoder/Claude).
    let prompt = input.map(|v| v + cache_read);
    let cached = if cache_read > 0 {
        Some(cache_read)
    } else {
        None
    };

    Some(Metrics {
        prompt_tokens: prompt,
        completion_tokens: output,
        cached_tokens: cached,
        cost_usd: None,
        logprobs: None,
        completion_token_ids: None,
        prompt_token_ids: None,
        extra: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qoder::load_jsonl_events;

    fn fixture_events() -> Vec<serde_json::Value> {
        let content = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"abc-123\",\"timestamp\":\"2026-08-21T10:00:00Z\",\"cwd\":\"/data/myapp\"}\n",
            "{\"type\":\"model_change\",\"id\":\"m1\",\"parentId\":null,\"timestamp\":\"2026-08-21T10:00:00Z\",\"provider\":\"dashscope\",\"modelId\":\"glm-5.2\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":\"m1\",\"timestamp\":\"2026-08-21T10:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"list files\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"timestamp\":\"2026-08-21T10:00:02Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"need ls\"},{\"type\":\"text\",\"text\":\"Let me check.\"},{\"type\":\"toolCall\",\"id\":\"t1\",\"name\":\"bash\",\"arguments\":{\"command\":\"ls\"}}],\"usage\":{\"input\":100,\"output\":20,\"cacheRead\":50,\"cacheWrite\":0,\"totalTokens\":170}}}\n",
            "{\"type\":\"message\",\"id\":\"tr1\",\"parentId\":\"a1\",\"timestamp\":\"2026-08-21T10:00:03Z\",\"message\":{\"role\":\"toolResult\",\"toolCallId\":\"t1\",\"toolName\":\"bash\",\"content\":[{\"type\":\"text\",\"text\":\"a.txt\\nb.txt\"}],\"isError\":false}}\n",
            "{\"type\":\"message\",\"id\":\"a2\",\"parentId\":\"tr1\",\"timestamp\":\"2026-08-21T10:00:05Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Found a.txt and b.txt.\"}],\"usage\":{\"input\":150,\"output\":10,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":160}}}\n",
        );
        load_jsonl_events(content)
    }

    #[test]
    fn test_is_pi_format() {
        let events = fixture_events();
        assert!(is_pi_format(&events));

        // Qoder format should not be detected as pi
        let qoder = load_jsonl_events(
            "{\"type\":\"runtime-config\",\"sessionId\":\"x\",\"model\":\"m\"}\n",
        );
        assert!(!is_pi_format(&qoder));
    }

    #[test]
    fn test_convert_basic_flow() {
        let events = fixture_events();
        let traj = convert_pi_events(&events, "pi").unwrap();

        assert_eq!(traj.schema_version, ATIF_SCHEMA_VERSION);
        assert_eq!(traj.session_id.as_deref(), Some("abc-123"));
        assert_eq!(traj.agent.name, "pi");
        assert_eq!(traj.agent.model_name.as_deref(), Some("glm-5.2"));
        traj.validate_step_ids().unwrap();

        // user + assistant(tool) + assistant(final)
        assert_eq!(traj.steps.len(), 3);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "list files");

        let tool_step = &traj.steps[1];
        assert_eq!(tool_step.source, StepSource::Agent);
        assert_eq!(tool_step.reasoning_content.as_deref(), Some("need ls"));
        assert_eq!(tool_step.message, "Let me check.");
        let tcs = tool_step.tool_calls.as_ref().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function_name, "bash");
        assert_eq!(tcs[0].tool_call_id, "t1");
        // toolResult attached as observation
        let obs = tool_step.observation.as_ref().unwrap();
        assert_eq!(obs.results.len(), 1);
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("t1"));

        let final_step = &traj.steps[2];
        assert_eq!(final_step.message, "Found a.txt and b.txt.");

        // Token totals: (100+50) + 150 = 300 prompt, 20+10 = 30 completion
        let fm = traj.final_metrics.as_ref().unwrap();
        assert_eq!(fm.total_prompt_tokens, Some(300));
        assert_eq!(fm.total_completion_tokens, Some(30));
        assert_eq!(fm.total_cached_tokens, Some(50));
    }

    #[test]
    fn test_convert_error_tool_result() {
        let content = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"s1\",\"timestamp\":\"2026-08-21T10:00:00Z\",\"cwd\":\"/tmp\"}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":null,\"timestamp\":\"2026-08-21T10:00:01Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"id\":\"t9\",\"name\":\"bash\",\"arguments\":{\"command\":\"fail\"}}],\"usage\":{\"input\":10,\"output\":5,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":15}}}\n",
            "{\"type\":\"message\",\"id\":\"tr1\",\"parentId\":\"a1\",\"timestamp\":\"2026-08-21T10:00:02Z\",\"message\":{\"role\":\"toolResult\",\"toolCallId\":\"t9\",\"toolName\":\"bash\",\"content\":[{\"type\":\"text\",\"text\":\"boom\"}],\"isError\":true}}\n",
        );
        let events = load_jsonl_events(content);
        let traj = convert_pi_events(&events, "pi").unwrap();
        assert_eq!(traj.steps.len(), 1);
        let obs = traj.steps[0].observation.as_ref().unwrap();
        let extra = obs.results[0].extra.as_ref().unwrap();
        assert_eq!(extra["is_error"], serde_json::Value::Bool(true));
    }

    #[test]
    fn test_extract_private_metadata() {
        let events = fixture_events();
        let meta = extract_private_metadata(&events, "myapp");
        assert_eq!(meta["cwd"], serde_json::json!("/data/myapp"));
        assert_eq!(meta["user_message_count"], serde_json::json!(1));
        assert_eq!(meta["assistant_message_count"], serde_json::json!(2));
        assert_eq!(meta["project"], serde_json::json!("myapp"));
    }
}
