//! SQLite reads and writes for durable containment lifecycle state.

mod evidence;
mod reconcile;

#[cfg(target_os = "linux")]
pub use reconcile::DueContainmentAction;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

use super::{AuditError, AuditStore, parse_status, parse_uuid, sqlite_time, unsigned};
use crate::{ContainmentAction, ContainmentFailureStage, ContainmentLifecycle, RiskCaseStatus};

const ACTION_COLUMNS: &str =
    "action_id, case_id, binding_id, source_binding_id, agent_id, root_pid,
    process_start_time, source_path, duration_secs, expires_at_ns, lifecycle_state,
    blocked_at_ns, requested_by, failure_stage, failure_reason, attempt_count,
    next_retry_at_ns, created_at_ns, updated_at_ns";

/// Result of atomically claiming the one live containment slot for a case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainmentClaimResult {
    /// The supplied pending action now owns the case-level claim.
    Claimed,
    /// Another pending, active, or expiring action already owns the claim.
    Existing(Box<ContainmentAction>),
    /// Human review made the case ineligible before the claim was persisted.
    CaseIneligible(RiskCaseStatus),
}

/// Result of atomically activating an action and confirming its case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentActivationResult {
    /// The action is active and an open case is confirmed.
    Activated,
    /// Human review made the case ineligible while enforcement was applying.
    CaseIneligible(RiskCaseStatus),
    /// Another worker replaced the supplied lifecycle claim.
    LostClaim,
}

impl AuditStore {
    /// Inserts one containment action, returning false when any constraint conflict is ignored.
    ///
    /// # Errors
    ///
    /// Returns a typed database, unsigned-value, or lock error.
    pub fn insert_containment_action(
        &self,
        action: &ContainmentAction,
    ) -> Result<bool, AuditError> {
        let conn = self.connection()?;
        let changed = insert_action(&conn, action)?;
        Ok(changed == 1)
    }

