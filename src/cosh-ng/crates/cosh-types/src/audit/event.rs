//! Types for the audit subsystem.
//!
//! See `docs/audit-design.md` for the full design. This module is the pure
//! type layer — no I/O, no policy evaluation, no log writing.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::validation::validate_token;

/// Stable schema identifier for version 1 audit events.
pub const AUDIT_EVENT_SCHEMA: &str = "cosh.audit.event";

/// Stable schema version for the first unified audit event contract.
pub const AUDIT_EVENT_SCHEMA_VERSION: u16 = 1;

/// Maximum serialized size of one version 1 record, including its newline.
pub const MAX_AUDIT_RECORD_BYTES: usize = 64 * 1024;

/// Maximum bytes retained for an unknown event payload.
pub const MAX_UNKNOWN_DATA_BYTES: usize = 4096;

// =====================================================================
// Action — structured input to the PDP.
// =====================================================================

/// A structured action submitted to the audit subsystem. Raw shell strings
/// must be parsed into an `Action` (with shell metacharacters rejected) by
/// `cosh_platform::audit::action`. Audit rules never match against
/// `raw` — it is preserved purely for log display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub subsystem: ActionSubsystem,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// Subsystem identifier. Serialized as a lowercase string. Unknown values
/// (forward-compatibility for new command domains) round-trip through
/// `Other(name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionSubsystem {
    Pkg,
    Svc,
    Checkpoint,
    Shell,
    Cosh,
    Other(String),
}

impl ActionSubsystem {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pkg => "pkg",
            Self::Svc => "svc",
            Self::Checkpoint => "checkpoint",
            Self::Shell => "shell",
            Self::Cosh => "cosh",
            Self::Other(s) => s.as_str(),
        }
    }

    /// Parse from a textual token (case-insensitive for known variants).
    pub fn from_token(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "pkg" => Self::Pkg,
            "svc" => Self::Svc,
            "checkpoint" => Self::Checkpoint,
            "shell" => Self::Shell,
            "cosh" => Self::Cosh,
            _ => Self::Other(s.to_string()),
        }
    }
}

impl Serialize for ActionSubsystem {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ActionSubsystem {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ok(Self::from_token(&s))
    }
}

// =====================================================================
// Decision — output of the PDP.
// =====================================================================

/// The decision produced by the PDP for a given Action under a given Policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub outcome: Outcome,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<String>,
    pub policy_version: String,
}

/// Three-state decision outcome.
///
/// `RequireApproval` is the third state — distinct from `Deny`. It exists to
/// model "safe to auto-run / needs human / never auto-run" without forcing
/// a binary collapse. PEPs can map `RequireApproval` to `Deny` in non-
/// interactive contexts, and to a confirmation prompt in interactive ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Allow,
    Deny,
    RequireApproval,
}

// =====================================================================
// Policy / Rule / Match — declarative ruleset.
// =====================================================================

/// A complete audit policy. The first matching rule wins; if none match,
/// `default` is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: String,
    pub default: Outcome,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub matches: Match,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Match conditions for a Rule. ALL specified fields must match for the rule
/// to fire (logical AND). A `Match` with all fields empty is rejected at
/// load time — see `Policy::from_toml_str`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsystem: Option<ActionSubsystem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<StringMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<StringMatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg: Vec<ArgMatch>,
}

impl Match {
    /// True when no field is set — such a rule would match every action and
    /// is therefore rejected at load time.
    pub fn is_empty(&self) -> bool {
        self.subsystem.is_none()
            && self.operation.is_none()
            && self.target.is_none()
            && self.arg.is_empty()
    }
}

/// String match. TOML syntax is uniform across all match fields:
/// - `field = "value"`              → `Exact`
/// - `field = { one_of = [...] }`   → `OneOf`
/// - `field = { glob = "ng*" }`     → `Glob` (only `*` and `?`)
///
/// No regex by design — see `docs/audit-design.md` §3.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringMatch {
    Exact(String),
    OneOf { one_of: Vec<String> },
    Glob { glob: String },
}

