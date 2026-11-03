//! Resumable retention and capacity work for CPU-paced capture runtimes.

use super::GenAISqliteStore;
use super::schema::PruneTable;
use agentsight_sqlite_lifecycle::{CheckpointOutcome, retention_cutoff_ns};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RETENTION_TABLES: [PruneTable; 4] = [
    PruneTable::Events,
    PruneTable::Resources,
    PruneTable::Evaluations,
    PruneTable::Tokenless,
];

/// A cleanup pass keeps its phase when yielding the worker's CPU permit.
#[derive(Default)]
pub(crate) struct MaintenanceCursor {
    phase: usize,
    cutoff: Option<i64>,
    retry_after: Option<Instant>,
}

impl GenAISqliteStore {
    /// Perform at most one bounded delete transaction or checkpoint per worker tick.
    pub(crate) fn maintain_step(
        &self,
        max_db_size_mb: u64,
        cursor: &mut MaintenanceCursor,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.capture_has_priority()
            || cursor
                .retry_after
                .is_some_and(|after| Instant::now() < after)
        {
            return Ok(false);
        }
        let _timer = crate::runtime_metrics::StageTimer::start("genai_maintenance_step");
        if cursor.phase == 0 && cursor.cutoff.is_none() && self.storage_policy.retention_days > 0 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_nanos()
                .min(u64::MAX as u128) as u64;
            cursor.cutoff = Some(
                retention_cutoff_ns(now, self.storage_policy.retention_days)?.min(i64::MAX as u64)
                    as i64,
            );
        }
        if cursor.phase < RETENTION_TABLES.len() {
            if let Some(cutoff) = cursor.cutoff {
                let table = RETENTION_TABLES[cursor.phase];
                if self.delete_maintenance_batch(table, Some(cutoff), 128)? > 0 {
                    return Ok(false);
                }
            }
            cursor.phase += 1;
            return Ok(false);
        }
        if cursor.phase == RETENTION_TABLES.len() {
            if self.maintenance_checkpoint()? == CheckpointOutcome::Completed {
                cursor.phase += 1;
                cursor.retry_after = None;
            } else {
                cursor.retry_after = Some(Instant::now() + Duration::from_secs(1));
            }
            return Ok(false);
        }
        let limit = max_db_size_mb.saturating_mul(1024 * 1024);
        let snapshot = self.size_snapshot()?;
        if limit == 0
            || snapshot.physical_bytes <= limit
            || snapshot.logical_bytes <= limit.saturating_mul(9) / 10
        {
            *cursor = MaintenanceCursor::default();
            return Ok(true);
        }
        let Some((table, count)) = self.prune_candidate()? else {
            *cursor = MaintenanceCursor::default();
            return Ok(true);
        };
        let deleted =
            self.delete_maintenance_batch(table, None, (count / 10).clamp(1, 128) as usize)?;
        cursor.phase = RETENTION_TABLES.len();
        if deleted == 0 && !self.capture_has_priority() {
            log::debug!("GenAI capacity cleanup yielded without eligible rows");
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_reader_defers_checkpoint_retries_without_losing_the_pass() {
        let dir = std::env::temp_dir().join(format!("checkpoint-backoff-{}", uuid::Uuid::new_v4()));
        let path = dir.join("genai.db");
        let store = GenAISqliteStore::new_with_path(
            &path,
            crate::config::PeriodicStoragePolicy {
                retention_days: 0,
                ..Default::default()
            },
        )
        .unwrap();
        let reader = rusqlite::Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT COUNT(*) FROM agent_resource_samples;")
            .unwrap();
        store.conn.lock().unwrap().execute_batch("INSERT INTO agent_resource_samples(timestamp_ns,pid,cpu_percent,memory_bytes) VALUES (1,1,0,0)").unwrap();
        let mut cursor = MaintenanceCursor::default();
        for _ in 0..5 {
            assert!(!store.maintain_step(0, &mut cursor).unwrap());
        }
        assert_eq!(cursor.phase, RETENTION_TABLES.len());
        assert!(cursor.retry_after.is_some());
        reader.execute_batch("COMMIT").unwrap();
        assert!(!store.maintain_step(0, &mut cursor).unwrap());
        assert_eq!(cursor.phase, RETENTION_TABLES.len());
        cursor.retry_after = Some(Instant::now() - Duration::from_secs(1));
        assert!(!store.maintain_step(0, &mut cursor).unwrap());
        assert!(store.maintain_step(0, &mut cursor).unwrap());
        drop(reader);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