    /// Atomically claims the one pending/active/expiring action slot for a case.
    ///
    /// # Errors
    ///
    /// Returns a typed database, unsigned-value, case, stored-data, or lock error.
    pub fn claim_containment_action(
        &self,
        action: &ContainmentAction,
    ) -> Result<ContainmentClaimResult, AuditError> {
        if action.lifecycle_state != ContainmentLifecycle::Pending {
            return Err(AuditError::InvalidData(
                "a containment claim must start pending".into(),
            ));
        }
        if action.source_binding_id.is_none() {
            return Err(AuditError::InvalidData(
                "a new containment claim requires an exact source binding".into(),
            ));
        }
        let mut conn = self.connection()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status = transaction
            .query_row(
                "SELECT status FROM risk_cases WHERE case_id = ?1",
                [action.case_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(AuditError::MissingCase(action.case_id))?;
        let status = parse_status(&status)?;
        if !matches!(status, RiskCaseStatus::Open | RiskCaseStatus::Confirmed) {
            transaction.commit()?;
            return Ok(ContainmentClaimResult::CaseIneligible(status));
        }
        let changed = insert_action(&transaction, action)?;
        let result = if changed == 1 {
            ContainmentClaimResult::Claimed
        } else {
            let existing = live_action(&transaction, action.case_id)?.ok_or_else(|| {
                AuditError::InvalidData(format!(
                    "containment claim {} conflicted without a live case action",
                    action.action_id
                ))
            })?;
            ContainmentClaimResult::Existing(Box::new(existing))
        };
        transaction.commit()?;
        Ok(result)
    }

    /// Atomically activates a pending action and confirms an open case.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, case, stored-data, or lock error.
    pub fn activate_containment_action(
        &self,
        action_id: Uuid,
        claimed_at_ns: u64,
        updated_at_ns: u64,
    ) -> Result<ContainmentActivationResult, AuditError> {
        let updated_at_ns = updated_at_ns.max(claimed_at_ns.saturating_add(1));
        let claimed_at_ns = sqlite_time(claimed_at_ns)?;
        let updated_at_ns = sqlite_time(updated_at_ns)?;
        let mut conn = self.connection()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let action = transaction
            .query_row(
                "SELECT case_id, lifecycle_state, updated_at_ns
                 FROM containment_actions WHERE action_id = ?1",
                [action_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                AuditError::InvalidData(format!("containment action {action_id} does not exist"))
            })?;
        let case_id = parse_uuid(&action.0)?;
        if parse_lifecycle(&action.1)? != ContainmentLifecycle::Pending || action.2 != claimed_at_ns
        {
            transaction.commit()?;
            return Ok(ContainmentActivationResult::LostClaim);
        }
        let status = transaction
            .query_row(
                "SELECT status FROM risk_cases WHERE case_id = ?1",
                [action.0.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(AuditError::MissingCase(case_id))?;
        let status = parse_status(&status)?;
        if !matches!(status, RiskCaseStatus::Open | RiskCaseStatus::Confirmed) {
            transaction.commit()?;
            return Ok(ContainmentActivationResult::CaseIneligible(status));
        }
        let activated = transaction.execute(
            "UPDATE containment_actions
             SET lifecycle_state = 'active', failure_stage = NULL, failure_reason = NULL,
                 next_retry_at_ns = NULL, updated_at_ns = ?1
             WHERE action_id = ?2 AND lifecycle_state = 'pending' AND updated_at_ns = ?3",
            params![updated_at_ns, action_id.to_string(), claimed_at_ns],
        )?;
        if activated != 1 {
            transaction.commit()?;
            return Ok(ContainmentActivationResult::LostClaim);
        }
        resolve_case_after_activation(&transaction, &action.0, updated_at_ns)?;
        transaction.commit()?;
        Ok(ContainmentActivationResult::Activated)
    }

    /// Loads one containment action by its stable action ID.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn containment_action(
        &self,
        action_id: Uuid,
    ) -> Result<Option<ContainmentAction>, AuditError> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(&format!(
            "SELECT {ACTION_COLUMNS} FROM containment_actions WHERE action_id = ?1"
        ))?;
        statement
            .query_row([action_id.to_string()], containment_row)
            .optional()?
            .map(containment_action_from_row)
            .transpose()
    }

    /// Loads one containment action by its unique enforcement binding.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn containment_action_by_binding(
        &self,
        binding_id: Uuid,
    ) -> Result<Option<ContainmentAction>, AuditError> {
        self.connection()?
            .query_row(
                &format!(
                    "SELECT {ACTION_COLUMNS}
                     FROM containment_actions WHERE binding_id = ?1"
                ),
                [binding_id.to_string()],
                containment_row,
            )
            .optional()?
            .map(containment_action_from_row)
            .transpose()
    }

    /// Loads the newest containment action for one case.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn latest_containment_action(
        &self,
        case_id: Uuid,
    ) -> Result<Option<ContainmentAction>, AuditError> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(&format!(
            "SELECT {ACTION_COLUMNS}
             FROM containment_actions
             WHERE case_id = ?1
             ORDER BY created_at_ns DESC, action_id ASC
             LIMIT 1"
        ))?;
        statement
            .query_row([case_id.to_string()], containment_row)
            .optional()?
            .map(containment_action_from_row)
            .transpose()
    }

    /// Requests immediate reverse reconciliation for one live containment binding.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    pub fn request_containment_expiry(
        &self,
        binding_id: Uuid,
        current_time_ns: u64,
    ) -> Result<bool, AuditError> {
        let current_time_ns = sqlite_time(current_time_ns)?;
        let changed = self.connection()?.execute(
            "UPDATE containment_actions
             SET lifecycle_state = 'expiring',
                 failure_stage = NULL,
                 failure_reason = NULL,
                 next_retry_at_ns = ?1,
                 updated_at_ns = MAX(updated_at_ns + 1, ?1)
             WHERE binding_id = ?2
               AND lifecycle_state IN ('pending', 'active', 'expiring')",
            params![current_time_ns, binding_id.to_string()],
        )?;
        Ok(changed == 1)
    }

    /// Persists the current mutable lifecycle fields for an action.
    ///
    /// An existing first-block timestamp is never cleared or replaced.
    ///
    /// # Errors
    ///
    /// Returns a typed database, unsigned-value, or lock error.
    pub fn update_containment_action(
        &self,
        action: &ContainmentAction,
    ) -> Result<bool, AuditError> {
        let changed = self.connection()?.execute(
            "UPDATE containment_actions SET
                lifecycle_state = ?1,
                blocked_at_ns = COALESCE(blocked_at_ns, ?2),
                failure_stage = ?3,
                failure_reason = ?4,
                attempt_count = ?5,
                next_retry_at_ns = ?6,
                expires_at_ns = ?7,
                duration_secs = ?8,
                updated_at_ns = ?9
             WHERE action_id = ?10",
            params![
                lifecycle_value(action.lifecycle_state),
                action.blocked_at_ns.map(sqlite_time).transpose()?,
                action.failure_stage.map(failure_stage_value),
                action.failure_reason,
                i64::from(action.attempt_count),
                action.next_retry_at_ns.map(sqlite_time).transpose()?,
                action.expires_at_ns.map(sqlite_time).transpose()?,
                action.duration_secs.map(sqlite_time).transpose()?,
                sqlite_time(action.updated_at_ns)?,
                action.action_id.to_string(),
            ],
        )?;
        Ok(changed == 1)
    }

    /// Records the earliest confirmed kernel denial for a containment binding.
    ///
    /// Duplicate or reordered calls retain the smallest occurrence timestamp.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    pub fn mark_containment_blocked(
        &self,
        binding_id: Uuid,
        blocked_at_ns: u64,
    ) -> Result<bool, AuditError> {
        let blocked_at_ns = sqlite_time(blocked_at_ns)?;
        let changed = self.connection()?.execute(
            "UPDATE containment_actions
             SET blocked_at_ns = CASE
                     WHEN blocked_at_ns IS NULL OR blocked_at_ns > ?1 THEN ?1
                     ELSE blocked_at_ns
                 END,
                 updated_at_ns = MAX(updated_at_ns, ?1)
             WHERE binding_id = ?2",
            params![blocked_at_ns, binding_id.to_string()],
        )?;
        Ok(changed == 1)
    }
}

