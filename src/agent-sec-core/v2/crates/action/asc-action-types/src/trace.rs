//! Normalize the V1 business-correlation payload without accepting identity claims.

use serde_json::{Map, Value};

use crate::Correlation;

/// Bounded caller metadata, separate from kernel-authenticated attribution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionTraceContext {
    /// Opaque business correlation fields.
    pub correlation: Correlation,
    /// Caller-declared agent label, never an authorization principal.
    pub agent_name: Option<String>,
}

impl ActionTraceContext {
    /// Accepts V1 snake/camel aliases; valid snake values take precedence.
    ///
    /// Unknown fields and non-string/empty values are ignored. Values are
    /// stripped and capped to 256 Unicode characters with the V1 suffix.
    pub fn from_payload(payload: Option<&Map<String, Value>>) -> Self {
        let field = |snake, camel| {
            payload.and_then(|p| clean(p.get(snake)).or_else(|| clean(p.get(camel))))
        };
        Self {
            correlation: Correlation {
                trace_id: field("trace_id", "traceId").unwrap_or_default(),
                session_id: field("session_id", "sessionId"),
                run_id: field("run_id", "runId"),
                call_id: field("call_id", "callId"),
                tool_call_id: field("tool_call_id", "toolCallId"),
            },
            agent_name: field("agent_name", "agentName"),
        }
    }

    /// Emits only normalized V1 metadata, suitable for a method parameter.
    pub fn to_payload(&self) -> Map<String, Value> {
        [
            ("trace_id", Some(self.correlation.trace_id.as_str())),
            ("session_id", self.correlation.session_id.as_deref()),
            ("run_id", self.correlation.run_id.as_deref()),
            ("call_id", self.correlation.call_id.as_deref()),
            ("tool_call_id", self.correlation.tool_call_id.as_deref()),
            ("agent_name", self.agent_name.as_deref()),
        ]
        .into_iter()
        .filter_map(|(key, value)| {
            value
                .filter(|s| !s.is_empty())
                .map(|value| (key.to_owned(), Value::String(value.to_owned())))
        })
        .collect()
    }
}

fn clean(value: Option<&Value>) -> Option<String> {
    const SUFFIX: &str = "...[truncated]";
    let text = value?
        .as_str()?
        .trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c));
    if text.is_empty() {
        return None;
    }
    Some(if text.chars().count() > 256 {
        text.chars()
            .take(256 - SUFFIX.len())
            .chain(SUFFIX.chars())
            .collect()
    } else {
        text.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aliases_whitespace_limits_and_identity_claims_match_v1() {
        let payload = json!({
            "trace_id": "\u{1c} snake \u{1f}", "traceId": "camel",
            "session_id": false, "sessionId": " session ",
            "run_id": " ", "runId": "run", "callId": 3,
            "toolCallId": "工具".repeat(200), "agentName": " agent ",
            "uid": 0, "gid": 0, "pid": 0, "unknown": "discard",
        });
        let normalized = ActionTraceContext::from_payload(payload.as_object());
        assert_eq!(normalized.correlation.trace_id, "snake");
        assert_eq!(
            normalized.correlation.session_id.as_deref(),
            Some("session")
        );
        assert_eq!(normalized.correlation.run_id.as_deref(), Some("run"));
        assert_eq!(normalized.correlation.call_id, None);
        let id = normalized.correlation.tool_call_id.as_deref().unwrap();
        assert_eq!(id.chars().count(), 256);
        assert!(id.ends_with("...[truncated]"));
        assert_eq!(normalized.agent_name.as_deref(), Some("agent"));
        let result = normalized.to_payload();
        assert_eq!(result.len(), 5);
        assert!(!result.contains_key("uid"));
        assert!(
            ActionTraceContext::from_payload(None)
                .to_payload()
                .is_empty()
        );
    }
}
