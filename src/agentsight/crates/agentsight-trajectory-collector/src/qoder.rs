//! Qoder/QoderWork JSONL parsing helpers.
//!
//! Splits raw session file content into JSON events and extracts the
//! Qoder-private metadata that rides along in the ATIF document's `extra`
//! field (it has no dedicated ATIF columns).

use std::collections::HashMap;

/// Parse JSONL content into JSON events; blank/malformed lines are skipped
/// with a warning so one bad line never poisons a whole session.
pub fn load_jsonl_events(content: &str) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => events.push(v),
            Err(e) => {
                log::warn!("Skipping malformed JSON on line {}: {e}", line_num + 1);
            }
        }
    }
    events
}

/// Qoder-private session info destined for the ATIF `extra` field.
///
/// Returns a map with `cwd` (first seen), `user_message_count`,
/// `assistant_message_count` and `project`.
pub fn extract_private_metadata(
    events: &[serde_json::Value],
    project: &str,
) -> HashMap<String, serde_json::Value> {
    let mut cwd: Option<String> = None;
    let mut user_count: u64 = 0;
    let mut assistant_count: u64 = 0;
    let mut in_assistant_turn = false;

    for e in events {
        if cwd.is_none() {
            cwd = e.get("cwd").and_then(|v| v.as_str()).map(String::from);
        }
        match e.get("type").and_then(|v| v.as_str()) {
            // Claude-style tool results also ride in type=="user" events;
            // they are observations, not user messages.
            Some("user") => {
                in_assistant_turn = false;
                if !carries_only_tool_results(e.get("message")) {
                    user_count += 1;
                }
            }
            // The ATIF converter merges consecutive assistant events into a
            // single Agent step ("Collect all consecutive assistant events
            // (same LLM turn)"), so count turns rather than events: the count
            // has to describe the trajectory it rides on, as the user side
            // does since 9df75a971.
            Some("assistant") => {
                if !in_assistant_turn {
                    assistant_count += 1;
                    in_assistant_turn = true;
                }
            }
            // A skipped event neither extends nor ends the turn, mirroring the
            // converter's merge loop.
            Some(t) if crate::atif::SKIP_TYPES.contains(&t) => {}
            _ => in_assistant_turn = false,
        }
    }

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
    extra.insert(
        "project".to_string(),
        serde_json::Value::String(project.to_string()),
    );
    extra
}

/// Return `true` when a Claude-style user event only carries tool results:
/// a `message.content` array with at least one `tool_result` block and no
/// `text` block. This mirrors the discrimination the ATIF converter in
/// `atif.rs` applies when classifying such events as observations instead
/// of user turns.
fn carries_only_tool_results(message: Option<&serde_json::Value>) -> bool {
    let Some(content) = message.and_then(|m| m.get("content")) else {
        return false;
    };
    let Some(blocks) = content.as_array() else {
        return false;
    };
    let has_tool_result = blocks
        .iter()
        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));
    let has_text = blocks
        .iter()
        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"));
    has_tool_result && !has_text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_jsonl_skips_malformed() {
        let content = "{\"type\":\"user\"}\n\nnot-json\n{\"type\":\"assistant\"}\n";
        let events = load_jsonl_events(content);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "user");
    }

    #[test]
    fn test_extract_private_metadata() {
        let content = concat!(
            "{\"type\":\"runtime-config\",\"sessionId\":\"abc\",\"model\":\"qwen-max\"}\n",
            "{\"type\":\"user\",\"cwd\":\"/data/myapp\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[]}}\n",
        );
        let events = load_jsonl_events(content);
        let extra = extract_private_metadata(&events, "myapp");
        assert_eq!(extra["cwd"], "/data/myapp");
        assert_eq!(extra["user_message_count"], 1);
        assert_eq!(extra["assistant_message_count"], 1);
        assert_eq!(extra["project"], "myapp");
    }

    #[test]
    fn test_extract_private_metadata_counts_assistant_turns_like_the_trajectory() {
        use agentsight_atif::StepSource;
        // The converter merges consecutive assistant events into one Agent
        // step ("same LLM turn"), so the count that rides in `extra` must
        // describe the trajectory rather than the raw event stream — the same
        // contract the user side follows since 9df75a971.
        let content = concat!(
            "{\"type\":\"user\",\"cwd\":\"/data/myapp\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"part one\"}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"part two\"}]}}\n",
        );
        let events = load_jsonl_events(content);
        let trajectory = crate::atif::convert_qoder_events(&events, "qoder").unwrap();
        let agent_steps = trajectory
            .steps
            .iter()
            .filter(|s| s.source == StepSource::Agent)
            .count();
        let extra = extract_private_metadata(&events, "myapp");
        assert_eq!(agent_steps, 1, "both assistant events are one LLM turn");
        assert_eq!(
            extra["assistant_message_count"], agent_steps as i64,
            "the count must describe the trajectory it rides on"
        );
    }

    #[test]
    fn test_extract_private_metadata_skips_tool_result_carriers() {
        // Claude-style tool results ride in type=="user" events; they are
        // observations, not user messages, so only the first event counts.
        let content = concat!(
            "{\"type\":\"user\",\"cwd\":\"/data/myapp\",\"message\":{\"role\":\"user\",\"content\":\"list the files\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"exec\",\"input\":{}}]}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"file-a\\nfile-b\"}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"read_file\",\"input\":{}}]}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t2\",\"content\":\"file-a contents\"}]}}\n",
        );
        let events = load_jsonl_events(content);
        let extra = extract_private_metadata(&events, "myapp");
        assert_eq!(extra["user_message_count"], 1);
        assert_eq!(extra["assistant_message_count"], 2);
    }

    #[test]
    fn test_extract_private_metadata_counts_text_user_events() {
        // Plain-text user events (string content or text blocks, including
        // one mixed with a tool_result) are genuine user messages.
        let content = concat!(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hello again\"}]}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t9\",\"content\":\"x\"},{\"type\":\"text\",\"text\":\"and this too\"}]}}\n",
        );
        let events = load_jsonl_events(content);
        let extra = extract_private_metadata(&events, "myapp");
        assert_eq!(extra["user_message_count"], 3);
    }
}
