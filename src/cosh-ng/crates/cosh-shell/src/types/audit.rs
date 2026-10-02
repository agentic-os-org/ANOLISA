//! Standalone Shell mirror of the version 1 audit wire contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Fixed schema identifier for unified audit records.
pub const AUDIT_EVENT_SCHEMA: &str = "cosh.audit.event";
/// Fixed schema version emitted by this Shell release.
pub const AUDIT_EVENT_SCHEMA_VERSION: u16 = 1;
/// Maximum serialized record size, including the trailing newline.
pub const MAX_AUDIT_RECORD_BYTES: usize = 64 * 1024;
/// Maximum payload accepted for an unknown future event.
pub const MAX_UNKNOWN_DATA_BYTES: usize = 4096;
/// Producer redaction policy version shared with the canonical contract.
pub const AUDIT_REDACTION_POLICY_VERSION: &str = "audit-redaction-v1";

/// Failure behavior for governed audit boundaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditMode {
    /// Continue and expose a degraded gap.
    #[default]
    BestEffort,
    /// Block Shell-owned governed execution when durability fails.
    Required,
}

/// Cross-runtime correlation fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// Component metadata on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditComponent {
    pub name: String,
    pub version: String,
}

/// Actor category responsible for an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActorKind {
    User,
    Agent,
    System,
    Other,
}

/// Minimal actor metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditActor {
    pub kind: AuditActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub euid: Option<u32>,
}

/// Stable outcome status values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcomeStatus {
    Started,
    Success,
    Failed,
    Cancelled,
    Allowed,
    Denied,
    Degraded,
    Recovered,
    Unknown,
}

/// Bounded event outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEventOutcome {
    pub status: AuditOutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default)]
    pub retryable: bool,
}

/// Minimal subject metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSubject {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Producer redaction result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditRedactionStatus {
    Clean,
    Redacted,
    Dropped,
    FailedClosed,
}

/// Redaction metadata without sensitive values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRedaction {
    pub policy_version: String,
    pub status: AuditRedactionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

impl AuditRedaction {
    /// Declares that no sensitive field was presented to the payload.
    pub fn clean() -> Self {
        Self {
            policy_version: AUDIT_REDACTION_POLICY_VERSION.to_string(),
            status: AuditRedactionStatus::Clean,
            fields: Vec::new(),
        }
    }

    /// Declares the public field paths the producer omitted or replaced.
    pub fn dropped(fields: &[&str]) -> Self {
        Self {
            policy_version: AUDIT_REDACTION_POLICY_VERSION.to_string(),
            status: AuditRedactionStatus::Dropped,
            fields: fields.iter().map(|field| (*field).to_string()).collect(),
        }
    }
}

/// Allowlisted Shell command payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditShellCommandData {
    pub program: String,
    pub command_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
}

/// Allowlisted Tool payload used for Shell-hosted execution boundaries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditToolData {
    pub tool_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
}

/// Allowlisted approval payload used only for Shell-owned actions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditApprovalData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_hash: Option<String>,
}

/// Allowlisted evidence-access payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEvidenceData {
    pub scheme: String,
    pub evidence_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_category: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditLifecycleData {
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    reason_code: Option<String>,
    #[serde(default)]
    retry_count: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditControlData {
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    count: Option<u64>,
    #[serde(default)]
    bytes: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
}

/// Version 1 event envelope mirrored without an internal workspace dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEventV1 {
    pub schema: String,
    pub schema_version: u16,
    pub event_id: String,
    pub event_type: String,
    #[serde(with = "rfc3339_millis")]
    pub occurred_at: DateTime<Utc>,
    #[serde(with = "rfc3339_millis")]
    pub observed_at: DateTime<Utc>,
    pub sequence: u64,
    pub component: AuditComponent,
    pub identity: AuditIdentity,
    pub actor: AuditActor,
    pub outcome: AuditEventOutcome,
    pub subject: AuditSubject,
    pub data: Value,
    pub redaction: AuditRedaction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_schema: Option<u16>,
}

mod rfc3339_millis {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(&value)
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}