/// Marks a case resolved once its containment action becomes active.
///
/// Matching zero rows is an expected, idempotent outcome: the case has already
/// reached a terminal status (`resolved`, `false_positive`, or `accepted_risk`),
/// so activation cannot and must not change it. That situation is logged loudly
/// instead of being silently ignored, because it usually means containment was
/// activated against a case a human already triaged out of scope.
fn resolve_case_after_activation(
    conn: &Connection,
    case_id: &str,
    updated_at_ns: i64,
) -> Result<(), AuditError> {
    let resolved = conn.execute(
        "UPDATE risk_cases SET status = 'resolved', updated_at_ns = ?1
         WHERE case_id = ?2 AND status IN ('open', 'confirmed')",
        params![updated_at_ns, case_id],
    )?;
    if resolved == 0 {
        log::warn!(
            "containment activation for case {case_id} updated 0 risk_cases rows; \
             the case is likely already in a terminal state \
             (resolved/false_positive/accepted_risk), so containment activation \
             did not change its status"
        );
    }
    Ok(())
}

fn insert_action(conn: &Connection, action: &ContainmentAction) -> Result<usize, AuditError> {
    conn.execute(
        "INSERT INTO containment_actions (
            action_id, case_id, binding_id, source_binding_id, agent_id, root_pid, process_start_time,
            source_path, duration_secs, expires_at_ns, lifecycle_state, blocked_at_ns,
            requested_by, failure_stage, failure_reason, attempt_count, next_retry_at_ns,
            created_at_ns, updated_at_ns
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19
         ) ON CONFLICT DO NOTHING",
        params![
            action.action_id.to_string(),
            action.case_id.to_string(),
            action.binding_id.to_string(),
            action.source_binding_id.map(|id| id.to_string()),
            action.agent_id,
            i64::from(action.root_pid),
            sqlite_time(action.process_start_time)?,
            action.source_path,
            action.duration_secs.map(sqlite_time).transpose()?,
            action.expires_at_ns.map(sqlite_time).transpose()?,
            lifecycle_value(action.lifecycle_state),
            action.blocked_at_ns.map(sqlite_time).transpose()?,
            action.requested_by,
            action.failure_stage.map(failure_stage_value),
            action.failure_reason,
            i64::from(action.attempt_count),
            action.next_retry_at_ns.map(sqlite_time).transpose()?,
            sqlite_time(action.created_at_ns)?,
            sqlite_time(action.updated_at_ns)?,
        ],
    )
    .map_err(Into::into)
}

fn live_action(conn: &Connection, case_id: Uuid) -> Result<Option<ContainmentAction>, AuditError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ACTION_COLUMNS}
         FROM containment_actions
         WHERE case_id = ?1 AND lifecycle_state IN ('pending', 'active', 'expiring')
         LIMIT 1"
    ))?;
    statement
        .query_row([case_id.to_string()], containment_row)
        .optional()?
        .map(containment_action_from_row)
        .transpose()
}

type ContainmentRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    i32,
    i64,
    String,
    Option<i64>,
    Option<i64>,
    String,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<i64>,
    i64,
    i64,
);

fn containment_row(row: &Row<'_>) -> rusqlite::Result<ContainmentRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
    ))
}

