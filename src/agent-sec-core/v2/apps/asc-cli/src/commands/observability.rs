//! V1-compatible stdin adapter to the native observability ingestion contract.
use std::collections::HashMap;
use std::io::Read;

use asc_daemon_protocol::{
    CompatibilityV1, DaemonRequest, ObservabilityRecordParams, TraceCarrierV1, method,
};
use asc_observability::{Context, MetadataKind, MetadataShape, ObservabilityRecord};
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::InputError;

#[derive(Debug, Subcommand)]
pub(crate) enum ObservabilityCommand {
    /// Record one observability JSON object from stdin.
    Record(RecordCommand),
}

#[derive(Debug, Args)]
pub(crate) struct RecordCommand {
    /// Input format: json.
    #[arg(long, default_value = "json")]
    format: String,
    /// Read payload from stdin (required).
    #[arg(long)]
    stdin: bool,
}

impl ObservabilityCommand {
    pub(super) fn request(&self) -> Result<DaemonRequest, InputError> {
        let Self::Record(command) = self;
        command.request(std::io::stdin().lock())
    }
}

impl RecordCommand {
    fn request(&self, input: impl Read) -> Result<DaemonRequest, InputError> {
        let invalid = |message: &str| InputError::Observability(message.to_owned());
        if self.format != "json" {
            return Err(invalid("--format must be json."));
        }
        if !self.stdin {
            return Err(invalid("--stdin is required."));
        }
        let limit = asc_daemon_protocol::BUSINESS_FRAME_BYTES;
        let mut raw = String::new();
        input
            .take(limit as u64 + 1)
            .read_to_string(&mut raw)
            .map_err(|_| invalid("cannot read observability stdin"))?;
        if raw.len() > limit {
            return Err(invalid(
                "observability input exceeds the 4194304-byte input limit",
            ));
        }
        if raw.trim().is_empty() {
            return Err(invalid("stdin is empty"));
        }
        let value: Value = serde_json::from_str(&raw).map_err(|_| invalid("invalid JSON"))?;
        if !value.is_object() {
            return Err(invalid("payload must be a JSON object"));
        }
        let record = ObservabilityRecord::from_json_value(&value)
            .map_err(|error| invalid(&error.to_string()))?;
        let kind = match record.hook().metadata_shape() {
            MetadataShape::Common => MetadataKind::AgentRun,
            MetadataShape::ModelCall => MetadataKind::ModelCall,
            MetadataShape::ToolCall => MetadataKind::ToolCall,
        };
        let context =
            asc_observability::bind_metadata(&Context::current(), &value["metadata"], kind)
                .map_err(|field| invalid(&format!("invalid metadata: {field}")))?;
        let mut headers = HashMap::new();
        asc_observability::inject_context(&context, &mut headers);
        let params = serde_json::to_value(ObservabilityRecordParams {
            hook: record.hook().as_str().to_owned(),
            observed_at: record.observed_at_iso(),
            metrics: record
                .metrics()
                .iter()
                .map(|(key, value)| (key.to_owned(), value.clone()))
                .collect(),
        })?;
        let labels = context.get::<asc_observability::CompatibilityCorrelation>();
        Ok(DaemonRequest {
            method: method::OBS_RECORD.to_owned(),
            params,
            trace_context: Some(TraceCarrierV1::from_headers(headers)),
            compatibility: labels
                .filter(|labels| labels.trace_id.is_some() || labels.invocation_label.is_some())
                .map(|labels| CompatibilityV1 {
                    version: 1,
                    trace_id: labels.trace_id.clone(),
                    invocation_label: labels.invocation_label.clone(),
                }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v1_records_become_business_params_and_native_baggage() {
        let fixtures: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../fixtures/observability/v1-records.json"
        ))
        .unwrap();
        let parent = asc_observability::bind_trace_context_input(
            &Context::new(),
            &json!({
                "sessionId": "inherited", "runId": "inherited", "callId": "stale",
                "toolCallId": "stale", "agentName": "codex", "trace_id": "opaque"
            }),
        )
        .unwrap();
        let _guard = parent.attach();
        let command = RecordCommand {
            format: "json".into(),
            stdin: true,
        };
        for case in fixtures {
            let request = command
                .request(serde_json::to_vec(&case["input"]).unwrap().as_slice())
                .unwrap();
            assert_eq!(request.method, "obs.record");
            let mut expected = case["expected"].clone();
            expected.as_object_mut().unwrap().remove("metadata");
            assert_eq!(request.params, expected);
            let context =
                asc_observability::extract_parent(&request.trace_context.unwrap().headers());
            let _child = context.attach();
            let snapshot = asc_observability::snapshot();
            assert_eq!(snapshot.agent["session_id"], "  session,一=1%  ");
            assert_eq!(snapshot.agent["agent_name"], "codex");
            for (snake, camel) in [("call_id", "callId"), ("tool_call_id", "toolCallId")] {
                assert_eq!(
                    snapshot.agent.get(snake).map(String::as_str),
                    case["expected"]["metadata"][camel].as_str()
                );
            }
            assert_eq!(
                request.compatibility.unwrap().trace_id.as_deref(),
                Some("opaque")
            );
        }
        assert_eq!(
            asc_observability::snapshot().agent["session_id"],
            "inherited"
        );
    }

    #[test]
    fn cli_normalizes_v1_timestamp_inputs_before_the_wire_boundary() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../fixtures/observability/v1-timestamps.json"
        ))
        .unwrap();
        let command = RecordCommand {
            format: "json".into(),
            stdin: true,
        };
        for case in cases {
            let input = serde_json::to_vec(&json!({
                "hook": "before_agent_run", "observedAt": case["input"],
                "metadata": {"sessionId": "s", "runId": "r"}, "metrics": {"user_input": "x"}
            }))
            .unwrap();
            let actual = command
                .request(input.as_slice())
                .ok()
                .map(|request| request.params["observedAt"].clone());
            assert_eq!(
                actual,
                (!case["expected"].is_null()).then(|| case["expected"].clone()),
                "input: {}",
                case["input"]
            );
        }
    }

    #[test]
    fn input_errors_precede_any_daemon_call() {
        for (format, stdin, input, message) in [
            ("jsonl", true, "", "--format must be json."),
            ("json", false, "", "--stdin is required."),
            ("json", true, "  ", "stdin is empty"),
            ("json", true, "{", "invalid JSON"),
            ("json", true, "[]", "payload must be a JSON object"),
        ] {
            let command = RecordCommand {
                format: format.into(),
                stdin,
            };
            assert_eq!(
                command.request(input.as_bytes()).unwrap_err().to_string(),
                format!("Error: {message}")
            );
        }
    }
}
