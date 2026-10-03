//! Query and risk-case types shared by storage, APIs, and correlation.

use agentsight_enforcement_protocol::SecurityEvent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bounded filters for security-event queries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventFilter {
    /// Inclusive lower occurrence-time bound.
    pub start_ns: Option<u64>,
    /// Inclusive upper occurrence-time bound.
    pub end_ns: Option<u64>,
    /// Exact serialized event discriminator.
    pub event_type: Option<String>,
    /// Exact normalized result such as `allowed` or `blocked`.
    pub result: Option<String>,
    /// Exact product policy identifier.
    pub policy_id: Option<String>,
    /// Exact product Agent identifier.
    pub agent_id: Option<String>,
    /// Exact AgentSight session identifier.
    pub session_id: Option<String>,
    /// Exact policy-binding identifier.
    pub binding_id: Option<Uuid>,
    /// Requested page size; storage clamps it to `1..=1000`.
    pub limit: usize,
    /// Non-negative row offset.
    pub offset: i64,
}

impl Default for AuditEventFilter {
    fn default() -> Self {
        Self {
            start_ns: None,
            end_ns: None,
            event_type: None,
            result: None,
            policy_id: None,
            agent_id: None,
            session_id: None,
            binding_id: None,
            limit: 100,
            offset: 0,
        }
    }
}

/// One stable page of immutable security events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventPage {
    /// Events ordered by occurrence time descending.
    pub items: Vec<SecurityEvent>,
    /// Total matching events before pagination.
    pub total: u64,
    /// Effective clamped page size.
    pub limit: usize,
    /// Effective non-negative offset.
    pub offset: i64,
}

/// Aggregated security activity for one AgentSight session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSession {
    /// Stable AgentSight session identifier.
    pub session_id: String,
    /// Earliest matching event occurrence time.
    pub first_seen_ns: u64,
    /// Latest matching event occurrence time.
    pub last_seen_ns: u64,
    /// Number of matching normalized security events.
    pub security_event_count: u64,
}

/// One page of sessions grouped over the complete filtered event set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSessionPage {
    /// Sessions ordered by latest matching event descending.
    pub items: Vec<AuditSession>,
    /// Total matching sessions before pagination.
    pub total: u64,
    /// Effective clamped page size.
    pub limit: usize,
    /// Effective non-negative session offset.
    pub offset: i64,
}

/// One grouping key and its event count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditCountBy {
    /// Grouping value; absent database values are returned as `unknown`.
    pub key: String,
    /// Number of matching events.
    pub count: u64,
}

/// Aggregate security-event totals for the local store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSummary {
    /// Total immutable events.
    pub total_events: u64,
    /// Policy decisions with an observed kernel block.
    pub blocked_events: u64,
    /// Evidence-loss state events.
    pub evidence_loss_events: u64,
}

/// Aggregate correlated risk-case totals for dashboard summaries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskCaseSummary {
    /// Complete number of correlated cases.
    pub total: u64,
    /// Cases still awaiting human review.
    pub open: u64,
    /// Cases backed by a confirmed kernel denial.
    pub blocked: u64,
}

/// Review state for one correlated risk case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskCaseStatus {
    /// Awaiting review.
    Open,
    /// Confirmed as a real risk.
    Confirmed,
    /// Reviewed as a false positive.
    FalsePositive,
    /// Explicitly accepted without changing policy.
    AcceptedRisk,
    /// Remediated and closed.
    Resolved,
}

/// Product-facing case priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskSeverity {
    /// Informational evidence without an active risk.
    Low,
    /// Trusted or lower-confidence match requiring review.
    Medium,
    /// Unknown-public exfiltration match.
    High,
    /// High-risk match with confirmed kernel denial.
    Critical,
}

/// Stored risk-case summary used by list queries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskCase {
    /// Stable case identifier.
    pub case_id: Uuid,
    /// Idempotent correlation key.
    pub correlation_key: String,
    /// Product policy identifier.
    pub policy_id: String,
    /// Immutable policy revision.
    pub policy_revision: u64,
    /// Product Agent identifier.
    pub agent_id: String,
    /// Optional AgentSight session identifier.
    pub session_id: Option<String>,
    /// Product-facing priority.
    pub severity: RiskSeverity,
    /// Deterministic prioritization score.
    pub risk_score: u8,
    /// Current review state.
    pub status: RiskCaseStatus,
    /// Whether the kernel confirmed denial.
    pub blocked: bool,
    /// First evidence occurrence time.
    pub opened_at_ns: u64,
    /// Most recent case update time.
    pub updated_at_ns: u64,
    /// Minimized explanation suitable for list views.
    pub summary: String,
}

/// Expanded case with immutable evidence in occurrence order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskCaseDetail {
    /// Stored case summary.
    #[serde(flatten)]
    pub case: RiskCase,
    /// Linked evidence ordered by position.
    pub evidence: Vec<SecurityEvent>,
}

/// Persisted lifecycle of one containment request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainmentLifecycle {
    /// Persisted but not yet acknowledged by the enforcer.
    Pending,
    /// Enforcement is attached to the selected process tree.
    Active,
    /// Detachment is due, in progress, or awaiting retry.
    Expiring,
    /// Detachment was acknowledged by the enforcer.
    Expired,
    /// The current lifecycle operation failed terminally.
    Failed,
}

/// Operation stage associated with a containment failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainmentFailureStage {
    /// Applying the enforcement binding failed.
    Attach,
    /// Detaching the enforcement binding failed.
    Detach,
    /// Restoring persisted state against the enforcer failed.
    Reconcile,
}

/// Durable record of one case containment request and its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentAction {
    /// Stable idempotency identifier for this request.
    pub action_id: Uuid,
    /// Audit case that initiated containment.
    pub case_id: Uuid,
    /// Enforcement binding created for this action.
    pub binding_id: Uuid,
    /// Exact audit binding replaced by this action, absent only for legacy rows.
    #[serde(default)]
    pub source_binding_id: Option<Uuid>,
    /// Product Agent identity used for correlation.
    pub agent_id: String,
    /// Selected process-tree root.
    pub root_pid: i32,
    /// Linux process start time used to reject PID reuse.
    pub process_start_time: u64,
    /// Canonical sensitive source path recovered from policy state.
    pub source_path: String,
    /// Temporary duration, or `None` for explicit persistent enforcement.
    pub duration_secs: Option<u64>,
    /// Detachment deadline for temporary actions.
    pub expires_at_ns: Option<u64>,
    /// Current containment lifecycle state.
    pub lifecycle_state: ContainmentLifecycle,
    /// First confirmed kernel denial for this binding.
    pub blocked_at_ns: Option<u64>,
    /// Authenticated principal or current dashboard-token identity.
    pub requested_by: String,
    /// Operation stage for the latest failure.
    pub failure_stage: Option<ContainmentFailureStage>,
    /// Sanitized actionable failure detail.
    pub failure_reason: Option<String>,
    /// Number of bounded reconciliation attempts.
    pub attempt_count: u32,
    /// Persisted deadline for the next reconciliation attempt.
    pub next_retry_at_ns: Option<u64>,
    /// Action creation timestamp.
    pub created_at_ns: u64,
    /// Most recent lifecycle update timestamp.
    pub updated_at_ns: u64,
}
