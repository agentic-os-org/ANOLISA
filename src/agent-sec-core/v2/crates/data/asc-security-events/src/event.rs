//! The canonical security-event envelope (v1 `security_events/schema.py`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::EventError;
use crate::timestamp::{NaivePolicy, normalize_iso_to_utc_iso, now_iso};

/// Outcome of the action that produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EventResult {
    /// Action completed (v1 default).
    #[default]
    Succeeded,
    /// Action failed.
    Failed,
}

/// Single security event persisted as one `JSONL` record and one `SQLite` row.
///
/// Field order is deliberate: it reproduces v1 `SecurityEvent.to_dict()`, which
/// is what the `JSONL` writer serializes, so records stay comparable key by key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SecurityEventRaw")]
pub struct SecurityEvent {
    /// Unique event identifier; UUID v4 unless supplied.
    pub event_id: String,
    /// Producer-defined event kind, e.g. `sandbox_prehook`.
    pub event_type: String,
    /// Action category used for grouping.
    pub category: String,
    /// Whether the action succeeded.
    pub result: EventResult,
    /// UTC ISO-8601 timestamp, normalized on construction.
    pub timestamp: String,
    /// Middleware-injected trace identifier; empty until then.
    pub trace_id: String,
    /// Producing process id.
    pub pid: u32,
    /// Producing user id.
    pub uid: u32,
    /// Optional session correlation.
    pub session_id: Option<String>,
    /// Optional agent run/turn correlation.
    pub run_id: Option<String>,
    /// Optional LLM call correlation.
    pub call_id: Option<String>,
    /// Optional tool call correlation.
    pub tool_call_id: Option<String>,
    /// Backend-specific structured payload.
    pub details: Map<String, Value>,
}

impl SecurityEvent {
    /// Builds an event with v1's auto-filled defaults.
    #[must_use]
    pub fn new(
        event_type: impl Into<String>,
        category: impl Into<String>,
        details: Map<String, Value>,
    ) -> Self {
        Self {
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.into(),
            category: category.into(),
            result: EventResult::Succeeded,
            timestamp: now_iso(),
            trace_id: String::new(),
            pid: std::process::id(),
            uid: rustix::process::getuid().as_raw(),
            session_id: None,
            run_id: None,
            call_id: None,
            tool_call_id: None,
            details,
        }
    }

    /// Replaces the timestamp, applying v1's normalization rules.
    ///
    /// An empty value resets the field to the current time, matching the v1
    /// field validator rather than storing an empty string.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::Timestamp`] when the value is not ISO-8601.
    pub fn set_timestamp(&mut self, value: &str) -> Result<(), EventError> {
        self.timestamp = normalize_timestamp(value)?;
        Ok(())
    }
}

/// Returns the verdict embedded in `details`, if present.
///
/// Checks the top-level `verdict` key first and then `result.verdict`, exactly
/// as v1 `extract_verdict` does.
#[must_use]
pub fn extract_verdict(details: &Map<String, Value>) -> Option<String> {
    if let Some(Value::String(direct)) = details.get("verdict") {
        return Some(direct.clone());
    }

    if let Some(Value::Object(result)) = details.get("result")
        && let Some(Value::String(nested)) = result.get("verdict")
    {
        return Some(nested.clone());
    }

    None
}

fn normalize_timestamp(value: &str) -> Result<String, EventError> {
    if value.is_empty() {
        return Ok(now_iso());
    }
    // Keep JSONL and SQLite writers aligned: timestamps are stored as UTC.
    Ok(normalize_iso_to_utc_iso(
        value,
        "timestamp",
        NaivePolicy::Local,
    )?)
}

/// Wire form used to apply the timestamp validator during deserialization,
/// mirroring pydantic's validate-on-construct behaviour.
#[derive(Deserialize)]
struct SecurityEventRaw {
    #[serde(default = "new_uuid")]
    event_id: String,
    event_type: String,
    category: String,
    #[serde(default)]
    result: EventResult,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    trace_id: String,
    #[serde(default = "std::process::id")]
    pid: u32,
    #[serde(default = "current_uid")]
    uid: u32,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    details: Map<String, Value>,
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

impl TryFrom<SecurityEventRaw> for SecurityEvent {
    type Error = EventError;