impl AuditEventV1 {
    /// Builds a Shell event with writer-assigned fields left at their initial values.
    ///
    /// The caller owns `redaction`: only the producer that shaped `payload` knows
    /// which source fields it omitted, so this constructor must not infer a claim
    /// from the event type.
    pub fn shell<T: Serialize>(
        event_type: &str,
        identity: AuditIdentity,
        outcome: AuditEventOutcome,
        subject: AuditSubject,
        payload: &T,
        redaction: AuditRedaction,
    ) -> Result<Self, String> {
        let now = Utc::now();
        let data = serde_json::to_value(payload).map_err(|error| error.to_string())?;
        let event = Self {
            schema: AUDIT_EVENT_SCHEMA.to_string(),
            schema_version: AUDIT_EVENT_SCHEMA_VERSION,
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            occurred_at: now,
            observed_at: now,
            sequence: 0,
            component: AuditComponent {
                name: "cosh-shell".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            identity,
            actor: AuditActor {
                kind: AuditActorKind::User,
                uid: None,
                euid: None,
            },
            outcome,
            subject,
            data,
            redaction,
            legacy_schema: None,
        };
        event.validate()?;
        Ok(event)
    }

    /// Validates the bounded wire fields owned by the standalone Shell.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != AUDIT_EVENT_SCHEMA || self.schema_version != 1 {
            return Err("unsupported audit schema".to_string());
        }
        if self.event_id.is_empty() || self.event_id.len() > 128 {
            return Err("invalid audit event id".to_string());
        }
        if self.event_type.is_empty() || self.event_type.len() > 96 {
            return Err("invalid audit event type".to_string());
        }
        if !self.data.is_object() {
            return Err("audit data must be an object".to_string());
        }
        if serde_json::to_vec(&self.data)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_UNKNOWN_DATA_BYTES
        {
            return Err("unknown audit data exceeds limit".to_string());
        }
        let shape_valid = match self.event_type.as_str() {
            "session.started" | "session.ended" => {
                serde_json::from_value::<AuditLifecycleData>(self.data.clone()).is_ok()
            }
            "shell.command.started" | "shell.command.completed" | "shell.command.failed" => {
                serde_json::from_value::<AuditShellCommandData>(self.data.clone()).is_ok()
            }
            "approval.requested" | "approval.resolved" => {
                serde_json::from_value::<AuditApprovalData>(self.data.clone()).is_ok()
            }
            "tool.execution.started" => {
                serde_json::from_value::<AuditToolData>(self.data.clone()).is_ok()
            }
            "evidence.accessed" => {
                serde_json::from_value::<AuditEvidenceData>(self.data.clone()).is_ok()
            }
            "audit.degraded" | "audit.recovered" => {
                serde_json::from_value::<AuditControlData>(self.data.clone()).is_ok()
            }
            _ => true,
        };
        if !shape_valid {
            return Err("audit data does not match event payload schema".to_string());
        }
        if self.event_type.starts_with("shell.command.")
            && (self.identity.shell_session_id.is_none() || self.identity.command_id.is_none())
        {
            return Err("shell command audit identity is incomplete".to_string());
        }
        if self.event_type.starts_with("approval.")
            && (self.identity.run_id.is_none() || self.identity.request_id.is_none())
        {
            return Err("approval audit identity is incomplete".to_string());
        }
        if self.event_type.starts_with("tool.")
            && (self.identity.run_id.is_none() || self.identity.tool_use_id.is_none())
        {
            return Err("tool audit identity is incomplete".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_shaped_event_round_trips_through_shell_mirror() {
        let value = serde_json::json!({
            "schema": "cosh.audit.event",
            "schema_version": 1,
            "event_id": "event-1",
            "event_type": "provider.request.completed",
            "occurred_at": "2026-07-22T00:00:00.000Z",
            "observed_at": "2026-07-22T00:00:01.000Z",
            "sequence": 2,
            "component": {"name": "cosh-core", "version": "0.1.0"},
            "identity": {"run_id": "run-1", "request_id": "request-1"},
            "actor": {"kind": "agent"},
            "outcome": {"status": "success", "retryable": false},
            "subject": {"kind": "provider", "name": "fixture"},
            "data": {"provider": "fixture", "future_optional": 1},
            "redaction": {"policy_version": "audit-redaction-v1", "status": "clean"}
        });
        let event: AuditEventV1 = serde_json::from_value(value.clone()).expect("mirror fixture");
        event.validate().expect("valid fixture");
        assert_eq!(serde_json::to_value(event).expect("serialize"), value);
    }

    #[test]
    fn shell_command_requires_stable_command_identity() {
        let result = AuditEventV1::shell(
            "shell.command.started",
            AuditIdentity::default(),
            AuditEventOutcome {
                status: AuditOutcomeStatus::Started,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "shell_command".to_string(),
                name: None,
            },
            &AuditShellCommandData::default(),
            AuditRedaction::dropped(&["command"]),
        );
        assert!(result.is_err());
    }

    #[test]
    fn redaction_helpers_carry_the_shared_policy_version() {
        let clean = serde_json::to_value(AuditRedaction::clean()).expect("serialize clean");
        assert_eq!(
            clean,
            serde_json::json!({
                "policy_version": AUDIT_REDACTION_POLICY_VERSION,
                "status": "clean",
            }),
            "a clean claim must omit the empty field list"
        );
        let dropped =
            serde_json::to_value(AuditRedaction::dropped(&["command", "cwd"])).expect("serialize");
        assert_eq!(
            dropped,
            serde_json::json!({
                "policy_version": AUDIT_REDACTION_POLICY_VERSION,
                "status": "dropped",
                "fields": ["command", "cwd"],
            })
        );
    }

    #[test]
    fn shell_constructor_preserves_the_producer_redaction_claim() {
        let event = AuditEventV1::shell(
            "session.started",
            AuditIdentity {
                shell_session_id: Some("session-1".to_string()),
                ..AuditIdentity::default()
            },
            AuditEventOutcome {
                status: AuditOutcomeStatus::Started,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "session".to_string(),
                name: None,
            },
            &serde_json::json!({}),
            AuditRedaction::clean(),
        )
        .expect("session event");
        assert_eq!(event.redaction, AuditRedaction::clean());
    }
}