/// Match an entry in `Action.args`. An ArgMatch fires when the action has at
/// least one `(k, v)` pair where `key` matches `k` and (if specified) `value`
/// matches `v`. Multiple ArgMatch entries in a `Match` are combined with AND
/// (all must find a satisfying pair).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgMatch {
    pub key: StringMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<StringMatch>,
}

// =====================================================================
// LogEntry — append-only audit record.
// =====================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub user: String,
    pub uid: u32,
    pub euid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sudo_user: Option<String>,
    pub pid: u32,
    pub action: Action,
    pub decision: Decision,
    pub source: LogSource,
    pub redacted: bool,
}

/// Origin of an audit call — useful for filtering logs by caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LogSource {
    Cli,
    Tui { tool_name: String },
    External { caller: String },
}

// =====================================================================
// Unified audit event version 1.
// =====================================================================

/// Runtime behavior when an audit record cannot be durably persisted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditMode {
    /// Continue the action while exposing a visible degraded state.
    #[default]
    BestEffort,
    /// Block governed transitions that cannot be durably audited.
    Required,
}

/// Origin of one effective audit setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSettingSource {
    /// Built-in product default.
    #[default]
    Default,
    /// Host-wide `/etc/copilot-shell/config.toml` setting.
    System,
    /// Per-user `~/.copilot-shell/config.toml` setting.
    User,
    /// Environment override used only for the storage root.
    Environment,
}

/// Effective audit settings after system and user layering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSettings {
    /// Failure behavior for governed transitions.
    pub mode: AuditMode,
    /// Maximum age of closed segments.
    pub retention_days: u32,
    /// Maximum total bytes of closed and active segments.
    pub max_disk_bytes: u64,
    /// Source of the effective mode.
    pub mode_source: AuditSettingSource,
    /// Source of the effective age limit.
    pub retention_days_source: AuditSettingSource,
    /// Source of the effective disk limit.
    pub max_disk_bytes_source: AuditSettingSource,
}

impl Default for AuditSettings {
    fn default() -> Self {
        Self {
            mode: AuditMode::BestEffort,
            retention_days: 30,
            max_disk_bytes: 1024 * 1024 * 1024,
            mode_source: AuditSettingSource::Default,
            retention_days_source: AuditSettingSource::Default,
            max_disk_bytes_source: AuditSettingSource::Default,
        }
    }
}

/// Stable component identifier for audit ordering and filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditComponentName {
    /// Structured command-line client.
    CoshCli,
    /// Unified agent runtime.
    CoshCore,
    /// Interactive PTY shell.
    CoshShell,
    /// Bounded future component name preserved by readers.
    Other(String),
}

impl AuditComponentName {
    /// Returns the stable wire name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::CoshCli => "cosh-cli",
            Self::CoshCore => "cosh-core",
            Self::CoshShell => "cosh-shell",
            Self::Other(value) => value,
        }
    }

    /// Parses a wire name while preserving a bounded unknown value.
    pub fn parse(value: &str) -> Result<Self, AuditValidationError> {
        match value {
            "cosh-cli" => Ok(Self::CoshCli),
            "cosh-core" => Ok(Self::CoshCore),
            "cosh-shell" => Ok(Self::CoshShell),
            other => {
                validate_token("component.name", other, 64)?;
                Ok(Self::Other(other.to_string()))
            }
        }
    }
}

impl Serialize for AuditComponentName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AuditComponentName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Component metadata captured by an event producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditComponent {
    /// Stable component name.
    pub name: AuditComponentName,
    /// Product version that emitted the record.
    pub version: String,
}

/// Correlation identities shared across Core and Shell lifecycles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditIdentity {
    /// Installation-scoped opaque identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    /// Interactive Shell session identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_session_id: Option<String>,
    /// Provider session identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// One user-message execution identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// One model-turn identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Provider or approval request identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Provider Tool-use identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Native or foreground command identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// Kind of actor responsible for a semantic action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActorKind {
    /// Local interactive user.
    User,
    /// Agent runtime.
    Agent,
    /// Host system or scheduled maintenance.
    System,
    /// Bounded future actor category.
    Other,
}

/// Minimised actor metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditActor {
    /// Stable actor category.
    pub kind: AuditActorKind,
    /// Real user identifier when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Effective user identifier when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub euid: Option<u32>,
}

