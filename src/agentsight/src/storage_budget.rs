//! Global capacity budget shared by every SQLite maintenance job.
//!
//! Users configure one combined storage limit instead of per-database values.
//! Each maintenance job asks the budget for its effective size limit right
//! before running; when the combined allocation exceeds the limit the excess
//! is assigned to the largest databases first, so every store keeps its own
//! safe-deletion rules while the group converges back under the limit.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{
    CAUSAL_DB_NAME, ENFORCEMENT_DB_NAME, REUSE_DB_NAME, SECURITY_AUDIT_DB_NAME, StorageConfig,
};
use crate::database::DatabaseId;

const BYTES_PER_MB: u64 = 1024 * 1024;

/// Combined-capacity budget evaluated by every maintenance job.
#[derive(Debug)]
pub struct StorageBudget {
    /// Runtime configuration file probed for live `max_total_size_mb` edits.
    config_path: Option<PathBuf>,
    /// Last valid cap, retained while the runtime file is absent or invalid.
    last_valid_cap_mb: AtomicU64,
    /// Canonical database files measured for the combined allocation.
    stores: Vec<(DatabaseId, PathBuf)>,
}

impl StorageBudget {
    /// Builds a budget from the effective startup configuration.
    pub fn new(config_path: Option<PathBuf>, storage: &StorageConfig) -> Self {
        let private = storage.base_path.join(".agentsight-private");
        let stores = vec![
            (DatabaseId::Primary, storage.primary_path()),
            (DatabaseId::GenAi, storage.genai_path()),
            (DatabaseId::Interruptions, storage.interruption_path()),
            (DatabaseId::Trajectories, storage.trajectory_path()),
            (DatabaseId::Optimization, storage.optimization_path()),
            (
                DatabaseId::SecurityAudit,
                private.join(SECURITY_AUDIT_DB_NAME),
            ),
            (DatabaseId::Reuse, private.join(REUSE_DB_NAME)),
            (DatabaseId::Causal, private.join(CAUSAL_DB_NAME)),
            (DatabaseId::Enforcement, private.join(ENFORCEMENT_DB_NAME)),
        ];
        Self {
            config_path,
            last_valid_cap_mb: AtomicU64::new(storage.max_total_size_mb),
            stores,
        }
    }

    /// Effective combined limit in MiB, re-read from the config file so
    /// Dashboard edits apply without a restart. Zero disables the budget.
    /// Invalid or partial edits retain the most recently observed valid value.
    pub fn cap_mb(&self) -> u64 {
        if let Some(cap_mb) = self
            .config_path
            .as_deref()
            .and_then(read_cap_from_file)
            .filter(|cap_mb| StorageConfig::validate_total_size_mb(*cap_mb).is_ok())
        {
            self.last_valid_cap_mb.store(cap_mb, Ordering::Relaxed);
            cap_mb
        } else {
            self.last_valid_cap_mb.load(Ordering::Relaxed)
        }
    }

    /// Combined physical bytes of every measured database.
    pub fn total_physical_bytes(&self) -> u64 {
        self.stores
            .iter()
            .map(|(_, path)| physical_db_bytes(path))
            .sum()
    }

