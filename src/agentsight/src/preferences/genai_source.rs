//! `genai_events.db` provider: maps raw event columns to the unified
//! [`PreferenceEventRow`] shape consumed by the rule engine.
//!
//! Linux-only because the cleaning/mining steps lean on the eBPF-side genai
//! parser types; the trajectory provider covers macOS.

use super::detector::PreferenceEventRow;
use crate::genai::GenAIBuilder;
use crate::genai::semantic::{MessagePart, OutputMessage};

/// Build one unified row from the narrow `genai_events` column set.
///
/// The user query is cleaned of agent template noise (cosh-ng `user_input:`
/// lines, timestamp prefixes, `<system-reminder>` blocks) and the
/// `output_messages` JSON is mined for tool-call names, so the rule engine
/// never touches raw wire-capture formats.
pub fn row_from_event(
    id: i64,
    session_id: Option<String>,
    conversation_id: Option<String>,
    start_timestamp_ns: i64,
    user_query: Option<&str>,
    output_messages: Option<&str>,
) -> PreferenceEventRow {
    PreferenceEventRow {
        id,
        session_id,
        conversation_id,
        timestamp_ns: Some(start_timestamp_ns),
        user_text: user_query.and_then(cleaned_user_query),
        tool_names: output_messages
            .map(|raw| tool_names_from_output_messages(id, raw))
            .unwrap_or_default(),
    }
}

/// User query with agent template noise stripped. Returns `None` when there
/// is no usable text left.
fn cleaned_user_query(raw: &str) -> Option<String> {
    let cleaned = GenAIBuilder::strip_user_query_prefix(raw);
    if cleaned.trim().is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Tool-call names mined from the `output_messages` JSON array. Bodies come
/// from wire capture and may be truncated, so malformed JSON is tolerated
/// with a debug log rather than an error.
fn tool_names_from_output_messages(event_id: i64, raw: &str) -> Vec<String> {
    let messages: Vec<OutputMessage> = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(e) => {
            log::debug!(
                "Preference genai source: unparseable output_messages (event_id={event_id}): {e}"
            );
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for message in &messages {
        for part in &message.parts {
            if let MessagePart::ToolCall { name, .. } = part {
                if !name.is_empty() {
                    names.push(name.clone());
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_agent_template_prefix_from_user_query() {
        let row = row_from_event(
            1,
            Some("s1".to_string()),
            Some("c1".to_string()),
            42,
            Some("Some preamble text\n  user_input: 先出方案再写代码\n\nruntime_frame:"),
            None,
        );
        assert_eq!(row.user_text.as_deref(), Some("先出方案再写代码"));
        assert_eq!(row.timestamp_ns, Some(42));
        assert!(row.tool_names.is_empty());

        // Nothing usable left after stripping → None, not an empty string.
        let row = row_from_event(2, None, None, 0, Some("   "), None);
        assert!(row.user_text.is_none());
    }

    #[test]
    fn mines_tool_names_and_tolerates_bad_json() {
        let output = r#"[{"role":"assistant","parts":[
            {"type":"tool_call","name":"bash","arguments":{"cmd":"ls"}},
            {"type":"text","content":"done"},
            {"type":"tool_call","name":"bash"}
        ]}]"#;
        let row = row_from_event(1, None, None, 0, None, Some(output));
        assert_eq!(row.tool_names, vec!["bash", "bash"]);

        // Truncated JSON must not error out or emit tool names.
        let row = row_from_event(
            2,
            None,
            None,
            0,
            None,
            Some(r#"[{"role":"assistant","parts":[{"type":"tool_c"#),
        );
        assert!(row.tool_names.is_empty());
    }
}