/// Terminal or intermediate event outcome category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcomeStatus {
    /// Lifecycle action began and has no terminal result yet.
    Started,
    /// Lifecycle action completed successfully.
    Success,
    /// Lifecycle action failed.
    Failed,
    /// Lifecycle action was cancelled.
    Cancelled,
    /// Policy or human decision allowed the action.
    Allowed,
    /// Policy or human decision denied the action.
    Denied,
    /// Audit subsystem is degraded.
    Degraded,
    /// Audit subsystem recovered durability.
    Recovered,
    /// Bounded future status.
    Unknown,
}

/// Bounded outcome metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEventOutcome {
    /// Stable result category.
    pub status: AuditOutcomeStatus,
    /// Bounded machine-readable result code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Whether retrying the semantic action may succeed.
    #[serde(default)]
    pub retryable: bool,
}

/// Minimised subject metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSubject {
    /// Stable subject category such as `tool` or `provider`.
    pub kind: String,
    /// Bounded canonical name when safe to persist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Result of the producer-side redaction policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditRedactionStatus {
    /// No sensitive field was presented to the typed producer.
    Clean,
    /// One or more fields were replaced.
    Redacted,
    /// One or more fields were omitted.
    Dropped,
    /// The producer failed closed before persisting unsafe content.
    FailedClosed,
}

/// Redaction metadata without secret values or secret-type labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRedaction {
    /// Stable producer redaction policy version.
    pub policy_version: String,
    /// Redaction outcome.
    pub status: AuditRedactionStatus,
    /// Bounded public field paths that were changed or omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

/// Known version 1 event names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownAuditEventType {
    /// Session writer initialized.
    SessionStarted,
    /// Session ended normally.
    SessionEnded,
    /// Model turn began.
    TurnStarted,
    /// Model turn completed.
    TurnCompleted,
    /// Model turn failed.
    TurnFailed,
    /// Provider request began.
    ProviderRequestStarted,
    /// Provider request completed.
    ProviderRequestCompleted,
    /// Provider request failed.
    ProviderRequestFailed,
    /// Provider request was cancelled.
    ProviderRequestCancelled,
    /// Policy decision was produced.
    PolicyDecision,
    /// Hook decision was produced.
    HookDecision,
    /// Tool use was requested.
    ToolRequested,
    /// Tool execution began.
    ToolExecutionStarted,
    /// Tool execution completed.
    ToolCompleted,
    /// Tool execution failed.
    ToolFailed,
    /// Tool execution was cancelled.
    ToolCancelled,
    /// Approval was requested.
    ApprovalRequested,
    /// Approval was resolved.
    ApprovalResolved,
    /// Shell command began.
    ShellCommandStarted,
    /// Shell command completed.
    ShellCommandCompleted,
    /// Shell command failed.
    ShellCommandFailed,
    /// Existing evidence was accessed.
    EvidenceAccessed,
    /// Audit persistence degraded.
    AuditDegraded,
    /// Audit persistence recovered.
    AuditRecovered,
    /// Retention removed closed segments.
    RetentionPruned,
    /// Redacted audit export was published.
    ExportCreated,
}