    /// Effective per-store size limit in MiB for one maintenance run.
    ///
    /// Returns the store's own limit unchanged when the budget is disabled or
    /// satisfied. Otherwise the store must shed its share of the excess (the
    /// largest databases first) and receives a reduced limit that triggers its
    /// normal size enforcement. Sub-MiB excess is treated as satisfied so
    /// rounding cannot stall convergence.
    pub fn effective_limit_mb(&self, own_id: DatabaseId, own_limit_mb: u64) -> u64 {
        let cap_mb = self.cap_mb();
        if cap_mb == 0 {
            return own_limit_mb;
        }
        let sizes: Vec<(DatabaseId, u64)> = self
            .stores
            .iter()
            .map(|(id, path)| (*id, physical_db_bytes(path)))
            .collect();
        let total: u64 = sizes.iter().map(|(_, size)| size).sum();
        let cap_bytes = cap_mb.saturating_mul(BYTES_PER_MB);
        let mut excess = total.saturating_sub(cap_bytes);
        if excess < BYTES_PER_MB {
            return own_limit_mb;
        }

        // Largest databases shed the excess first; ties break on the stable
        // id so every job computes the same assignment.
        let mut ranked = sizes;
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        for (id, size) in ranked {
            let shed = excess.min(size);
            if id == own_id {
                let kept_mb = (size - shed) / BYTES_PER_MB;
                let reduced_mb = kept_mb.max(1);
                return if own_limit_mb == 0 {
                    reduced_mb
                } else {
                    own_limit_mb.min(reduced_mb)
                };
            }
            excess -= shed;
            if excess == 0 {
                return own_limit_mb;
            }
        }
        own_limit_mb
    }
}

/// Reads `storage.max_total_size_mb` from a runtime configuration file.
fn read_cap_from_file(path: &Path) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct StorageProbe {
        #[serde(default)]
        max_total_size_mb: Option<u64>,
    }
    #[derive(serde::Deserialize)]
    struct ConfigProbe {
        #[serde(default)]
        storage: Option<StorageProbe>,
    }
    let content = std::fs::read_to_string(path).ok()?;
    let probe: ConfigProbe = serde_json::from_str(&content).ok()?;
    probe.storage.and_then(|storage| storage.max_total_size_mb)
}

