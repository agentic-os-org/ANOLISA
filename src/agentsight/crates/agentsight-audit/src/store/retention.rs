//! Transactional retention for complete security case graphs.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, MaintenanceReport, MaintenanceStatus, SizeBasis, SizePolicy,
    checkpoint_truncate, enforce_size_policy, measure_database, retention_cutoff_ns,
};
use rusqlite::{TransactionBehavior, params};

use super::{AuditError, AuditStore, sqlite_time};

const PURGEABLE_CASES: &str = "SELECT cases.case_id
     FROM risk_cases AS cases
     WHERE cases.updated_at_ns < ?1
       AND cases.status IN ('false_positive', 'accepted_risk', 'resolved')
       AND NOT EXISTS (
           SELECT 1
           FROM containment_actions AS actions
           WHERE actions.case_id = cases.case_id
             AND (
                 actions.lifecycle_state IN ('pending', 'active', 'expiring')
                 OR actions.updated_at_ns >= ?1
             )
       )";

/// Retention and capacity limits for security audit data.
#[derive(Debug, Clone, Copy, Default)]
pub struct AuditMaintenancePolicy {
    /// Maximum evidence age in days; zero disables age retention.
    pub retention_days: u64,
    /// Maximum logical database size in MiB; zero disables size maintenance.
    pub max_db_size_mb: u64,
}

/// Result of one graph-safe security audit maintenance pass.
#[derive(Debug, Clone, Copy)]
pub struct AuditMaintenanceReport {
    /// Graph rows removed by age retention.
    pub expired_rows: u64,
    /// Result of capacity enforcement.
    pub size: MaintenanceReport,
}

impl AuditStore {
    /// Deletes expired events and complete inactive case graphs before `cutoff_ns`.
    ///
    /// Evidence shared with a retained case and every graph with live containment
    /// remain intact. The returned count includes every deleted graph row.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    pub fn purge_before(&self, cutoff_ns: u64) -> Result<u64, AuditError> {
        let cutoff_ns = sqlite_time(cutoff_ns)?;
        let mut conn = self.connection()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut deleted = 0_u64;

        deleted += transaction.execute(
            &format!("DELETE FROM containment_actions WHERE case_id IN ({PURGEABLE_CASES})"),
            params![cutoff_ns],
        )? as u64;
        deleted += transaction.execute(
            &format!("DELETE FROM risk_evidence_links WHERE case_id IN ({PURGEABLE_CASES})"),
            params![cutoff_ns],
        )? as u64;
        deleted += transaction.execute(
            &format!("DELETE FROM risk_cases WHERE case_id IN ({PURGEABLE_CASES})"),
            params![cutoff_ns],
        )? as u64;
        deleted += transaction.execute(
            "DELETE FROM security_events
             WHERE occurred_at_ns < ?1
               AND NOT EXISTS (
                   SELECT 1 FROM risk_evidence_links AS links
                   WHERE links.event_id = security_events.event_id
               )",
            params![cutoff_ns],
        )? as u64;
        deleted += delete_unreferenced_policy_revisions(&transaction)? as u64;

