//! Primary database cleanup yields between individual row deletions and checkpoints.

use super::*;

/// Resume a capacity pass without restarting age scans or draining the database at once.
#[derive(Default)]
pub(crate) struct MaintenanceCursor {
    phase: usize,
    table: usize,
    cutoff: Option<u64>,
    retry_after: Option<std::time::Instant>,
}

impl Storage {
    /// Perform one bounded maintenance operation; true completes the scheduled pass.
    pub(crate) fn maintain_step(
        &self,
        limit_bytes: u64,
        cursor: &mut MaintenanceCursor,
    ) -> Result<bool> {
        if cursor
            .retry_after
            .is_some_and(|after| std::time::Instant::now() < after)
        {
            return Ok(false);
        }
        if cursor.phase < 4 {
            if self.retention_days > 0 {
                let cutoff = match cursor.cutoff {
                    Some(cutoff) => cutoff,
                    None => {
                        let cutoff = Self::retention_cutoff_ns(self.retention_days)?;
                        cursor.cutoff = Some(cutoff);
                        cutoff
                    }
                };
                let deleted = match cursor.phase {
                    0 => self.audit_store.purge_expired_step(cutoff)?,
                    1 => self.token_store.purge_expired_step(cutoff)?,
                    2 => self.http_store.purge_expired_step(cutoff)?,
                    _ => self.token_consumption_store.purge_expired_step(cutoff)?,
                };
                if deleted > 0 {
                    return Ok(false);
                }
            }
            cursor.phase += 1;
            return Ok(false);
        }
        if cursor.phase == 4 {
            if self.audit_store.checkpoint_passive()? == CheckpointOutcome::Completed
                && self.audit_store.checkpoint_outcome()? == CheckpointOutcome::Completed
            {
                cursor.phase = 5;
                cursor.retry_after = None;
            } else {
                cursor.retry_after =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
            }
            return Ok(false);
        }
        let size = self.audit_store.size_snapshot()?;
        if limit_bytes == 0
            || size.physical_bytes <= limit_bytes
            || size.logical_bytes <= limit_bytes.saturating_mul(9) / 10
        {
            *cursor = MaintenanceCursor::default();
            return Ok(true);
        }
        let mut deleted = 0;
        for _ in 0..4 {
            deleted = match cursor.table {
                0 => self.audit_store.delete_oldest_batch(1)?,
                1 => self.token_store.delete_oldest_batch(1)?,
                2 => self.http_store.delete_oldest_batch(1)?,
                _ => self.token_consumption_store.delete_oldest_batch(1)?,
            };
            cursor.table = (cursor.table + 1) % 4;
            if deleted > 0 {
                break;
            }
        }
        cursor.phase = 4;
        if deleted == 0 {
            *cursor = MaintenanceCursor::default();
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paced_retention_visits_all_primary_tables_and_keeps_recent_records() {
        let dir = std::env::temp_dir().join(format!("paced-retention-{}", uuid::Uuid::new_v4()));
        let config = SqliteConfig {
            base_path: dir.clone(),
            retention_days: 1,
            ..SqliteConfig::default()
        };
        let storage = Storage::with_sqlite_config(&config).unwrap();
        storage.use_managed_checkpoints().unwrap();
        let db = rusqlite::Connection::open(config.db_path()).unwrap();
        for timestamp in [1_i64, 2, i64::MAX] {
            db.execute("INSERT INTO audit_events(event_type,timestamp_ns,pid,comm) VALUES ('fixture',?1,1,'fixture')", [timestamp]).unwrap();
            db.execute("INSERT INTO token_records(timestamp_ns,pid,comm,provider,input_tokens,output_tokens) VALUES (?1,1,'fixture','fixture',1,1)", [timestamp]).unwrap();
            db.execute("INSERT INTO token_consumption(timestamp_ns,comm,provider,model,total_input_tokens,total_output_tokens) VALUES (?1,'fixture','fixture','fixture',1,1)", [timestamp]).unwrap();
            let mut record = super::super::tests::sample_http_record();
            record.timestamp_ns = timestamp as u64;
            storage.http_store.insert(&record).unwrap();
        }
        let mut cursor = MaintenanceCursor::default();
        let mut finished = false;
        for _ in 0..32 {
            if storage.maintain_step(0, &mut cursor).unwrap() {
                finished = true;
                break;
            }
        }
        assert!(finished);
        storage.checkpoint_passive().unwrap();
        for table in [
            "audit_events",
            "token_records",
            "http_records",
            "token_consumption",
        ] {
            let timestamps = db
                .prepare(&format!("SELECT timestamp_ns FROM {table}"))
                .unwrap()
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(timestamps, [i64::MAX]);
        }
        drop(db);
        drop(storage);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn paced_cleanup_terminates_when_only_unmanaged_pages_exceed_the_limit() {
        let dir = std::env::temp_dir().join(format!("paced-unmanaged-{}", uuid::Uuid::new_v4()));
        let config = SqliteConfig {
            base_path: dir.clone(),
            retention_days: 0,
            ..SqliteConfig::default()
        };
        let storage = Storage::with_sqlite_config(&config).unwrap();
        let db = rusqlite::Connection::open(config.db_path()).unwrap();
        db.execute_batch("CREATE TABLE extension_data(payload BLOB); INSERT INTO extension_data VALUES (zeroblob(2097152));").unwrap();
        let mut cursor = MaintenanceCursor::default();
        assert!((0..16).any(|_| storage.maintain_step(1024 * 1024, &mut cursor).unwrap()));
        assert!(storage.audit_store.size_snapshot().unwrap().logical_bytes > 1024 * 1024);
        drop(db);
        drop(storage);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn paced_primary_cleanup_yields_to_readers_and_preserves_recent_rows() {
        let dir = std::env::temp_dir().join(format!("paced-primary-{}", uuid::Uuid::new_v4()));
        let config = SqliteConfig {
            base_path: dir.clone(),
            retention_days: 0,
            max_db_size_mb: 1,
            ..SqliteConfig::default()
        };
        let storage = Storage::with_sqlite_config(&config).unwrap();
        let mut record = super::super::tests::sample_http_record();
        record.request_body = Some("x".repeat(300 * 1024));
        for timestamp in 1..=8 {
            record.timestamp_ns = timestamp;
            storage.http_store.insert(&record).unwrap();
        }
        let reader = rusqlite::Connection::open(config.db_path()).unwrap();
        reader
            .execute_batch("BEGIN; SELECT COUNT(*) FROM http_records;")
            .unwrap();
        record.timestamp_ns = 9;
        storage.http_store.insert(&record).unwrap();
        let mut cursor = MaintenanceCursor::default();
        for _ in 0..6 {
            assert!(!storage.maintain_step(1024 * 1024, &mut cursor).unwrap());
        }
        assert_eq!(storage.http_store.count().unwrap(), 9);
        reader.execute_batch("COMMIT").unwrap();
        assert!(cursor.retry_after.is_some());
        assert!(!storage.maintain_step(1024 * 1024, &mut cursor).unwrap());
        assert_eq!(cursor.phase, 4);
        cursor.retry_after = None;
        let mut previous = 9;
        let mut finished = false;
        for _ in 0..100 {
            finished = storage.maintain_step(1024 * 1024, &mut cursor).unwrap();
            let current = storage.http_store.count().unwrap();
            assert!(previous - current <= 1);
            previous = current;
            if finished {
                break;
            }
        }
        assert!(finished);
        let remaining = storage.http_store.query_since(0).unwrap();
        assert!(!remaining.is_empty() && remaining.len() < 9);
        assert!(remaining.iter().any(|record| record.timestamp_ns == 9));
        assert!(storage.audit_store.size_snapshot().unwrap().logical_bytes <= 1024 * 1024 * 9 / 10);
        drop(reader);
        drop(storage);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