/// Physical bytes of one database including WAL and SHM sidecars.
///
/// WAL-heavy stores may rank first for one maintenance pass. Their own
/// maintenance checkpoints before pruning, so a WAL that shrinks below the
/// assigned limit avoids deletion and the next pass rebalances its allocation.
fn physical_db_bytes(path: &Path) -> u64 {
    let mut total = file_len(path);
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        total += file_len(Path::new(&sidecar));
    }
    total
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Merges `storage.max_total_size_mb` into a configuration file atomically.
pub(crate) fn write_total_size_limit(
    config_path: &Path,
    max_total_size_mb: u64,
) -> Result<(), String> {
    StorageConfig::validate_total_size_mb(max_total_size_mb)?;
    let mut root: serde_json::Value = match std::fs::read_to_string(config_path) {
        Ok(content) => serde_json::from_str(&content)
            .map_err(|error| format!("existing config is not valid JSON: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({ "schema_version": crate::config::CURRENT_SCHEMA_VERSION })
        }
        Err(error) => return Err(format!("cannot read config file: {error}")),
    };
    let storage = root
        .as_object_mut()
        .ok_or_else(|| "config root must be a JSON object".to_string())?
        .entry("storage")
        .or_insert_with(|| serde_json::json!({}));
    storage
        .as_object_mut()
        .ok_or_else(|| "storage section must be a JSON object".to_string())?
        .insert(
            "max_total_size_mb".to_string(),
            serde_json::Value::from(max_total_size_mb),
        );

    let mut tmp = config_path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp_path = Path::new(&tmp);
    let pretty = serde_json::to_string_pretty(&root).map_err(|error| error.to_string())?;
    std::fs::write(tmp_path, pretty)
        .map_err(|error| format!("cannot write config file: {error}"))?;
    std::fs::rename(tmp_path, config_path)
        .map_err(|error| format!("cannot replace config file: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn budget_with(base: &Path, cap_mb: u64) -> StorageBudget {
        let storage = StorageConfig {
            base_path: base.to_path_buf(),
            max_total_size_mb: cap_mb,
            ..StorageConfig::default()
        };
        StorageBudget::new(None, &storage)
    }

    fn write_db(base: &Path, name: &str, bytes: usize) -> PathBuf {
        let path = base.join(name);
        std::fs::write(&path, vec![0u8; bytes]).unwrap();
        path
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agentsight-budget-{tag}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn disabled_or_satisfied_budget_keeps_store_limit() {
        let base = unique_dir("satisfied");
        let budget = budget_with(&base, 9);
        write_db(&base, "agentsight.db", 512 * 1024);
        assert_eq!(
            budget.effective_limit_mb(DatabaseId::Primary, 500),
            500,
            "usage below the cap must not reduce the store limit"
        );

        let disabled = budget_with(&base, 0);
        write_db(&base, "genai_events.db", 4 * 1024 * 1024);
        assert_eq!(disabled.effective_limit_mb(DatabaseId::GenAi, 0), 0);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn excess_is_assigned_to_largest_store_first() {
        let base = unique_dir("excess");
        let budget = budget_with(&base, 9);
        // 12 MiB primary + 1 MiB genai against a 9 MiB cap: the largest store
        // must shed 4 MiB and may keep 8 MiB; the small store is untouched.
        write_db(&base, "agentsight.db", 12 * 1024 * 1024);
        write_db(&base, "genai_events.db", 1024 * 1024);

        assert_eq!(budget.effective_limit_mb(DatabaseId::Primary, 500), 8);
        assert_eq!(budget.effective_limit_mb(DatabaseId::GenAi, 200), 200);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn excess_spills_to_smaller_stores_when_largest_is_insufficient() {
        let base = unique_dir("spill");
        let budget = budget_with(&base, 9);
        // 10 MiB + 10 MiB against a 9 MiB cap: the stable-id winner sheds its
        // full 10 MiB first, then the other sheds 1 MiB.
        write_db(&base, "agentsight.db", 10 * 1024 * 1024);
        write_db(&base, "genai_events.db", 10 * 1024 * 1024);

        assert_eq!(budget.effective_limit_mb(DatabaseId::Primary, 500), 1);
        assert_eq!(budget.effective_limit_mb(DatabaseId::GenAi, 200), 9);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn budget_overrides_disabled_store_limit_when_it_must_shed() {
        let base = unique_dir("disabled-store");
        let budget = budget_with(&base, 9);
        write_db(&base, "agentsight.db", 12 * 1024 * 1024);
        assert_eq!(budget.effective_limit_mb(DatabaseId::Primary, 0), 9);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn sub_mib_excess_does_not_trigger_cleanup() {
        let base = unique_dir("sub-mib");
        let budget = budget_with(&base, 9);
        write_db(&base, "agentsight.db", 9 * 1024 * 1024 + 512 * 1024);
        assert_eq!(budget.effective_limit_mb(DatabaseId::Primary, 500), 500);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn cap_is_hot_read_from_config_file() {
        let base = unique_dir("hot-reload");
        let config_path = base.join("config.json");
        let storage = StorageConfig {
            base_path: base.clone(),
            max_total_size_mb: 100,
            ..StorageConfig::default()
        };
        write_db(&base, "agentsight.db", 12 * 1024 * 1024);

        std::fs::write(&config_path, r#"{"storage": {"max_total_size_mb": 9}}"#).unwrap();
        let budget = StorageBudget::new(Some(config_path.clone()), &storage);
        assert_eq!(budget.cap_mb(), 9);
        assert_eq!(budget.effective_limit_mb(DatabaseId::Primary, 500), 9);

        // Unreadable or invalid edits retain the latest valid value.
        std::fs::write(&config_path, "{ not json").unwrap();
        assert_eq!(budget.cap_mb(), 9);
        std::fs::write(&config_path, r#"{"storage": {"max_total_size_mb": 8}}"#).unwrap();
        assert_eq!(budget.cap_mb(), 9);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn total_includes_wal_sidecars() {
        let base = unique_dir("sidecars");
        let budget = budget_with(&base, 10);
        write_db(&base, "agentsight.db", 1024);
        let mut wal = base.join("agentsight.db");
        wal.as_mut_os_string().push("-wal");
        std::fs::write(&wal, vec![0u8; 2048]).unwrap();
        assert_eq!(budget.total_physical_bytes(), 3072);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