    fn try_from(raw: SecurityEventRaw) -> Result<Self, Self::Error> {
        Ok(Self {
            event_id: raw.event_id,
            event_type: raw.event_type,
            category: raw.category,
            result: raw.result,
            timestamp: normalize_timestamp(&raw.timestamp)?,
            trace_id: raw.trace_id,
            pid: raw.pid,
            uid: raw.uid,
            session_id: raw.session_id,
            run_id: raw.run_id,
            call_id: raw.call_id,
            tool_call_id: raw.tool_call_id,
            details: raw.details,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn details_of(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected object, got {other}"),
        }
    }

    #[test]
    fn defaults_match_v1() {
        let event = SecurityEvent::new("sandbox_prehook", "sandbox", Map::new());
        assert_eq!(event.result, EventResult::Succeeded);
        assert_eq!(event.trace_id, "");
        assert_eq!(event.pid, std::process::id());
        assert_eq!(event.uid, rustix::process::getuid().as_raw());
        assert!(event.session_id.is_none());
        assert_eq!(
            uuid::Uuid::parse_str(&event.event_id)
                .unwrap()
                .get_version_num(),
            4
        );
        assert!(
            event.timestamp.ends_with("+00:00"),
            "got {}",
            event.timestamp
        );
    }

    #[test]
    fn serialized_key_order_matches_v1_to_dict() {
        let event = SecurityEvent::new("t", "c", details_of(json!({"a": 1})));
        let encoded = serde_json::to_string(&event).unwrap();
        // v1 `to_dict()` order. Compared by position of each `"key":` token so
        // that payload values cannot be mistaken for keys.
        let expected = [
            "event_id",
            "event_type",
            "category",
            "result",
            "timestamp",
            "trace_id",
            "pid",
            "uid",
            "session_id",
            "run_id",
            "call_id",
            "tool_call_id",
            "details",
        ];
        let mut previous = 0;
        for key in expected {
            let token = format!("\"{key}\":");
            let position = encoded
                .find(&token)
                .unwrap_or_else(|| panic!("missing key {key} in {encoded}"));
            assert!(
                position >= previous,
                "key {key} is out of v1 order in {encoded}"
            );
            previous = position;
        }
    }

    #[test]
    fn optional_fields_serialize_as_null_like_v1() {
        let event = SecurityEvent::new("t", "c", Map::new());
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(encoded["session_id"], Value::Null);
        assert_eq!(encoded["run_id"], Value::Null);
        assert_eq!(encoded["call_id"], Value::Null);
        assert_eq!(encoded["tool_call_id"], Value::Null);
    }

    #[test]
    fn result_serializes_as_lowercase_literal() {
        assert_eq!(
            serde_json::to_string(&EventResult::Succeeded).unwrap(),
            "\"succeeded\""
        );
        assert_eq!(
            serde_json::to_string(&EventResult::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn deserialization_normalizes_offset_timestamp_to_utc() {
        let event: SecurityEvent = serde_json::from_value(json!({
            "event_type": "t",
            "category": "c",
            "details": {},
            "timestamp": "2026-01-02T11:04:05+08:00",
        }))
        .unwrap();
        assert_eq!(event.timestamp, "2026-01-02T03:04:05+00:00");
    }

    #[test]
    fn deserialization_replaces_empty_timestamp() {
        let event: SecurityEvent = serde_json::from_value(json!({
            "event_type": "t",
            "category": "c",
            "details": {},
            "timestamp": "",
        }))
        .unwrap();
        assert!(event.timestamp.ends_with("+00:00"));
    }

    #[test]
    fn deserialization_rejects_invalid_timestamp() {
        let err = serde_json::from_value::<SecurityEvent>(json!({
            "event_type": "t",
            "category": "c",
            "details": {},
            "timestamp": "nope",
        }))
        .expect_err("invalid timestamp must fail");
        assert!(
            err.to_string()
                .contains("Invalid time format for timestamp")
        );
    }

    #[test]
    fn set_timestamp_normalizes_and_rejects() {
        let mut event = SecurityEvent::new("t", "c", Map::new());
        event.set_timestamp("2026-01-02T03:04:05Z").unwrap();
        assert_eq!(event.timestamp, "2026-01-02T03:04:05+00:00");
        assert!(event.set_timestamp("garbage").is_err());
    }

    #[test]
    fn extract_verdict_prefers_top_level_string() {
        let details = details_of(json!({"verdict": "deny", "result": {"verdict": "allow"}}));
        assert_eq!(extract_verdict(&details).as_deref(), Some("deny"));
    }

    #[test]
    fn extract_verdict_falls_back_to_nested_result() {
        let details = details_of(json!({"result": {"verdict": "allow"}}));
        assert_eq!(extract_verdict(&details).as_deref(), Some("allow"));
    }

    #[test]
    fn extract_verdict_ignores_non_string_and_non_object() {
        assert!(extract_verdict(&details_of(json!({"verdict": 1}))).is_none());
        assert!(extract_verdict(&details_of(json!({"result": "deny"}))).is_none());
        assert!(extract_verdict(&details_of(json!({"result": {"verdict": 2}}))).is_none());
        assert!(extract_verdict(&Map::new()).is_none());
    }

    #[test]
    fn round_trip_preserves_all_fields() {
        let mut event = SecurityEvent::new(
            "t",
            "c",
            details_of(json!({"k": [1, 2, {"n": "\u{4f60}\u{597d}"}]})),
        );
        event.session_id = Some("s".to_owned());
        event.run_id = Some("r".to_owned());
        event.result = EventResult::Failed;
        let encoded = serde_json::to_string(&event).unwrap();
        let decoded: SecurityEvent = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn non_ascii_payload_is_not_escaped() {
        // v1 writes JSONL with ensure_ascii=False.
        let event = SecurityEvent::new("t", "c", details_of(json!({"msg": "\u{4f60}\u{597d}"})));
        let encoded = serde_json::to_string(&event).unwrap();
        assert!(encoded.contains("\u{4f60}\u{597d}"), "got {encoded}");
    }
}
