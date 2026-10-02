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

    for e in events {
        if cwd.is_none() {
            cwd = e.get("cwd").and_then(|v| v.as_str()).map(String::from);
        }
        match e.get("type").and_then(|v| v.as_str()) {
            Some("user") => user_count += 1,
            Some("assistant") => assistant_count += 1,
            _ => {}
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
}
