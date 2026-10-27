//! Translate AW callback metadata into the public sec-core CLI tracing context.

use serde_json::{json, Value};

/// Extracts bounded native trace fields without substituting AW-generated identifiers.
pub(super) fn from_event(event: &Value) -> Value {
    let adapter = event["agent"]["adapter"].as_str().unwrap_or_default();
    let native = &event["native"];
    let sources = match adapter {
        "openclaw" => vec![&native["event"], &native["context"]],
        "hermes" => vec![native, &native["extra"]],
        _ => vec![native],
    };
    let fields: [(&str, &[&str]); 5] = [
        ("trace_id", &["trace_id", "traceId"]),
        ("session_id", &["session_id", "sessionId"]),
        ("run_id", &["run_id", "runId"]),
        ("call_id", &["call_id", "callId"]),
        (
            "tool_call_id",
            &["tool_call_id", "toolCallId", "tool_use_id", "toolUseId"],
        ),
    ];
    let mut context = json!({"agent_name": adapter});
    for (key, aliases) in fields {
        if let Some(value) = sources.iter().find_map(|source| {
            aliases
                .iter()
                .find_map(|alias| correlation_value(&source[*alias]))
        }) {
            context[key] = json!(value);
        }
    }
    // AW call IDs may be scoped hashes or invocation UUIDs, not native tool IDs.
    let original_call = match adapter {
        "qoder" | "qoder-sdk" | "claude-sdk" => correlation_value(&native["tool_use_id"]),
        "pi" | "qwenpaw" | "agentscope" => correlation_value(&native["tool_call"]["id"]),
        _ => None,
    };
    if let Some(call) = original_call {
        context["tool_call_id"] = json!(call);
    }
    context
}

// Apply the CLI's normalization before argv reaches the OS argument limit.
fn correlation_value(value: &Value) -> Option<String> {
    const SUFFIX: &str = "...[truncated]";
    let value = value
        .as_str()?
        .trim_matches(|ch: char| ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch));
    if value.is_empty() {
        None
    } else if value.chars().count() <= 256 {
        Some(value.to_owned())
    } else {
        Some(value.chars().take(256 - SUFFIX.len()).collect::<String>() + SUFFIX)
    }
}