impl KnownAuditEventType {
    /// Returns the stable wire event name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session.started",
            Self::SessionEnded => "session.ended",
            Self::TurnStarted => "turn.started",
            Self::TurnCompleted => "turn.completed",
            Self::TurnFailed => "turn.failed",
            Self::ProviderRequestStarted => "provider.request.started",
            Self::ProviderRequestCompleted => "provider.request.completed",
            Self::ProviderRequestFailed => "provider.request.failed",
            Self::ProviderRequestCancelled => "provider.request.cancelled",
            Self::PolicyDecision => "policy.decision",
            Self::HookDecision => "hook.decision",
            Self::ToolRequested => "tool.requested",
            Self::ToolExecutionStarted => "tool.execution.started",
            Self::ToolCompleted => "tool.completed",
            Self::ToolFailed => "tool.failed",
            Self::ToolCancelled => "tool.cancelled",
            Self::ApprovalRequested => "approval.requested",
            Self::ApprovalResolved => "approval.resolved",
            Self::ShellCommandStarted => "shell.command.started",
            Self::ShellCommandCompleted => "shell.command.completed",
            Self::ShellCommandFailed => "shell.command.failed",
            Self::EvidenceAccessed => "evidence.accessed",
            Self::AuditDegraded => "audit.degraded",
            Self::AuditRecovered => "audit.recovered",
            Self::RetentionPruned => "retention.pruned",
            Self::ExportCreated => "export.created",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "session.started" => Self::SessionStarted,
            "session.ended" => Self::SessionEnded,
            "turn.started" => Self::TurnStarted,
            "turn.completed" => Self::TurnCompleted,
            "turn.failed" => Self::TurnFailed,
            "provider.request.started" => Self::ProviderRequestStarted,
            "provider.request.completed" => Self::ProviderRequestCompleted,
            "provider.request.failed" => Self::ProviderRequestFailed,
            "provider.request.cancelled" => Self::ProviderRequestCancelled,
            "policy.decision" => Self::PolicyDecision,
            "hook.decision" => Self::HookDecision,
            "tool.requested" => Self::ToolRequested,
            "tool.execution.started" => Self::ToolExecutionStarted,
            "tool.completed" => Self::ToolCompleted,
            "tool.failed" => Self::ToolFailed,
            "tool.cancelled" => Self::ToolCancelled,
            "approval.requested" => Self::ApprovalRequested,
            "approval.resolved" => Self::ApprovalResolved,
            "shell.command.started" => Self::ShellCommandStarted,
            "shell.command.completed" => Self::ShellCommandCompleted,
            "shell.command.failed" => Self::ShellCommandFailed,
            "evidence.accessed" => Self::EvidenceAccessed,
            "audit.degraded" => Self::AuditDegraded,
            "audit.recovered" => Self::AuditRecovered,
            "retention.pruned" => Self::RetentionPruned,
            "export.created" => Self::ExportCreated,
            _ => return None,
        })
    }
}

/// Known or bounded future event name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditEventType {
    /// Event understood by this version.
    Known(KnownAuditEventType),
    /// Future event retained by a bounded reader.
    Unknown(String),
}

impl AuditEventType {
    /// Parses a stable event name.
    pub fn parse(value: &str) -> Result<Self, AuditValidationError> {
        if let Some(known) = KnownAuditEventType::parse(value) {
            return Ok(Self::Known(known));
        }
        validate_token("event_type", value, 96)?;
        Ok(Self::Unknown(value.to_string()))
    }

    /// Returns the wire event name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(value) => value.as_str(),
            Self::Unknown(value) => value,
        }
    }
}

impl From<KnownAuditEventType> for AuditEventType {
    fn from(value: KnownAuditEventType) -> Self {
        Self::Known(value)
    }
}

impl Serialize for AuditEventType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AuditEventType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Shared allowlisted lifecycle payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditLifecycleData {
    /// Elapsed milliseconds for a completed lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Bounded machine-readable reason category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Retry ordinal for a provider or Tool lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
}

/// Allowlisted Provider request payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditProviderData {
    /// Canonical Provider identifier without endpoint information.
    pub provider: String,
    /// Canonical model identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Elapsed request milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Input token count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output token count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Bounded finish or error category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_category: Option<String>,
    /// Retry ordinal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
}

/// Allowlisted policy or Hook decision payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditDecisionData {
    /// Stable allow/deny/approval decision.
    pub decision: String,
    /// Bounded reason category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Policy or Hook contract version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    /// Decision latency in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Allowlisted Tool payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditToolData {
    /// Canonical Tool kind.
    pub tool_kind: String,
    /// Bounded input shape classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_shape: Option<String>,
    /// Salted input summary hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    /// Stable execution-path category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_path: Option<String>,
    /// Execution duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Bounded result category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_category: Option<String>,
    /// Captured output bytes before projection limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// Whether the referenced output was truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// Opaque evidence reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
}

/// Allowlisted approval payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditApprovalData {
    /// Stable risk category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    /// Stable assessment category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<String>,
    /// Stable approval decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// Bounded machine-readable reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Approval wait time in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// Salted preview hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_hash: Option<String>,
}

