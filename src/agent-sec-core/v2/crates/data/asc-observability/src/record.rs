//! The observability record envelope and its correlation metadata.
//!
//! Migrated from v1 `observability/schema.py`. The v1 model is a pydantic
//! discriminated union over `hook`; the six variants differ only in which
//! metadata fields they model and which metric names they accept, so v2 keeps a
//! single record type plus the per-hook tables in [`crate::hook`].
//!
//! Three wire behaviours were confirmed against a live v1 run and are load
//! bearing:
//!
//! * `observedAt` is rendered with `Z` for a zero offset and `+HH:MM`
//!   otherwise, and its fractional part is either absent (whole second) or
//!   exactly six digits.
//! * `metrics` is emitted in per-hook *declaration* order, restricted to the
//!   keys that were actually supplied. Unknown keys are dropped on ingest.
//! * `metadata` is emitted as `sessionId`, `runId`, `toolCallId`, `callId`,
//!   with absent optional fields omitted rather than serialized as `null`.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset, NaiveDateTime, SecondsFormat, Timelike};
use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::correlation::truncate_correlation_id;
use crate::error::ObservabilityError;
use crate::hook::{MetadataShape, ObservabilityHook, sorted_hook_names};

/// Correlation metadata carried by every observability record.
///
/// Field order is the serialization order and matches v1's class hierarchy:
/// the shared fields come from `ObservabilityMetadata`, then `toolCallId`
/// (declared by `ToolCallMetadata`), then `callId`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ObservabilityMetadata {
    /// Session this record belongs to.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// Run (one user turn) this record belongs to.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// Tool invocation ID; required for the two tool-call hooks.
    #[serde(rename = "toolCallId", skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Model API call ID; optional for model-call and tool-call hooks.
    #[serde(rename = "callId", skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

impl ObservabilityMetadata {
    /// Builds metadata for a hook that only needs session and run IDs.
    #[must_use]
    pub fn new(session_id: &str, run_id: &str) -> Self {
        Self {
            session_id: truncate_correlation_id(session_id),
            run_id: truncate_correlation_id(run_id),
            tool_call_id: None,
            call_id: None,
        }
    }

    /// Returns a copy with `callId` set.
    #[must_use]
    pub fn with_call_id(mut self, call_id: &str) -> Self {
        self.call_id = Some(truncate_correlation_id(call_id));
        self
    }

    /// Returns a copy with `toolCallId` set.
    #[must_use]
    pub fn with_tool_call_id(mut self, tool_call_id: &str) -> Self {
        self.tool_call_id = Some(truncate_correlation_id(tool_call_id));
        self
    }