fn containment_action_from_row(row: ContainmentRow) -> Result<ContainmentAction, AuditError> {
    Ok(ContainmentAction {
        action_id: parse_uuid(&row.0)?,
        case_id: parse_uuid(&row.1)?,
        binding_id: parse_uuid(&row.2)?,
        source_binding_id: row.3.as_deref().map(parse_uuid).transpose()?,
        agent_id: row.4,
        root_pid: row.5,
        process_start_time: unsigned(row.6, "process_start_time")?,
        source_path: row.7,
        duration_secs: row
            .8
            .map(|value| unsigned(value, "duration_secs"))
            .transpose()?,
        expires_at_ns: row
            .9
            .map(|value| unsigned(value, "expires_at_ns"))
            .transpose()?,
        lifecycle_state: parse_lifecycle(&row.10)?,
        blocked_at_ns: row
            .11
            .map(|value| unsigned(value, "blocked_at_ns"))
            .transpose()?,
        requested_by: row.12,
        failure_stage: row.13.as_deref().map(parse_failure_stage).transpose()?,
        failure_reason: row.14,
        attempt_count: u32::try_from(row.15)
            .map_err(|_| AuditError::InvalidData("attempt_count is out of range".into()))?,
        next_retry_at_ns: row
            .16
            .map(|value| unsigned(value, "next_retry_at_ns"))
            .transpose()?,
        created_at_ns: unsigned(row.17, "created_at_ns")?,
        updated_at_ns: unsigned(row.18, "updated_at_ns")?,
    })
}

fn lifecycle_value(value: ContainmentLifecycle) -> &'static str {
    match value {
        ContainmentLifecycle::Pending => "pending",
        ContainmentLifecycle::Active => "active",
        ContainmentLifecycle::Expiring => "expiring",
        ContainmentLifecycle::Expired => "expired",
        ContainmentLifecycle::Failed => "failed",
    }
}

fn parse_lifecycle(value: &str) -> Result<ContainmentLifecycle, AuditError> {
    match value {
        "pending" => Ok(ContainmentLifecycle::Pending),
        "active" => Ok(ContainmentLifecycle::Active),
        "expiring" => Ok(ContainmentLifecycle::Expiring),
        "expired" => Ok(ContainmentLifecycle::Expired),
        "failed" => Ok(ContainmentLifecycle::Failed),
        _ => Err(AuditError::InvalidData(format!(
            "unknown containment lifecycle '{value}'"
        ))),
    }
}

fn failure_stage_value(value: ContainmentFailureStage) -> &'static str {
    match value {
        ContainmentFailureStage::Attach => "attach",
        ContainmentFailureStage::Detach => "detach",
        ContainmentFailureStage::Reconcile => "reconcile",
    }
}

fn parse_failure_stage(value: &str) -> Result<ContainmentFailureStage, AuditError> {
    match value {
        "attach" => Ok(ContainmentFailureStage::Attach),
        "detach" => Ok(ContainmentFailureStage::Detach),
        "reconcile" => Ok(ContainmentFailureStage::Reconcile),
        _ => Err(AuditError::InvalidData(format!(
            "unknown containment failure stage '{value}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_case(conn: &Connection, case_id: &str, status: &str) {
        conn.execute(
            "INSERT INTO risk_cases (
                case_id, correlation_key, policy_id, policy_revision, agent_id,
                severity, risk_score, status, blocked, opened_at_ns, updated_at_ns, summary
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                case_id, case_id, "policy", 1_i64, "agent", "high", 10_i64, status, 0_i64, 1_i64,
                1_i64, "summary",
            ],
        )
        .unwrap();
    }

    fn case_status(conn: &Connection, case_id: &str) -> String {
        conn.query_row(
            "SELECT status FROM risk_cases WHERE case_id = ?1",
            [case_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    }

    #[test]
    fn resolve_case_after_activation_resolves_an_open_case() {
        let store = AuditStore::open_in_memory().unwrap();
        let conn = store.connection().unwrap();
        let case_id = Uuid::new_v4().to_string();
        insert_case(&conn, &case_id, "open");

        resolve_case_after_activation(&conn, &case_id, 2).unwrap();

        assert_eq!(case_status(&conn, &case_id), "resolved");
    }

    #[test]
    fn resolve_case_after_activation_ignores_zero_rows_for_terminal_case() {
        let store = AuditStore::open_in_memory().unwrap();
        let conn = store.connection().unwrap();
        let case_id = Uuid::new_v4().to_string();
        // A case already triaged out of scope must not be revived, and the
        // 0-row update must stay idempotent (warn, never error).
        insert_case(&conn, &case_id, "false_positive");

        let result = resolve_case_after_activation(&conn, &case_id, 5);

        assert!(result.is_ok());
        assert_eq!(case_status(&conn, &case_id), "false_positive");
    }
}
