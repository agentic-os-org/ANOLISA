//! JSONL → ATIF converter
//!
//! Parses QoderWork/Qoder/Claude Code JSONL session files and converts them
//! to ATIF v1.7 documents for trajectory display. These agents share a common
//! JSONL format with event types: `runtime-config`, `user`, `assistant`.
//! Codex rollouts use a different envelope schema and are delegated to the
//! collector crate's Codex converter.
//!
//! Content blocks within messages:
//! - `assistant` content: `thinking`, `text`, `tool_use`
//! - `user` content: `text` (human input), `tool_result` (tool output)

use agentsight_atif::{
    ATIF_SCHEMA_VERSION, Agent, AtifTrajectory, EXTRA_IS_ERROR, FinalMetrics, Observation,
    ObservationResult, Step, StepSource, ToolCall,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub fn convert_jsonl_to_atif(path: &Path) -> anyhow::Result<AtifTrajectory> {
    // Lossy decode: a session file torn mid-write by a killed agent can end
    // in the middle of a multi-byte character, and a strict `read_to_string`
    // would fail the whole conversion, dropping every complete record before
    // the tail. The torn tail becomes a malformed line the parser skips.
    let content = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())?;

    convert_jsonl_content_to_atif(&content)
}

pub fn convert_jsonl_content_to_atif(content: &str) -> anyhow::Result<AtifTrajectory> {
    // Codex rollouts use a different envelope schema
    // (`{"timestamp","type","payload"}` records). The collector crate already
    // ships a converter for them, so delegate instead of emitting an empty
    // trajectory. `response_item`/`turn_context` only occur in Codex files,
    // which keeps the extra parse off the Claude/Qoder path.
    if content.contains("response_item") || content.contains("turn_context") {
        let events = agentsight_trajectory_collector::qoder::load_jsonl_events(content);
        if agentsight_trajectory_collector::codex::is_codex_rollout(&events) {
            return agentsight_trajectory_collector::codex::convert_codex_events(&events, "codex");
        }
    }

    let mut session_id = String::new();
    let mut model_name: Option<String> = None;
    let mut agent_version = String::new();
    let mut agent_name = String::new();
    let mut steps: Vec<Step> = Vec::new();
    let mut step_id: usize = 0;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "runtime-config" => {
                if session_id.is_empty() {
                    session_id = event
                        .get("sessionId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                if model_name.is_none() {
                    model_name = event
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                if agent_version.is_empty() {
                    agent_version = event
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                if agent_name.is_empty() {
                    agent_name = event
                        .get("entrypoint")
                        .and_then(|v| v.as_str())
                        .unwrap_or("agent")
                        .to_string();
                }
            }
            "session_meta" | "progress" | "last-prompt" => continue,
            "user" => {
                let timestamp = event
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let content_arr = event.pointer("/message/content").and_then(|c| c.as_array());

                if let Some(blocks) = content_arr {
                    // One user message can carry tool results and text at the
                    // same time (typing while a tool call is pending). Both
                    // belong to the trajectory: the results as observations on
                    // the agent step, the text as its own user step.
                    if blocks
                        .iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                    {
                        append_tool_results(&mut steps, blocks);
                    }

                    let mut message_text = String::new();
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            if !text.is_empty() {
                                if !message_text.is_empty() {
                                    message_text.push('\n');
                                }
                                message_text.push_str(text);
                            }
                        }
                    }
                    if !message_text.is_empty() {
                        step_id += 1;
                        steps.push(Step {
                            step_id,
                            timestamp,
                            source: StepSource::User,
                            message: message_text,
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
                    }
                } else if let Some(content_str) =
                    event.pointer("/message/content").and_then(|c| c.as_str())
                {
                    step_id += 1;
                    steps.push(Step {
                        step_id,
                        timestamp,
                        source: StepSource::User,
                        message: content_str.to_string(),
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
                }
            }
            "assistant" => {
                let timestamp = event
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let step_model = event
                    .pointer("/message/model")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| model_name.clone());

                let content_arr = event.pointer("/message/content").and_then(|c| c.as_array());

                let mut message_text = String::new();
                let mut reasoning = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();

                if let Some(blocks) = content_arr {
                    for block in blocks {
                        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match block_type {
                            "thinking" => {
                                let text =
                                    block.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
                                if !text.is_empty() {
                                    if !reasoning.is_empty() {
                                        reasoning.push('\n');
                                    }
                                    reasoning.push_str(text);
                                }
                            }
                            "text" => {
                                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                if !text.is_empty() {
                                    if !message_text.is_empty() {
                                        message_text.push('\n');
                                    }
                                    message_text.push_str(text);
                                }
                            }
                            "tool_use" => {
                                let id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let input = block.get("input").cloned().unwrap_or(Value::Null);
                                tool_calls.push(ToolCall {
                                    tool_call_id: id,
                                    function_name: name,
                                    arguments: input,
                                    extra: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }

                step_id += 1;
                steps.push(Step {
                    step_id,
                    timestamp,
                    source: StepSource::Agent,
                    message: message_text,
                    model_name: step_model,
                    reasoning_effort: None,
                    reasoning_content: if reasoning.is_empty() {
                        None
                    } else {
                        Some(reasoning)
                    },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    observation: None,
                    metrics: None,
                    extra: None,
                    llm_call_count: None,
                    is_copied_context: None,
                });
            }
            _ => {}
        }
    }

    let total_steps = steps.len();

    let agent = Agent {
        name: if agent_name.is_empty() {
            "unknown".to_string()
        } else {
            agent_name
        },
        version: if agent_version.is_empty() {
            "0".to_string()
        } else {
            agent_version
        },
        model_name,
        tool_definitions: None,
        extra: None,
    };

    let final_metrics = FinalMetrics {
        total_prompt_tokens: None,
        total_completion_tokens: None,
        total_cached_tokens: None,
        total_cost_usd: None,
        total_steps: Some(total_steps),
        extra: None,
    };

    Ok(AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.to_string(),
        session_id: Some(if session_id.is_empty() {
            "unknown".to_string()
        } else {
            session_id
        }),
        agent,
        steps,
        trajectory_id: None,
        notes: None,
        final_metrics: Some(final_metrics),
        continued_trajectory_ref: None,
        subagent_trajectories: None,
        extra: None,
    })
}

/// Flatten a `tool_result.content` payload into result text.
///
/// Claude Code/Qoder emit `content` as an array of `{"type":"text","text":…}`
/// blocks; mirror `agentsight-trajectory-collector`'s ATIF converter so the
/// local viewer keeps the same tool output the canonical collector records.
fn flatten_tool_result_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let text_parts: Vec<&str> = blocks
                .iter()
                .filter_map(|block| {
                    let obj = block.as_object()?;
                    if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                        obj.get("text").and_then(|v| v.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            text_parts.join("\n")
        }
        other => other.to_string(),
    }
}

fn append_tool_results(steps: &mut [Step], blocks: &[Value]) {
    let last_agent_step = steps
        .iter_mut()
        .rev()
        .find(|s| s.source == StepSource::Agent);
    let step = match last_agent_step {
        Some(s) => s,
        None => return,
    };

    let observation = step.observation.get_or_insert(Observation {
        results: Vec::new(),
    });

    for block in blocks {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if block_type != "tool_result" {
            continue;
        }
        let source_call_id = block
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let content = Some(Value::String(
            block
                .get("content")
                .map(flatten_tool_result_content)
                .unwrap_or_default(),
        ));
        let extra = if block
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            Some(HashMap::from([(
                EXTRA_IS_ERROR.to_string(),
                Value::Bool(true),
            )]))
        } else {
            None
        };
        observation.results.push(ObservationResult {
            source_call_id,
            content,
            subagent_trajectory_ref: None,
            extra,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentsight_atif::StepSource;

    fn rt_cfg(session_id: &str, model: &str, version: &str, entry: &str) -> String {
        serde_json::json!({
            "type": "runtime-config",
            "sessionId": session_id,
            "model": model,
            "version": version,
            "entrypoint": entry
        })
        .to_string()
    }

    #[test]
    fn test_runtime_config_extraction() {
        let content = format!(
            "{}\n{}\n",
            rt_cfg("sess-1", "gpt-4o", "1.2.3", "my-agent"),
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"hello"}]}}"#
        );
        let traj = convert_jsonl_content_to_atif(&content).unwrap();
        assert_eq!(traj.session_id.as_deref(), Some("sess-1"));
        assert_eq!(traj.agent.name, "my-agent");
        assert_eq!(traj.agent.version, "1.2.3");
        assert_eq!(traj.agent.model_name.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn test_user_message_text_array() {
        let content = r#"{"type":"user","message":{"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 1);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "first\nsecond");
    }

    #[test]
    fn test_user_message_text_string() {
        let content = r#"{"type":"user","message":{"content":"hello world"}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 1);
        assert_eq!(traj.steps[0].message, "hello world");
    }

    #[test]
    fn test_assistant_message_with_thinking_text_tool_use() {
        let content = r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","message":{"model":"claude-4","content":[{"type":"thinking","thinking":"reasoning here"},{"type":"text","text":"answer"},{"type":"tool_use","id":"call-1","name":"bash","input":{"cmd":"ls"}}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 1);
        let step = &traj.steps[0];
        assert_eq!(step.source, StepSource::Agent);
        assert_eq!(step.message, "answer");
        assert_eq!(step.reasoning_content.as_deref(), Some("reasoning here"));
        assert_eq!(step.model_name.as_deref(), Some("claude-4"));
        let calls = step.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_call_id, "call-1");
        assert_eq!(calls[0].function_name, "bash");
    }

    #[test]
    fn test_tool_result_appended_to_agent_step() {
        let content = r#"{"type":"assistant","message":{"model":"m","content":[{"type":"tool_use","id":"tc1","name":"ls","input":null}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tc1","content":"file.txt"}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 1);
        let obs = traj.steps[0].observation.as_ref().unwrap();
        assert_eq!(obs.results.len(), 1);
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("tc1"));
    }

    #[test]
    fn test_tool_result_array_content_and_error_flag() {
        // Claude Code/Qoder write `tool_result.content` as an array of text
        // blocks and mark failures with `is_error`. Both used to be dropped,
        // so tool output disappeared from collected trajectories and
        // downstream failure detection saw no error.
        let content = r#"{"type":"assistant","message":{"model":"m","content":[{"type":"tool_use","id":"tc1","name":"bash","input":{"cmd":"false"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tc1","is_error":true,"content":[{"type":"text","text":"command failed"},{"type":"text","text":"exit 1"}]}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        let obs = traj.steps[0].observation.as_ref().unwrap();
        assert_eq!(obs.results.len(), 1);
        assert_eq!(
            obs.results[0].content,
            Some(Value::String("command failed\nexit 1".to_string())),
            "text blocks must be flattened into the result content"
        );
        assert_eq!(
            obs.results[0]
                .extra
                .as_ref()
                .and_then(|extra| extra.get(agentsight_atif::EXTRA_IS_ERROR)),
            Some(&Value::Bool(true)),
            "is_error must be carried in the result extra"
        );
    }

    #[test]
    fn test_tool_result_without_prior_agent_step() {
        let content = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tc1","content":"result"}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 0);
    }

    #[test]
    fn test_user_text_beside_tool_result_is_kept() {
        // A user can type while a tool call is pending; Claude Code then
        // records one message holding both the tool results and the text.
        let content = r#"{"type":"assistant","message":{"model":"m","content":[{"type":"tool_use","id":"tc1","name":"ls","input":null}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tc1","content":"file.txt"},{"type":"text","text":"keep going with plan B"}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();

        let user_steps: Vec<_> = traj
            .steps
            .iter()
            .filter(|s| s.source == StepSource::User)
            .collect();
        assert_eq!(user_steps.len(), 1);
        assert_eq!(user_steps[0].message, "keep going with plan B");

        let obs = traj.steps[0].observation.as_ref().unwrap();
        assert_eq!(obs.results.len(), 1);
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("tc1"));
    }

    #[test]
    fn test_skipped_event_types() {
        let content = r#"{"type":"session_meta","foo":"bar"}
{"type":"progress","p":1}
{"type":"last-prompt","prompt":"hi"}
{"type":"unknown_type","data":{}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 0);
        assert_eq!(traj.agent.name, "unknown");
        assert_eq!(traj.agent.version, "0");
        assert_eq!(traj.session_id.as_deref(), Some("unknown"));
    }

    #[test]
    fn test_invalid_json_lines_skipped() {
        let content = "not json\n\n{}\n";
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 0);
    }

    #[test]
    fn test_empty_content() {
        let traj = convert_jsonl_content_to_atif("").unwrap();
        assert_eq!(traj.steps.len(), 0);
        assert_eq!(traj.agent.name, "unknown");
    }

    #[test]
    fn test_final_metrics_total_steps() {
        let content = r#"{"type":"user","message":{"content":"hi"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"bye"}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.final_metrics.as_ref().unwrap().total_steps, Some(2));
    }

    #[test]
    fn test_assistant_no_content_blocks() {
        let content =
            r#"{"type":"assistant","message":{"model":"m","content":"plain text reply"}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 1);
        assert_eq!(traj.steps[0].message, "");
        assert!(traj.steps[0].tool_calls.is_none());
        assert!(traj.steps[0].reasoning_content.is_none());
    }

    #[test]
    fn test_user_empty_text_not_added() {
        let content = r#"{"type":"user","message":{"content":[{"type":"text","text":""}]}}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 0);
    }

    #[test]
    fn test_convert_jsonl_file() {
        let dir = std::env::temp_dir().join("agentsight_converter_test.jsonl");
        let content = r#"{"type":"user","message":{"content":"hello"}}"#;
        std::fs::write(&dir, content).unwrap();
        let traj = convert_jsonl_to_atif(&dir).unwrap();
        assert_eq!(traj.steps.len(), 1);
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn test_convert_jsonl_file_with_torn_utf8_tail() {
        let dir = std::env::temp_dir().join("agentsight_converter_torn_test.jsonl");
        let mut bytes = br#"{"type":"user","message":{"content":"hello"}}"#.to_vec();
        bytes.push(b'\n');
        // A killed agent's last write cut a multi-byte character in half.
        bytes.extend_from_slice(b"{\"type\":\"assistant\",\"message\":{\"content\":\"\xe4\xb8");
        std::fs::write(&dir, bytes).unwrap();
        let traj = convert_jsonl_to_atif(&dir).unwrap();
        assert_eq!(
            traj.steps.len(),
            1,
            "the complete record before the tail survives"
        );
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn test_runtime_config_defaults() {
        let content = r#"{"type":"runtime-config"}"#;
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.agent.name, "agent");
        assert_eq!(traj.agent.version, "0");
        assert!(traj.agent.model_name.is_none());
    }

    #[test]
    fn test_assistant_model_falls_back_to_runtime_config() {
        let content = format!(
            "{}\n{}",
            rt_cfg("s1", "gpt-4o", "1.0", "agent"),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#
        );
        let traj = convert_jsonl_content_to_atif(&content).unwrap();
        assert_eq!(traj.steps[0].model_name.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn test_codex_rollout_delegates_to_the_codex_converter() {
        // A Codex rollout is an envelope stream; the Claude-style loop would
        // return an empty trajectory with agent "unknown" for it.
        let content = concat!(
            "{\"timestamp\":\"2026-08-03T09:56:48.054Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"019fc70d\",\"cwd\":\"/Users/u/app\",\"cli_version\":\"0.146.0\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:52.360Z\",\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t-1\",\"model\":\"gpt-5.6-sol\",\"effort\":\"medium\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:52.374Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"list the files\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:58.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"exec\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:56:59.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"call_1\",\"output\":\"file-a\\n\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:57:00.153Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}}\n",
        );
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.session_id.as_deref(), Some("019fc70d"));
        assert_eq!(traj.agent.name, "codex");
        assert_eq!(traj.agent.version, "0.146.0");
        assert_eq!(traj.agent.model_name.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(traj.steps.len(), 2);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "list the files");
        let agent_step = &traj.steps[1];
        assert_eq!(agent_step.source, StepSource::Agent);
        assert_eq!(agent_step.message, "done");
        let calls = agent_step.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].function_name, "exec");
        let obs = agent_step.observation.as_ref().unwrap();
        assert_eq!(obs.results[0].source_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn test_qoder_session_meta_does_not_trigger_codex_delegation() {
        // Qoder transcripts also carry a `session_meta` event; the Claude
        // path must keep handling them (no payload envelope → not Codex).
        let content = concat!(
            "{\"type\":\"session_meta\",\"foo\":\"bar\"}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}\n",
        );
        let traj = convert_jsonl_content_to_atif(content).unwrap();
        assert_eq!(traj.steps.len(), 1);
        assert_eq!(traj.steps[0].source, StepSource::User);
        assert_eq!(traj.steps[0].message, "hello");
        assert_eq!(traj.agent.name, "unknown");
    }
}
