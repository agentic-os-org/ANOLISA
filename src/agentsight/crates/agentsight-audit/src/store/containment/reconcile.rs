//! Compare-and-swap transitions used by containment reconciliation workers.

use rusqlite::{Connection, params};

use super::super::{AuditError, AuditStore, sqlite_time};
use super::{ACTION_COLUMNS, ContainmentRow, containment_action_from_row, containment_row};
use crate::ContainmentAction;
#[cfg(target_os = "linux")]
use crate::{ContainmentFailureStage, ContainmentLifecycle};

#[cfg(target_os = "linux")]
const RECONCILE_CLAIM_LEASE_NS: u64 = 1_000_000_000;

/// One bounded due-row decode outcome.
#[cfg(target_os = "linux")]
pub enum DueContainmentAction {
    /// A valid action ready for claim acquisition.
    Valid(Box<ContainmentAction>),
    /// A malformed row that must be quarantined by its raw database key.
    Corrupt {
        /// Raw primary key retained even when it is not a valid UUID.
        action_key: String,
        /// Typed decode failure to sanitize before persistence.
        reason: String,
    },
}

impl AuditStore {
    /// Lists a bounded batch using lifecycle-specific expiry and retry eligibility.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, stored-data, or lock error.
    pub fn due_containment_actions(
        &self,
        now_ns: u64,
        limit: usize,
    ) -> Result<Vec<ContainmentAction>, AuditError> {
        let conn = self.connection()?;
        due_containment_rows(&conn, now_ns, limit)?
            .into_iter()
            .map(containment_action_from_row)
            .collect()
    }

