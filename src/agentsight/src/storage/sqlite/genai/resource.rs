//! Agent process resource sample persistence.

use rusqlite::params;
use serde::Serialize;

use super::GenAISqliteStore;

/// One process-level resource observation.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResourceSample {
    /// Unix epoch nanoseconds, aligned with GenAI event timestamps.
    pub timestamp_ns: i64,
    /// Observed process ID.
    pub pid: i32,
    /// Agent discovery name, when available.
    pub agent_name: Option<String>,
    /// Process CPU usage where one fully utilized core equals 100%.
    pub cpu_percent: f64,
    /// Resident set size in bytes.
    pub memory_bytes: i64,
}

impl GenAISqliteStore {
    /// Inserts one sampling batch in a single SQLite transaction.
    pub fn insert_resource_samples(
        &self,
        samples: &[ResourceSample],
    ) -> Result<(), Box<dyn std::error::Error>> {
        if samples.is_empty() {
            return Ok(());
        }
        self.check_and_prune_if_needed()?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|error| format!("GenAI SQLite connection mutex poisoned: {error}"))?;
        let transaction = conn.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO agent_resource_samples
                 (timestamp_ns, pid, agent_name, cpu_percent, memory_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for sample in samples {
                statement.execute(params![
                    sample.timestamp_ns,
                    sample.pid,
                    sample.agent_name,
                    sample.cpu_percent,
                    sample.memory_bytes,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserting_empty_batch_is_a_noop() {
        let path = std::env::temp_dir().join(format!(
            "agentsight-empty-resource-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        let store =
            GenAISqliteStore::new_with_path(&path, crate::config::InsertStoragePolicy::default())
                .expect("resource test store");
        store
            .insert_resource_samples(&[])
            .expect("empty resource batch");
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