/// Allowlisted Shell command payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditShellCommandData {
    /// Redacted first program token.
    pub program: String,
    /// Salted command hash.
    pub command_hash: String,
    /// Salted cwd scope hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_hash: Option<String>,
    /// Process exit code when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Command duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Captured output byte count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// Whether output was truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// Opaque terminal evidence reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
}

/// Allowlisted evidence access payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEvidenceData {
    /// Opaque reference scheme.
    pub scheme: String,
    /// Stable evidence type.
    pub evidence_type: String,
    /// Bounded size category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_category: Option<String>,
    /// Bounded range category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_category: Option<String>,
}

/// Allowlisted audit-control payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditControlData {
    /// Bounded internal operation category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Bounded safe error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Number of affected records or segments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// Number of affected bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Stable reason category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Validation failure for a producer or bounded reader event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditValidationError {
    field: &'static str,
    reason: &'static str,
}

impl AuditValidationError {
    pub(super) fn new(field: &'static str, reason: &'static str) -> Self {
        Self { field, reason }
    }
}

impl std::fmt::Display for AuditValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid {}: {}", self.field, self.reason)
    }
}

impl std::error::Error for AuditValidationError {}

/// Canonical version 1 audit event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEventV1 {
    /// Fixed `cosh.audit.event` schema name.
    pub schema: String,
    /// Fixed schema version `1`.
    pub schema_version: u16,
    /// Globally unique event identifier.
    pub event_id: String,
    /// Known or bounded future event name.
    pub event_type: AuditEventType,
    /// Producer time in UTC.
    #[serde(with = "rfc3339_millis")]
    pub occurred_at: DateTime<Utc>,
    /// Writer observation time in UTC.
    #[serde(with = "rfc3339_millis")]
    pub observed_at: DateTime<Utc>,
    /// Monotonic sequence within one segment.
    pub sequence: u64,
    /// Producer component metadata.
    pub component: AuditComponent,
    /// Cross-process correlation identities.
    pub identity: AuditIdentity,
    /// Actor metadata.
    pub actor: AuditActor,
    /// Result category.
    pub outcome: AuditEventOutcome,
    /// Subject category and safe canonical name.
    pub subject: AuditSubject,
    /// Allowlisted typed producer payload serialized as an object.
    pub(super) data: Value,
    /// Producer redaction result.
    pub redaction: AuditRedaction,
    /// Set only for projected version 0 records.
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
    /// Builds a validated event from an allowlisted payload.
    ///
    /// # Errors
    ///
    /// Returns an error for missing identity minima, unsafe bounded strings,
    /// non-object payloads, or an oversized unknown payload.
    #[allow(clippy::too_many_arguments)]
    pub fn new<T: Serialize>(
        event_id: String,
        event_type: AuditEventType,
        occurred_at: DateTime<Utc>,
        observed_at: DateTime<Utc>,
        sequence: u64,
        component: AuditComponent,
        identity: AuditIdentity,
        actor: AuditActor,
        outcome: AuditEventOutcome,
        subject: AuditSubject,
        payload: &T,
        redaction: AuditRedaction,
    ) -> Result<Self, AuditValidationError> {
        let data = serde_json::to_value(payload)
            .map_err(|_| AuditValidationError::new("data", "serialization failed"))?;
        let event = Self {
            schema: AUDIT_EVENT_SCHEMA.to_string(),
            schema_version: AUDIT_EVENT_SCHEMA_VERSION,
            event_id,
            event_type,
            occurred_at,
            observed_at,
            sequence,
            component,
            identity,
            actor,
            outcome,
            subject,
            data,
            redaction,
            legacy_schema: None,
        };
        event.validate()?;
        Ok(event)
    }

    /// Returns the bounded producer payload.
    pub fn data(&self) -> &Value {
        &self.data
    }

    /// Replaces the sequence and observed timestamp assigned by a segment writer.
    pub fn assign_writer_fields(&mut self, sequence: u64, observed_at: DateTime<Utc>) {
        self.sequence = sequence;
        self.observed_at = observed_at;
    }
}