    /// Returns per-row decode outcomes so one malformed row cannot abort a batch.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    #[cfg(target_os = "linux")]
    pub fn due_containment_candidates(
        &self,
        now_ns: u64,
        limit: usize,
    ) -> Result<Vec<DueContainmentAction>, AuditError> {
        let conn = self.connection()?;
        let rows = due_containment_rows(&conn, now_ns, limit)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let action_key = row.0.clone();
                match containment_action_from_row(row) {
                    Ok(action) => DueContainmentAction::Valid(Box::new(action)),
                    Err(error) => DueContainmentAction::Corrupt {
                        action_key,
                        reason: error.to_string(),
                    },
                }
            })
            .collect())
    }

    /// Quarantines one malformed row so it cannot recur or starve later work.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, stored-data, or lock error.
    #[cfg(target_os = "linux")]
    pub fn quarantine_containment_action(
        &self,
        action_key: &str,
        reason: &str,
        now_ns: u64,
    ) -> Result<(), AuditError> {
        let reason = sanitize_corrupt_reason(reason);
        let changed = self.connection()?.execute(
            "UPDATE containment_actions
             SET lifecycle_state = 'failed', failure_stage = 'reconcile',
                 failure_reason = ?1, next_retry_at_ns = NULL,
                 updated_at_ns = MAX(updated_at_ns, ?2)
             WHERE action_id = ?3",
            params![reason, sqlite_time(now_ns)?, action_key],
        )?;
        if changed != 1 {
            return Err(AuditError::InvalidData(format!(
                "corrupt containment action '{action_key}' disappeared during quarantine"
            )));
        }
        Ok(())
    }

    /// Claims one due action without allowing stale coordinators to duplicate work.
    ///
    /// Active actions become durably expiring in the same compare-and-swap.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    #[cfg(target_os = "linux")]
    pub fn claim_containment_reconciliation(
        &self,
        action: &ContainmentAction,
        now_ns: u64,
    ) -> Result<Option<ContainmentAction>, AuditError> {
        let mut claimed = action.clone();
        let claimed_at_ns = now_ns.max(action.updated_at_ns.saturating_add(1));
        claimed.updated_at_ns = claimed_at_ns;
        claimed.next_retry_at_ns = Some(claimed_at_ns.saturating_add(RECONCILE_CLAIM_LEASE_NS));
        if claimed.lifecycle_state == ContainmentLifecycle::Active {
            claimed.lifecycle_state = ContainmentLifecycle::Expiring;
        }
        let changed = self.connection()?.execute(
            "UPDATE containment_actions
             SET lifecycle_state = ?1, next_retry_at_ns = ?2, updated_at_ns = ?3
             WHERE action_id = ?4 AND lifecycle_state = ?5 AND updated_at_ns = ?6
               AND ((lifecycle_state = 'pending'
                     AND next_retry_at_ns IS NOT NULL
                     AND next_retry_at_ns <= ?7)
                    OR (lifecycle_state = 'active'
                        AND duration_secs IS NOT NULL
                        AND expires_at_ns IS NOT NULL
                        AND expires_at_ns <= ?7)
                    OR (lifecycle_state = 'expiring' AND (
                        (next_retry_at_ns IS NOT NULL AND next_retry_at_ns <= ?7)
                        OR (next_retry_at_ns IS NULL
                            AND duration_secs IS NOT NULL
                            AND expires_at_ns IS NOT NULL
                            AND expires_at_ns <= ?7))))",
            params![
                lifecycle_value(claimed.lifecycle_state),
                sqlite_time(claimed.next_retry_at_ns.unwrap_or(claimed_at_ns))?,
                sqlite_time(claimed_at_ns)?,
                action.action_id.to_string(),
                lifecycle_value(action.lifecycle_state),
                sqlite_time(action.updated_at_ns)?,
                sqlite_time(now_ns)?,
            ],
        )?;
        Ok((changed == 1).then_some(claimed))
    }

    /// Finishes lifecycle mutation only for the worker holding the latest claim.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, unsigned-value, or lock error.
    #[cfg(target_os = "linux")]
    pub fn finish_containment_reconciliation(
        &self,
        action: &ContainmentAction,
        claimed_lifecycle: ContainmentLifecycle,
        claimed_at_ns: u64,
    ) -> Result<bool, AuditError> {
        let changed = self.connection()?.execute(
            "UPDATE containment_actions SET
                 lifecycle_state = ?1,
                 failure_stage = ?2,
                 failure_reason = ?3,
                 attempt_count = ?4,
                 next_retry_at_ns = ?5,
                 updated_at_ns = ?6
             WHERE action_id = ?7 AND lifecycle_state = ?8 AND updated_at_ns = ?9",
            params![
                lifecycle_value(action.lifecycle_state),
                action.failure_stage.map(super::failure_stage_value),
                action.failure_reason,
                i64::from(action.attempt_count),
                action.next_retry_at_ns.map(sqlite_time).transpose()?,
                sqlite_time(action.updated_at_ns)?,
                action.action_id.to_string(),
                lifecycle_value(claimed_lifecycle),
                sqlite_time(claimed_at_ns)?,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Moves an exact claim to an exclusive cleanup lease before detachment.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    #[cfg(target_os = "linux")]
    pub fn begin_containment_cleanup(
        &self,
        action: &ContainmentAction,
        claimed_lifecycle: ContainmentLifecycle,
        claimed_at_ns: u64,
        now_ns: u64,
        reason: String,
    ) -> Result<Option<ContainmentAction>, AuditError> {
        let mut cleanup = action.clone();
        let cleanup_claim_ns = now_ns.max(claimed_at_ns.saturating_add(1));
        cleanup.lifecycle_state = ContainmentLifecycle::Expiring;
        cleanup.failure_stage = Some(ContainmentFailureStage::Detach);
        cleanup.failure_reason = Some(reason);
        cleanup.next_retry_at_ns = Some(cleanup_claim_ns.saturating_add(RECONCILE_CLAIM_LEASE_NS));
        cleanup.updated_at_ns = cleanup_claim_ns;
        let changed = self.connection()?.execute(
            "UPDATE containment_actions SET
                 lifecycle_state = 'expiring', failure_stage = 'detach',
                 failure_reason = ?1, next_retry_at_ns = ?2, updated_at_ns = ?3
             WHERE action_id = ?4 AND lifecycle_state = ?5 AND updated_at_ns = ?6",
            params![
                cleanup.failure_reason,
                sqlite_time(cleanup.next_retry_at_ns.unwrap_or(cleanup_claim_ns))?,
                sqlite_time(cleanup_claim_ns)?,
                action.action_id.to_string(),
                lifecycle_value(claimed_lifecycle),
                sqlite_time(claimed_at_ns)?,
            ],
        )?;
        Ok((changed == 1).then_some(cleanup))
    }
}

fn due_containment_rows(
    conn: &Connection,
    now_ns: u64,
    limit: usize,
) -> Result<Vec<ContainmentRow>, AuditError> {
    let limit = limit.clamp(1, 1_000);
    let mut statement = conn.prepare(&format!(
        "SELECT {ACTION_COLUMNS}
         FROM containment_actions
         WHERE (lifecycle_state = 'pending'
                AND next_retry_at_ns IS NOT NULL
                AND next_retry_at_ns <= ?1)
            OR (lifecycle_state = 'active'
                AND duration_secs IS NOT NULL
                AND expires_at_ns IS NOT NULL
                AND expires_at_ns <= ?1)
            OR (lifecycle_state = 'expiring' AND (
                (next_retry_at_ns IS NOT NULL AND next_retry_at_ns <= ?1)
                OR (next_retry_at_ns IS NULL
                    AND duration_secs IS NOT NULL
                    AND expires_at_ns IS NOT NULL
                    AND expires_at_ns <= ?1)))
            OR (lifecycle_state NOT IN ('pending', 'active', 'expiring', 'expired', 'failed')
                AND ((duration_secs IS NOT NULL
                      AND expires_at_ns IS NOT NULL
                      AND expires_at_ns <= ?1)
                     OR (next_retry_at_ns IS NOT NULL AND next_retry_at_ns <= ?1)))
         ORDER BY COALESCE(next_retry_at_ns, expires_at_ns, created_at_ns) ASC,
                  action_id ASC
         LIMIT ?2"
    ))?;
    let rows = statement.query_map(
        params![sqlite_time(now_ns)?, i64::try_from(limit).unwrap_or(1_000)],
        containment_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[cfg(target_os = "linux")]
fn sanitize_corrupt_reason(reason: &str) -> String {
    let sanitized: String = reason
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(384)
        .collect();
    format!("invalid persisted containment action: {}", sanitized.trim())
}

#[cfg(target_os = "linux")]
fn lifecycle_value(value: ContainmentLifecycle) -> &'static str {
    match value {
        ContainmentLifecycle::Pending => "pending",
        ContainmentLifecycle::Active => "active",
        ContainmentLifecycle::Expiring => "expiring",
        ContainmentLifecycle::Expired => "expired",
        ContainmentLifecycle::Failed => "failed",
    }
}