        transaction.commit()?;
        Ok(deleted)
    }

    /// Applies graph-safe age retention and capacity enforcement.
    ///
    /// # Errors
    ///
    /// Returns a typed lifecycle, database, timestamp, or lock error.
    pub fn maintain(
        &self,
        policy: AuditMaintenancePolicy,
    ) -> Result<AuditMaintenanceReport, AuditError> {
        let expired_rows = if policy.retention_days == 0 {
            0
        } else {
            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            self.purge_before(retention_cutoff_ns(now_ns, policy.retention_days)?)?
        };
        if expired_rows > 0 && self.checkpoint()? == CheckpointOutcome::Busy {
            let snapshot = self.size_snapshot()?;
            return Ok(AuditMaintenanceReport {
                expired_rows,
                size: MaintenanceReport {
                    status: MaintenanceStatus::CheckpointBusy,
                    rounds: 0,
                    deleted_rows: 0,
                    before: snapshot,
                    after: snapshot,
                },
            });
        }

        let limit_bytes = policy.max_db_size_mb.saturating_mul(1024 * 1024);
        let size = enforce_size_policy::<AuditError>(
            SizePolicy {
                limit_bytes,
                trigger_bytes: limit_bytes,
                target_bytes: limit_bytes.saturating_mul(9) / 10,
                trigger_basis: SizeBasis::Physical,
                target_basis: SizeBasis::Logical,
                max_rounds: 20,
                max_stalled_rounds: 3,
            },
            || self.size_snapshot(),
            |fraction| self.delete_oldest_graph_fraction(fraction),
            || self.checkpoint(),
        )?;
        Ok(AuditMaintenanceReport { expired_rows, size })
    }

    fn delete_oldest_graph_fraction(&self, fraction: f64) -> Result<usize, AuditError> {
        let mut conn = self.connection()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let case_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM risk_cases
             WHERE status IN ('false_positive', 'accepted_risk', 'resolved')
               AND NOT EXISTS (
                   SELECT 1 FROM containment_actions
                   WHERE containment_actions.case_id = risk_cases.case_id
                     AND lifecycle_state IN ('pending', 'active', 'expiring')
               )",
            [],
            |row| row.get(0),
        )?;
        let case_limit = fractional_count(case_count, fraction);
        let case_ids = {
            let mut statement = transaction.prepare(
                "SELECT case_id FROM risk_cases
                 WHERE status IN ('false_positive', 'accepted_risk', 'resolved')
                   AND NOT EXISTS (
                       SELECT 1 FROM containment_actions
                       WHERE containment_actions.case_id = risk_cases.case_id
                         AND lifecycle_state IN ('pending', 'active', 'expiring')
                   )
                 ORDER BY updated_at_ns ASC, case_id ASC LIMIT ?1",
            )?;
            statement
                .query_map(params![case_limit], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut deleted = 0usize;
        for case_id in case_ids {
            deleted += transaction.execute(
                "DELETE FROM containment_actions WHERE case_id = ?1",
                params![case_id],
            )?;
            deleted += transaction.execute(
                "DELETE FROM risk_evidence_links WHERE case_id = ?1",
                params![case_id],
            )?;
            deleted += transaction.execute(
                "DELETE FROM risk_cases WHERE case_id = ?1",
                params![case_id],
            )?;
        }

        let event_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM security_events AS events
             WHERE NOT EXISTS (
                 SELECT 1 FROM risk_evidence_links AS links
                 WHERE links.event_id = events.event_id
             )",
            [],
            |row| row.get(0),
        )?;
        let event_limit = fractional_count(event_count, fraction);
        deleted += transaction.execute(
            "DELETE FROM security_events WHERE event_id IN (
                SELECT events.event_id FROM security_events AS events
                WHERE NOT EXISTS (
                    SELECT 1 FROM risk_evidence_links AS links
                    WHERE links.event_id = events.event_id
                )
                ORDER BY events.occurred_at_ns ASC, events.event_id ASC LIMIT ?1
            )",
            params![event_limit],
        )?;
        deleted += delete_unreferenced_policy_revisions(&transaction)?;
        transaction.commit()?;
        Ok(deleted)
    }

    fn size_snapshot(&self) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot, AuditError> {
        let conn = self.connection()?;
        let path = conn
            .path()
            .map(PathBuf::from)
            .ok_or_else(|| AuditError::InvalidData("security database has no file path".into()))?;
        measure_database(&path, &conn).map_err(Into::into)
    }

    fn checkpoint(&self) -> Result<CheckpointOutcome, AuditError> {
        let conn = self.connection()?;
        checkpoint_truncate(&conn).map_err(Into::into)
    }
}

fn fractional_count(count: i64, fraction: f64) -> i64 {
    if count > 0 && fraction > 0.0 {
        ((count as f64 * fraction.clamp(0.0, 1.0)) as i64).max(1)
    } else {
        0
    }
}

fn delete_unreferenced_policy_revisions(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<usize, rusqlite::Error> {
    transaction.execute(
        "DELETE FROM policy_revisions AS revisions
         WHERE NOT EXISTS (
             SELECT 1 FROM security_events AS events
             WHERE events.policy_id = revisions.policy_id
               AND events.policy_revision = revisions.revision
         )
           AND NOT EXISTS (
             SELECT 1 FROM risk_cases AS cases
             WHERE cases.policy_id = revisions.policy_id
               AND cases.policy_revision = revisions.revision
         )",
        [],
    )
}
