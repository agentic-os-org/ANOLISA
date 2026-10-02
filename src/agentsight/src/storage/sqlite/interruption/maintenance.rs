//! Retention and capacity maintenance for the interruption store.

use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, MaintenanceStatus, SizeBasis, SizePolicy, checkpoint_truncate,
    enforce_size_policy, measure_database, retention_cutoff_ns,
};
use rusqlite::params;

use super::InterruptionStore;

impl InterruptionStore {
    /// Purges interruption events older than `cutoff_ns`.
    pub fn purge_before(&self, cutoff_ns: i64) -> Result<usize, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("interruption store connection mutex poisoned: {error}"))?;
        let deleted = conn.execute(
            "DELETE FROM interruption_events WHERE occurred_at_ns < ?1",
            params![cutoff_ns],
        )?;
        Ok(deleted)
    }

    /// Applies age retention and trims oldest events to 90% of the size limit.
    ///
    /// A zero retention or size limit disables that portion of maintenance.
    /// No `VACUUM` is performed; freed pages remain available for reuse.
    pub fn purge_old_and_oversized(
        &self,
        retention_days: u64,
        max_db_size_mb: u64,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let mut deleted_by_age = self.purge_stale_process_exits(
            i64::try_from(now_ns)
                .unwrap_or(i64::MAX)
                .saturating_sub(Self::PROCESS_EXIT_TTL_SECS * 1_000_000_000),
        )?;
        if retention_days > 0 {
            let cutoff_ns = retention_cutoff_ns(now_ns, retention_days)?;
            deleted_by_age = deleted_by_age
                .saturating_add(self.purge_before(i64::try_from(cutoff_ns).unwrap_or(i64::MAX))?);
        }

        let limit_bytes = max_db_size_mb.saturating_mul(1024 * 1024);
        if (deleted_by_age > 0 || limit_bytes > 0)
            && self.checkpoint_outcome()? == CheckpointOutcome::Busy
        {
            log::warn!("interruption WAL checkpoint remained busy before size maintenance");
            return Ok(deleted_by_age);
        }
        let report = enforce_size_policy::<Box<dyn std::error::Error>>(
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
            |fraction| self.delete_oldest_fraction(fraction),
            || self.checkpoint_outcome(),
        )?;

        if report.status == MaintenanceStatus::CheckpointBusy {
            log::warn!(
                "interruption size maintenance stopped at a busy checkpoint after deleting {} rows",
                report.deleted_rows
            );
        }

        Ok(deleted_by_age.saturating_add(report.deleted_rows))
    }

    fn purge_stale_process_exits(
        &self,
        cutoff_ns: i64,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("interruption store connection mutex poisoned: {error}"))?;
        Ok(conn.execute(
            "DELETE FROM process_exits WHERE exited_at_ns < ?1",
            params![cutoff_ns],
        )?)
    }

    fn delete_oldest_fraction(&self, fraction: f64) -> Result<usize, Box<dyn std::error::Error>> {
        if fraction <= 0.0 {
            return Ok(0);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("interruption store connection mutex poisoned: {error}"))?;
        let event_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM interruption_events", [], |row| {
                row.get(0)
            })?;
        if event_count == 0 {
            return Ok(0);
        }
        let batch = ((event_count as f64 * fraction.clamp(0.0, 1.0)) as i64).max(1);
        Ok(conn.execute(
            "DELETE FROM interruption_events WHERE id IN (
                SELECT id FROM interruption_events ORDER BY occurred_at_ns ASC, id ASC LIMIT ?1
            )",
            params![batch],
        )?)
    }

    fn size_snapshot(
        &self,
    ) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("interruption store connection mutex poisoned: {error}"))?;
        measure_database(&self.db_path, &conn).map_err(Into::into)
    }

    #[cfg(test)]
    pub(super) fn total_db_file_size(&self) -> u64 {
        self.size_snapshot()
            .map(|snapshot| snapshot.physical_bytes)
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(super) fn effective_db_file_size(&self) -> Result<u64, Box<dyn std::error::Error>> {
        Ok(self.size_snapshot()?.logical_bytes)
    }

    #[cfg(test)]
    pub(super) fn row_count(&self) -> Result<usize, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("interruption store connection mutex poisoned: {error}"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM interruption_events", [], |row| {
            row.get(0)
        })?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    fn checkpoint_outcome(&self) -> Result<CheckpointOutcome, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("interruption store connection mutex poisoned: {error}"))?;
        checkpoint_truncate(&conn).map_err(Into::into)
    }

    #[cfg(test)]
    pub(super) fn checkpoint(&self) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(self.checkpoint_outcome()? == CheckpointOutcome::Busy)
    }
}
