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
    capacity_dirty: bool,
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
            let checkpoint = if cursor.capacity_dirty {
                self.checkpoint_passive()?
            } else {
                self.maintenance_checkpoint()?
            };
            if checkpoint == CheckpointOutcome::Completed {
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
            || self.live_bytes()? <= limit.saturating_mul(9) / 10
        {
            if cursor.capacity_dirty && self.maintenance_checkpoint()? == CheckpointOutcome::Busy {
                cursor.retry_after = Some(Instant::now() + Duration::from_secs(1));
                return Ok(false);
            }
            *cursor = MaintenanceCursor::default();
            return Ok(true);
        }
        let Some((table, count)) = self.prune_candidate()? else {
            if cursor.capacity_dirty && self.maintenance_checkpoint()? == CheckpointOutcome::Busy {
                cursor.retry_after = Some(Instant::now() + Duration::from_secs(1));
                return Ok(false);
            }
            *cursor = MaintenanceCursor::default();
            return Ok(true);
        };
        let deleted =
            self.delete_maintenance_batch(table, None, (count / 10).clamp(1, 128) as usize)?;
        // Reused WAL pages are not live data. Do not copy and truncate the
        // entire WAL after each deletion merely to measure the remaining rows.
        cursor.capacity_dirty |= deleted > 0;
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
    fn capacity_batches_measure_live_pages_without_truncating_after_every_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("genai.db");
        let store = GenAISqliteStore::new_with_path(
            &path,
            crate::config::PeriodicStoragePolicy {
                retention_days: 0,
                ..Default::default()
            },
        )
        .unwrap();
        store.use_managed_checkpoints().unwrap();
        for timestamp in 1..=8 {
            store
                .conn
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO genai_events(event_type,start_timestamp_ns,event_json)
                 VALUES('llm_call',?1,zeroblob(307200))",
                    [timestamp],
                )
                .unwrap();
        }
        let mut cursor = MaintenanceCursor::default();
        for _ in 0..5 {
            assert!(!store.maintain_step(1, &mut cursor).unwrap());
        }
        assert_eq!(cursor.phase, 5);
        assert!(!store.maintain_step(1, &mut cursor).unwrap());
        assert_eq!(cursor.phase, 4);
        assert!(cursor.capacity_dirty);
        assert!(store.size_snapshot().unwrap().wal_bytes > 0);
        assert!(!store.maintain_step(1, &mut cursor).unwrap());
        assert_eq!(cursor.phase, 5);
        assert!(store.size_snapshot().unwrap().wal_bytes > 0);
        assert!((0..32).any(|_| store.maintain_step(1, &mut cursor).unwrap()));
        assert!(store.live_bytes().unwrap() <= 1024 * 1024 * 9 / 10);
        assert_eq!(store.size_snapshot().unwrap().wal_bytes, 0);
        let newest: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT MAX(start_timestamp_ns) FROM genai_events",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(newest, 8);
    }

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

    #[test]
    fn pending_rows_still_finish_a_dirty_capacity_pass_with_truncated_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("genai.db");
        let store = GenAISqliteStore::new_with_path(
            &path,
            crate::config::PeriodicStoragePolicy {
                retention_days: 0,
                ..Default::default()
            },
        )
        .unwrap();
        store.use_managed_checkpoints().unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO genai_events(status,event_type,start_timestamp_ns,event_json)
                 VALUES ('complete','llm_call',1,zeroblob(2097152))",
                [],
            )
            .unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO genai_events(status,event_type,start_timestamp_ns,event_json)
                 VALUES ('pending','llm_call',2,zeroblob(2097152))",
                [],
            )
            .unwrap();

        let mut cursor = MaintenanceCursor::default();
        let mut finished = false;
        for _ in 0..32 {
            if store.maintain_step(1, &mut cursor).unwrap() {
                finished = true;
                break;
            }
        }
        assert!(finished);
        assert_eq!(store.size_snapshot().unwrap().wal_bytes, 0);
        assert_eq!(
            store
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM genai_events WHERE status = 'pending'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }
}