    /// Serializes the metadata exactly as the `metadata_json` column stores it.
    ///
    /// # Errors
    ///
    /// Returns an error only if the JSON writer fails, which cannot happen for
    /// this shape.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Drops fields the hook does not model, mirroring v1 `extra="ignore"`.
    fn conform_to(&mut self, hook: ObservabilityHook) -> Result<(), ObservabilityError> {
        match hook.metadata_shape() {
            MetadataShape::Common => {
                self.tool_call_id = None;
                self.call_id = None;
            }
            MetadataShape::ModelCall => {
                self.tool_call_id = None;
            }
            MetadataShape::ToolCall => {
                if self.tool_call_id.is_none() {
                    return Err(ObservabilityError::MissingMetadata {
                        field: "toolCallId",
                        hook: hook.as_str().to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// A validated, hook-scoped metrics payload.
///
/// Construction filters the supplied keys against the hook allowlist, so an
/// instance can never carry a metric the hook does not declare.
#[derive(Debug, Clone, PartialEq)]
pub struct HookMetrics {
    hook: ObservabilityHook,
    values: BTreeMap<String, Value>,
}

impl HookMetrics {
    /// Filters `values` to the hook allowlist.
    ///
    /// # Errors
    ///
    /// Returns [`ObservabilityError::EmptyMetrics`] when no allowed metric
    /// survives the filter, matching v1
    /// `_validate_at_least_one_known_metric`.
    pub fn new(
        hook: ObservabilityHook,
        values: Map<String, Value>,
    ) -> Result<Self, ObservabilityError> {
        let allowed: BTreeMap<String, Value> = values
            .into_iter()
            .filter(|(key, _)| hook.metric_names().contains(&key.as_str()))
            .collect();

        if allowed.is_empty() {
            return Err(ObservabilityError::EmptyMetrics);
        }

        Ok(Self {
            hook,
            values: allowed,
        })
    }

    /// Returns the hook these metrics belong to.
    #[must_use]
    pub const fn hook(&self) -> ObservabilityHook {
        self.hook
    }

    /// Looks up one metric value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// Returns the number of retained metrics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether no metric is retained; always `false` after validation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Iterates the retained metrics in v1 declaration order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Value)> {
        self.hook
            .metric_names()
            .iter()
            .filter_map(move |name| self.values.get(*name).map(|value| (*name, value)))
    }

    /// Serializes the metrics exactly as the `metrics_json` column stores it.
    ///
    /// # Errors
    ///
    /// Returns an error only if the JSON writer fails.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl Serialize for HookMetrics {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.values.len()))?;
        for (name, value) in self.iter() {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

/// One validated observability record.
///
/// The invariant "metrics and metadata match `hook`" is enforced by
/// [`ObservabilityRecord::new`], so the fields stay private.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservabilityRecord {
    hook: ObservabilityHook,
    observed_at: DateTime<FixedOffset>,
    metadata: ObservabilityMetadata,
    metrics: HookMetrics,
}

impl ObservabilityRecord {
    /// Validates and assembles one record.
    ///
    /// `observed_at` is truncated to microsecond precision because v1 carries a
    /// Python `datetime`, whose resolution is one microsecond.
    ///
    /// # Errors
    ///
    /// Returns [`ObservabilityError::MissingMetadata`] when a tool-call hook is
    /// missing `toolCallId`, or [`ObservabilityError::EmptyMetrics`] when no
    /// allowed metric was supplied.
    pub fn new(
        hook: ObservabilityHook,
        observed_at: DateTime<FixedOffset>,
        metadata: ObservabilityMetadata,
        metrics: Map<String, Value>,
    ) -> Result<Self, ObservabilityError> {
        let mut metadata = metadata;
        metadata.session_id = truncate_correlation_id(&metadata.session_id);
        metadata.run_id = truncate_correlation_id(&metadata.run_id);
        metadata.tool_call_id = metadata
            .tool_call_id
            .as_deref()
            .map(truncate_correlation_id);
        metadata.call_id = metadata.call_id.as_deref().map(truncate_correlation_id);
        metadata.conform_to(hook)?;

        Ok(Self {
            hook,
            observed_at: truncate_to_micros(observed_at),
            metadata,
            metrics: HookMetrics::new(hook, metrics)?,
        })
    }

    /// Returns the hook that produced this record.
    #[must_use]
    pub const fn hook(&self) -> ObservabilityHook {
        self.hook
    }

    /// Returns the observation instant, offset preserved.
    #[must_use]
    pub const fn observed_at(&self) -> DateTime<FixedOffset> {
        self.observed_at
    }

    /// Returns the correlation metadata.
    #[must_use]
    pub const fn metadata(&self) -> &ObservabilityMetadata {
        &self.metadata
    }

    /// Returns the validated metrics payload.
    #[must_use]
    pub const fn metrics(&self) -> &HookMetrics {
        &self.metrics
    }

    /// Renders `observedAt` in the exact v1 wire form.
    #[must_use]
    pub fn observed_at_iso(&self) -> String {
        format_observed_at(self.observed_at)
    }

    /// Returns the value stored in the `observed_at_epoch` column.
    #[must_use]
    pub fn observed_at_epoch(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)] // Mirrors Python `datetime.timestamp()`.
        let micros = self.observed_at.timestamp_micros() as f64;
        micros / 1_000_000.0
    }

    /// Serializes the record as one JSONL line body.
    ///
    /// # Errors
    ///
    /// Returns an error only if the JSON writer fails.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Parses and validates one record from a JSON value.
    ///
    /// # Errors
    ///
    /// Returns the first validation failure, mirroring v1
    /// `validate_observability_record`.
    pub fn from_json_value(value: &Value) -> Result<Self, ObservabilityError> {
        let object = value
            .as_object()
            .ok_or(ObservabilityError::NotAnObject { name: "record" })?;

        let hook_name = object
            .get("hook")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let hook =
            ObservabilityHook::parse(hook_name).ok_or_else(|| ObservabilityError::UnknownHook {
                hook: hook_name.to_owned(),
                expected: sorted_hook_names()
                    .into_iter()
                    .map(|name| format!("'{name}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
            })?;

        let observed_at = parse_observed_at(
            object
                .get("observedAt")
                .or_else(|| object.get("observed_at")),
        )?;

        let metadata = parse_metadata(object.get("metadata").unwrap_or(&Value::Null), hook)?;

        let metrics = match object.get("metrics") {
            Some(Value::Object(map)) => map.clone(),
            None | Some(Value::Null) => Map::new(),
            Some(_) => return Err(ObservabilityError::NotAnObject { name: "metrics" }),
        };

        Self::new(hook, observed_at, metadata, metrics)
    }
}

impl Serialize for ObservabilityRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("ObservabilityRecord", 4)?;
        state.serialize_field("hook", self.hook.as_str())?;
        state.serialize_field("observedAt", &self.observed_at_iso())?;
        state.serialize_field("metadata", &self.metadata)?;
        state.serialize_field("metrics", &self.metrics)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ObservabilityRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_json_value(&value).map_err(serde::de::Error::custom)
    }
}

/// Renders a timestamp the way pydantic's JSON mode does.
///
/// Verified against v1: `Z` for a zero offset, `+HH:MM` otherwise; no
/// fractional part on a whole second, and exactly six digits otherwise.
#[must_use]
pub fn format_observed_at(value: DateTime<FixedOffset>) -> String {
    let format = if value.timestamp_subsec_micros() == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    };
    value.to_rfc3339_opts(format, true)
}

fn truncate_to_micros(value: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    let micros = value.timestamp_subsec_micros();
    value.with_nanosecond(micros * 1_000).unwrap_or(value)
}

fn parse_observed_at(value: Option<&Value>) -> Result<DateTime<FixedOffset>, ObservabilityError> {
    let raw = match value {
        Some(Value::String(text)) => text.as_str(),
        Some(other) => {
            return Err(ObservabilityError::InvalidTimestamp {
                value: other.to_string(),
            });
        }
        None => {
            return Err(ObservabilityError::InvalidTimestamp {
                value: String::new(),
            });
        }
    };

    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(parsed);
    }

    // Distinguish "parses but has no offset" from "not a timestamp at all" so
    // the v1 wording `observedAt must be timezone-aware` stays reachable.
    for pattern in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if NaiveDateTime::parse_from_str(raw, pattern).is_ok() {
            return Err(ObservabilityError::NaiveTimestamp);
        }
    }

    Err(ObservabilityError::InvalidTimestamp {
        value: raw.to_owned(),
    })
}

fn parse_metadata(
    value: &Value,
    hook: ObservabilityHook,
) -> Result<ObservabilityMetadata, ObservabilityError> {
    let object = value
        .as_object()
        .ok_or(ObservabilityError::NotAnObject { name: "metadata" })?;

    let field = |camel: &'static str, snake: &'static str| -> Option<String> {
        object
            .get(camel)
            .or_else(|| object.get(snake))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let required = |camel: &'static str, snake: &'static str| {
        field(camel, snake).ok_or_else(|| ObservabilityError::MissingMetadata {
            field: camel,
            hook: hook.as_str().to_owned(),
        })
    };

    Ok(ObservabilityMetadata {
        session_id: required("sessionId", "session_id")?,
        run_id: required("runId", "run_id")?,
        tool_call_id: field("toolCallId", "tool_call_id"),
        call_id: field("callId", "call_id"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record_from(value: &Value) -> ObservabilityRecord {
        ObservabilityRecord::from_json_value(value).expect("payload must validate")
    }

    /// Golden lines captured from a live v1 `to_record()` run.
    #[test]
    fn wire_form_matches_v1_golden_lines() {
        let cases = [
            (
                json!({
                    "hook": "before_agent_run",
                    "observedAt": "2024-01-02T03:04:05.123456+00:00",
                    "metadata": {"sessionId": "s1", "runId": "r1", "extra": "drop"},
                    "metrics": {
                        "user_input": "hi", "prompt": "p",
                        "unknown_metric": 1, "model_id": "m"
                    },
                }),
                concat!(
                    r#"{"hook":"before_agent_run","observedAt":"2024-01-02T03:04:05.123456Z","#,
                    r#""metadata":{"sessionId":"s1","runId":"r1"},"#,
                    r#""metrics":{"prompt":"p","user_input":"hi","model_id":"m"}}"#,
                ),
            ),
            (
                json!({
                    "hook": "before_agent_run",
                    "observedAt": "2024-01-02T03:04:05+00:00",
                    "metadata": {"sessionId": "s1", "runId": "r1"},
                    "metrics": {"images_count": 0},
                }),
                concat!(
                    r#"{"hook":"before_agent_run","observedAt":"2024-01-02T03:04:05Z","#,
                    r#""metadata":{"sessionId":"s1","runId":"r1"},"#,
                    r#""metrics":{"images_count":0}}"#,
                ),
            ),
            (
                json!({
                    "hook": "after_tool_call",
                    "observedAt": "2024-01-02T03:04:05.500+08:00",
                    "metadata": {"sessionId": "s1", "runId": "r1", "toolCallId": "t1"},
                    "metrics": {"status": "ok", "result": null, "exit_code": 0},
                }),
                concat!(
                    r#"{"hook":"after_tool_call","observedAt":"2024-01-02T03:04:05.500000+08:00","#,
                    r#""metadata":{"sessionId":"s1","runId":"r1","toolCallId":"t1"},"#,
                    r#""metrics":{"result":null,"status":"ok","exit_code":0}}"#,
                ),
            ),
            (
                json!({
                    "hook": "before_llm_call",
                    "observedAt": "2024-01-02T03:04:05.000001+00:00",
                    "metadata": {"sessionId": "s1", "runId": "r1", "callId": "c1"},
                    "metrics": {"transport": "http", "api": "chat", "prompt": "p"},
                }),
                concat!(
                    r#"{"hook":"before_llm_call","observedAt":"2024-01-02T03:04:05.000001Z","#,
                    r#""metadata":{"sessionId":"s1","runId":"r1","callId":"c1"},"#,
                    r#""metrics":{"prompt":"p","api":"chat","transport":"http"}}"#,
                ),
            ),
            (
                json!({
                    "hook": "after_tool_call",
                    "observedAt": "2024-01-02T03:04:05+00:00",
                    "metadata": {
                        "callId": "c1", "toolCallId": "t1",
                        "runId": "r1", "sessionId": "s1"
                    },
                    "metrics": {"status": "ok"},
                }),
                concat!(
                    r#"{"hook":"after_tool_call","observedAt":"2024-01-02T03:04:05Z","#,
                    r#""metadata":{"sessionId":"s1","runId":"r1","toolCallId":"t1","callId":"c1"},"#,
                    r#""metrics":{"status":"ok"}}"#,
                ),
            ),
        ];

        for (payload, expected) in cases {
            let record = record_from(&payload);
            assert_eq!(
                record.to_json_string().expect("serializable"),
                expected,
                "wire form drifted for {}",
                record.hook()
            );
        }
    }

    /// Epoch values captured from a live v1 `observed_at.timestamp()`.
    #[test]
    fn epoch_matches_v1_timestamp() {
        let cases = [
            ("2024-01-02T03:04:05.123456+00:00", 1_704_164_645.123_456),
            ("2024-01-02T03:04:05+00:00", 1_704_164_645.0),
            ("2024-01-02T03:04:05.500+08:00", 1_704_135_845.5),
            ("2024-01-02T03:04:05.000001+00:00", 1_704_164_645.000_001),
        ];
        for (raw, expected) in cases {
            let record = record_from(&json!({
                "hook": "before_agent_run",
                "observedAt": raw,
                "metadata": {"sessionId": "s", "runId": "r"},
                "metrics": {"prompt": "p"},
            }));
            assert!(
                (record.observed_at_epoch() - expected).abs() < 1e-9,
                "epoch drifted for {raw}: {} vs {expected}",
                record.observed_at_epoch()
            );
        }
    }

    #[test]
    fn round_trips_through_json() {
        let payload = json!({
            "hook": "after_llm_call",
            "observedAt": "2024-05-06T07:08:09.010203+02:00",
            "metadata": {"sessionId": "s", "runId": "r", "callId": "c"},
            "metrics": {"latency_ms": 12, "outcome": "ok", "tool_calls": [{"b": 1, "a": 2}]},
        });
        let record = record_from(&payload);
        let line = record.to_json_string().expect("serializable");
        let parsed: ObservabilityRecord = serde_json::from_str(&line).expect("re-parsable");
        assert_eq!(parsed, record);
    }

    #[test]
    fn rejects_naive_timestamp() {
        let err = ObservabilityRecord::from_json_value(&json!({
            "hook": "before_agent_run",
            "observedAt": "2024-01-02T03:04:05",
            "metadata": {"sessionId": "s", "runId": "r"},
            "metrics": {"prompt": "p"},
        }))
        .expect_err("naive timestamps must be rejected");
        assert_eq!(err, ObservabilityError::NaiveTimestamp);
        assert_eq!(err.to_string(), "observedAt must be timezone-aware");
    }

    #[test]
    fn rejects_unparseable_timestamp() {
        let err = ObservabilityRecord::from_json_value(&json!({
            "hook": "before_agent_run",
            "observedAt": "not-a-time",
            "metadata": {"sessionId": "s", "runId": "r"},
            "metrics": {"prompt": "p"},
        }))
        .expect_err("garbage timestamps must be rejected");
        assert!(matches!(err, ObservabilityError::InvalidTimestamp { .. }));
    }

    #[test]
    fn rejects_empty_and_unknown_only_metrics() {
        for metrics in [json!({}), json!({"nope": 1})] {
            let err = ObservabilityRecord::from_json_value(&json!({
                "hook": "before_agent_run",
                "observedAt": "2024-01-02T03:04:05+00:00",
                "metadata": {"sessionId": "s", "runId": "r"},
                "metrics": metrics,
            }))
            .expect_err("metrics must contain an allowed name");
            assert_eq!(err, ObservabilityError::EmptyMetrics);
        }
    }

    #[test]
    fn rejects_unknown_hook_with_v1_wording() {
        let err = ObservabilityRecord::from_json_value(&json!({
            "hook": "nope",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {"sessionId": "s", "runId": "r"},
            "metrics": {"prompt": "p"},
        }))
        .expect_err("unknown hooks must be rejected");
        assert_eq!(
            err.to_string(),
            "unknown observability hook 'nope'; expected one of \
             ['after_agent_run', 'after_llm_call', 'after_tool_call', \
             'before_agent_run', 'before_llm_call', 'before_tool_call']"
        );
    }

    #[test]
    fn rejects_tool_call_hook_without_tool_call_id() {
        let err = ObservabilityRecord::from_json_value(&json!({
            "hook": "after_tool_call",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {"sessionId": "s", "runId": "r"},
            "metrics": {"status": "x"},
        }))
        .expect_err("toolCallId is required");
        assert_eq!(
            err,
            ObservabilityError::MissingMetadata {
                field: "toolCallId",
                hook: "after_tool_call".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_missing_session_and_run() {
        for metadata in [json!({"runId": "r"}), json!({"sessionId": "s"})] {
            let err = ObservabilityRecord::from_json_value(&json!({
                "hook": "before_agent_run",
                "observedAt": "2024-01-02T03:04:05+00:00",
                "metadata": metadata,
                "metrics": {"prompt": "p"},
            }))
            .expect_err("session and run are required");
            assert!(matches!(err, ObservabilityError::MissingMetadata { .. }));
        }
    }

    #[test]
    fn rejects_non_object_members() {
        let err = ObservabilityRecord::from_json_value(&json!({
            "hook": "before_agent_run",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": [],
            "metrics": {"prompt": "p"},
        }))
        .expect_err("metadata must be an object");
        assert_eq!(err, ObservabilityError::NotAnObject { name: "metadata" });

        let err = ObservabilityRecord::from_json_value(&json!({
            "hook": "before_agent_run",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {"sessionId": "s", "runId": "r"},
            "metrics": [],
        }))
        .expect_err("metrics must be an object");
        assert_eq!(err, ObservabilityError::NotAnObject { name: "metrics" });
    }

    /// v1 `extra="ignore"` drops correlation keys the hook does not model.
    #[test]
    fn drops_metadata_fields_the_hook_does_not_model() {
        let record = record_from(&json!({
            "hook": "before_agent_run",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {
                "sessionId": "s", "runId": "r",
                "callId": "c", "toolCallId": "t"
            },
            "metrics": {"prompt": "p"},
        }));
        assert_eq!(record.metadata().call_id, None);
        assert_eq!(record.metadata().tool_call_id, None);

        let record = record_from(&json!({
            "hook": "after_llm_call",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {
                "sessionId": "s", "runId": "r",
                "callId": "c", "toolCallId": "t"
            },
            "metrics": {"outcome": "ok"},
        }));
        assert_eq!(record.metadata().call_id.as_deref(), Some("c"));
        assert_eq!(record.metadata().tool_call_id, None);
    }

    #[test]
    fn caps_correlation_ids() {
        let record = record_from(&json!({
            "hook": "before_agent_run",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {"sessionId": "x".repeat(300), "runId": "r"},
            "metrics": {"prompt": "p"},
        }));
        assert_eq!(record.metadata().session_id.chars().count(), 256);
        assert!(record.metadata().session_id.ends_with("...[truncated]"));
    }

    #[test]
    fn metrics_iterate_in_declaration_order() {
        let record = record_from(&json!({
            "hook": "after_agent_run",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {"sessionId": "s", "runId": "r"},
            "metrics": {"duration_ms": 1, "response": "r", "success": true},
        }));
        let names: Vec<&str> = record.metrics().iter().map(|(name, _)| name).collect();
        assert_eq!(names, vec!["response", "success", "duration_ms"]);
        assert_eq!(record.metrics().len(), 3);
        assert!(!record.metrics().is_empty());
    }

    #[test]
    fn column_json_helpers_match_wire_members() {
        let record = record_from(&json!({
            "hook": "before_tool_call",
            "observedAt": "2024-01-02T03:04:05+00:00",
            "metadata": {"sessionId": "s", "runId": "r", "toolCallId": "t"},
            "metrics": {"parameters": {"a": 1}, "tool_name": "bash"},
        }));
        assert_eq!(
            record.metrics().to_json_string().expect("serializable"),
            r#"{"tool_name":"bash","parameters":{"a":1}}"#
        );
        assert_eq!(
            record.metadata().to_json_string().expect("serializable"),
            r#"{"sessionId":"s","runId":"r","toolCallId":"t"}"#
        );
    }

    #[test]
    fn sub_microsecond_precision_is_truncated_like_python() {
        let record = record_from(&json!({
            "hook": "before_agent_run",
            "observedAt": "2024-01-02T03:04:05.123456789+00:00",
            "metadata": {"sessionId": "s", "runId": "r"},
            "metrics": {"prompt": "p"},
        }));
        assert_eq!(record.observed_at_iso(), "2024-01-02T03:04:05.123456Z");
    }

    #[test]
    fn snake_case_aliases_are_accepted() {
        let record = record_from(&json!({
            "hook": "after_tool_call",
            "observed_at": "2024-01-02T03:04:05+00:00",
            "metadata": {"session_id": "s", "run_id": "r", "tool_call_id": "t"},
            "metrics": {"status": "ok"},
        }));
        assert_eq!(record.metadata().session_id, "s");
        assert_eq!(record.metadata().tool_call_id.as_deref(), Some("t"));
    }

    #[test]
    fn direct_construction_validates_the_same_way() {
        let observed_at = DateTime::parse_from_rfc3339("2024-01-02T03:04:05+00:00").expect("valid");
        let metadata = ObservabilityMetadata::new("s", "r").with_tool_call_id("t");
        let mut metrics = Map::new();
        metrics.insert("status".to_owned(), json!("ok"));
        let record = ObservabilityRecord::new(
            ObservabilityHook::AfterToolCall,
            observed_at,
            metadata,
            metrics,
        )
        .expect("valid record");
        assert_eq!(record.observed_at_iso(), "2024-01-02T03:04:05Z");

        let err = ObservabilityRecord::new(
            ObservabilityHook::AfterToolCall,
            observed_at,
            ObservabilityMetadata::new("s", "r"),
            Map::new(),
        )
        .expect_err("missing toolCallId must fail before metrics");
        assert!(matches!(err, ObservabilityError::MissingMetadata { .. }));
    }
}
