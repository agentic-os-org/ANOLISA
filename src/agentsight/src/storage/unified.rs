//! Unified Storage - high-level entry point for persistence
//!
//! This module provides a unified interface for storing and querying records.
//! It supports multiple storage backends with a common API.
//!
//! # Architecture
//!
//! ```text
//! storage/
//! ├── mod.rs           # Module declarations and re-exports
//! ├── unified.rs       # Unified Storage facade
//! ├── sqlite/          # SQLite implementation
//! │   ├── mod.rs
//! │   ├── audit.rs     # AuditStore implementation
//! │   ├── token.rs     # TokenStore implementation
//! │   └── connection.rs
//! └── sls/             # SLS implementation (planned)
//!     └── ...
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use agentsight::storage::Storage;
//! use agentsight::analyzer::AnalysisResult;
//!
//! // Create default SQLite storage
//! let storage = Storage::sqlite()?;
//!
//! // Store analysis result (automatically routes to correct store)
//! storage.store(&analysis_result)?;
//!
//! // Or access specific stores directly
//! storage.audit().insert(&audit_record)?;
//! storage.token().add(token_record)?;
//! ```

use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, MaintenanceStatus, SizeBasis, SizePolicy, enforce_size_policy,
    retention_cutoff_ns,
};
use anyhow::Result;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::sqlite::{AuditStore, HttpStore, TokenConsumptionStore, TokenStore};
use crate::analyzer::AnalysisResult;

/// Storage backend type
#[derive(Debug, Clone, Default)]
pub enum StorageBackend {
    /// SQLite local storage
    #[default]
    Sqlite,
    /// Alibaba Cloud Log Service (planned)
    Sls {
        endpoint: String,
        project: String,
        logstore: String,
        access_key_id: String,
        access_key_secret: String,
    },
    /// No-op backend: stores nothing, used when all persistence features are disabled.
    Noop,
    // Future: other backends can be added here
}

/// Configuration for SQLite storage
#[derive(Debug, Clone)]
pub struct SqliteConfig {
    /// Base directory for database files
    pub base_path: PathBuf,
    /// Database filename (shared for all tables)
    pub db_name: String,
    /// Audit table name
    pub audit_table: String,
    /// Token table name
    pub token_table: String,
    /// HTTP table name
    pub http_table: String,
    /// Token consumption breakdown table name
    pub token_consumption_table: String,
    /// Data retention period in days (0 = no limit)
    pub retention_days: u64,
    /// Max database file size in MB (0 = no size-based limit)
    pub max_db_size_mb: u64,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        Self {
            base_path: crate::config::default_base_path(),
            db_name: crate::config::PRIMARY_DB_NAME.to_string(),
            audit_table: "audit_events".to_string(),
            token_table: "token_records".to_string(),
            http_table: "http_records".to_string(),
            token_consumption_table: "token_consumption".to_string(),
            retention_days: crate::config::DEFAULT_RETENTION_DAYS,
            max_db_size_mb: crate::config::DEFAULT_MAX_DB_SIZE_MB,
        }
    }
}

impl SqliteConfig {
    /// Create a new SQLite config with custom base path
    pub fn new(base_path: PathBuf) -> Self {
        Self {
            base_path,
            ..Default::default()
        }
    }

    /// Get database path
    pub fn db_path(&self) -> PathBuf {
        self.base_path.join(&self.db_name)
    }
}

/// Unified storage facade that provides access to all storage types
///
/// This is the main entry point for storage operations, supporting:
/// - Audit record persistence
/// - Token usage persistence and querying
/// - HTTP request/response persistence
/// - Multiple backend support (SQLite, SLS, etc.)
pub struct Storage {
    backend: StorageBackend,
    audit_store: AuditStore,
    token_store: TokenStore,
    http_store: HttpStore,
    token_consumption_store: TokenConsumptionStore,
    /// Data retention period in days (0 = no limit)
    retention_days: u64,
    /// Max database file size in bytes (0 = no size-based limit)
    max_db_size_bytes: u64,
    #[cfg(test)]
    db_path: PathBuf,
}

impl Storage {
    /// Create a new Storage with the specified backend
    pub fn new(backend: StorageBackend) -> Result<Self> {
        match &backend {
            StorageBackend::Sqlite => {
                let config = SqliteConfig::default();
                Self::with_sqlite_config(&config)
            }
            StorageBackend::Sls { .. } => {
                // TODO: Implement SLS storage
                anyhow::bail!("SLS storage backend is not yet implemented");
            }
            StorageBackend::Noop => Self::noop(),
        }
    }

    /// Create a new Storage with SQLite backend and custom config
    pub fn with_sqlite_config(config: &SqliteConfig) -> Result<Self> {
        let db_path = config.db_path();
        let audit_store = AuditStore::with_table(&db_path, &config.audit_table)?;
        let token_store = TokenStore::with_table(&db_path, &config.token_table)?;
        let http_store = HttpStore::with_table(&db_path, &config.http_table)?;
        let token_consumption_store =
            TokenConsumptionStore::with_table(&db_path, &config.token_consumption_table)?;
        Ok(Storage {
            backend: StorageBackend::Sqlite,
            audit_store,
            token_store,
            http_store,
            token_consumption_store,
            retention_days: config.retention_days,
            max_db_size_bytes: config.max_db_size_mb * 1024 * 1024,
            #[cfg(test)]
            db_path,
        })
    }

    /// Create a new Storage with default SQLite config
    pub fn sqlite() -> Result<Self> {
        Self::new(StorageBackend::Sqlite)
    }

    /// Create a new no-op Storage that silently drops all writes.
    ///
    /// Used when all persistence features are disabled in `agentsight.json`.
    ///
    /// # Errors
    ///
    /// Returns an error when an in-memory backing store cannot be initialized.
    pub fn noop() -> Result<Self> {
        // Reuse SQLite stores with an in-memory database so the store API
        // remains available without touching the filesystem.
        let db_path = PathBuf::from(":memory:");
        let audit_store = AuditStore::with_table(&db_path, "audit_events")?;
        let token_store = TokenStore::with_table(&db_path, "token_records")?;
        let http_store = HttpStore::with_table(&db_path, "http_records")?;
        let token_consumption_store =
            TokenConsumptionStore::with_table(&db_path, "token_consumption")?;
        Ok(Storage {
            backend: StorageBackend::Noop,
            audit_store,
            token_store,
            http_store,
            token_consumption_store,
            retention_days: 0,
            max_db_size_bytes: 0,
            #[cfg(test)]
            db_path,
        })
    }

    /// Returns true if this storage backend is the no-op backend.
    pub fn is_noop(&self) -> bool {
        matches!(self.backend, StorageBackend::Noop)
    }

    /// Get the backend type
    pub fn backend(&self) -> &StorageBackend {
        &self.backend
    }

    /// Get audit storage
    pub fn audit(&self) -> &AuditStore {
        &self.audit_store
    }

    /// Get token storage
    pub fn token(&self) -> &TokenStore {
        &self.token_store
    }

    /// Get HTTP storage
    pub fn http(&self) -> &HttpStore {
        &self.http_store
    }

    /// Get token consumption breakdown storage
    pub fn token_consumption(&self) -> &TokenConsumptionStore {
        &self.token_consumption_store
    }

    /// Store an analysis result (automatically routes to correct store).
    ///
    /// This hot path only persists data; lifecycle maintenance is owned by the
    /// process-wide database worker.
    pub fn store(&self, result: &AnalysisResult) -> Result<i64> {
        if let AnalysisResult::Http(_) = result {
            return Ok(0);
        }
        if matches!(self.backend, StorageBackend::Noop) {
            log::trace!("Noop storage dropping analysis result");
            return Ok(0);
        }
        log::debug!("Storing analysis result: {result:?}");
        let id = match result {
            AnalysisResult::Audit(record) => self.audit_store.insert(record),
            AnalysisResult::Token(record) => self.token_store.insert(record),
            AnalysisResult::Message(_msg) => {
                log::trace!("Message storage not implemented, skipping");
                Ok(0)
            }
            AnalysisResult::PromptTokens(_count) => {
                log::trace!("Prompt token count storage not implemented, skipping");
                Ok(0)
            }
            AnalysisResult::Http(record) => self.http_store.insert(record),
            AnalysisResult::TokenConsumption(breakdown) => self.token_consumption_store.insert(
                breakdown,
                breakdown.timestamp_ns,
                breakdown.pid,
                &breakdown.comm,
            ),
        }?;

        Ok(id)
    }

    /// Purge records older than the configured retention period.
    ///
    /// Deletes rows from all tables where `timestamp_ns` is older than
    /// `now - retention_days`. Returns the total number of deleted rows.
    pub fn purge_expired(&self) -> Result<u64> {
        if self.retention_days == 0 {
            return Ok(0);
        }

        let cutoff_ns = Self::retention_cutoff_ns(self.retention_days)?;
        let mut total_deleted = 0u64;

        let audit_deleted = self.audit_store.purge_before(cutoff_ns)?;
        total_deleted += audit_deleted;

        let token_deleted = self.token_store.purge_before(cutoff_ns)?;
        total_deleted += token_deleted;

        let http_deleted = self.http_store.purge_before(cutoff_ns)?;
        total_deleted += http_deleted;

        let consumption_deleted = self.token_consumption_store.purge_before(cutoff_ns)?;
        total_deleted += consumption_deleted;

        if total_deleted > 0 {
            log::info!(
                "Purged {} expired records (retention={}d, audit={}, token={}, http={}, consumption={})",
                total_deleted,
                self.retention_days,
                audit_deleted,
                token_deleted,
                http_deleted,
                consumption_deleted,
            );
        }

        Ok(total_deleted)
    }

    /// Applies age retention, gates size cleanup on a successful checkpoint,
    /// and then enforces the configured size limit.
    ///
    /// # Errors
    ///
    /// Returns an error when retention, checkpoint, measurement, or pruning fails.
    pub fn maintain(&self) -> Result<()> {
        let deleted_by_age = self.purge_expired()?;
        if (deleted_by_age > 0 || self.max_db_size_bytes > 0)
            && self.audit_store.checkpoint_outcome()? == CheckpointOutcome::Busy
        {
            log::warn!("Primary WAL checkpoint remained busy before size maintenance");
            return Ok(());
        }
        self.purge_oversized()
    }

    /// Purge oldest records when the database exceeds the size limit.
    ///
    /// The lifecycle crate controls measurement and convergence while each
    /// business store retains ownership of its deletion query.
    pub fn purge_oversized(&self) -> Result<()> {
        let policy = SizePolicy {
            limit_bytes: self.max_db_size_bytes,
            trigger_bytes: self.max_db_size_bytes,
            target_bytes: self.max_db_size_bytes.saturating_mul(9) / 10,
            trigger_basis: SizeBasis::Physical,
            target_basis: SizeBasis::Logical,
            max_rounds: 20,
            max_stalled_rounds: 20,
        };
        let report = enforce_size_policy::<anyhow::Error>(
            policy,
            || self.audit_store.size_snapshot(),
            |fraction| {
                let mut deleted = 0usize;
                deleted += self
                    .audit_store
                    .delete_oldest_batch(purge_share(self.audit_store.count()?, fraction))?;
                deleted += self
                    .token_store
                    .delete_oldest_batch(purge_share(self.token_store.count(), fraction))?;
                deleted += self
                    .http_store
                    .delete_oldest_batch(purge_share(self.http_store.count()?, fraction))?;
                deleted += self
                    .token_consumption_store
                    .delete_oldest_batch(purge_share(
                        self.token_consumption_store.count(),
                        fraction,
                    ))?;
                Ok(deleted)
            },
            || self.audit_store.checkpoint_outcome(),
        )?;

        match report.status {
            MaintenanceStatus::CheckpointBusy => log::warn!(
                "WAL checkpoint during size-based purge was busy after deleting {} rows",
                report.deleted_rows
            ),
            MaintenanceStatus::Stalled | MaintenanceStatus::MaxRounds => log::error!(
                "Size-based purge stopped with {:?}: database remains {} bytes (limit {})",
                report.status,
                report.after.logical_bytes,
                self.max_db_size_bytes
            ),
            MaintenanceStatus::TargetReached => log::info!(
                "Size-based purge deleted {} rows in {} rounds; logical size is {} bytes",
                report.deleted_rows,
                report.rounds,
                report.after.logical_bytes
            ),
            MaintenanceStatus::Disabled
            | MaintenanceStatus::BelowTrigger
            | MaintenanceStatus::ReusableCapacity
            | MaintenanceStatus::NoRows => {}
        }
        Ok(())
    }

    #[cfg(test)]
    fn total_db_size(&self) -> u64 {
        self.audit_store
            .size_snapshot()
            .map(|snapshot| snapshot.physical_bytes)
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn effective_db_size(&self) -> u64 {
        self.audit_store
            .size_snapshot()
            .map(|snapshot| snapshot.logical_bytes)
            .unwrap_or(0)
    }

    /// Compute the cutoff timestamp for retention.
    fn retention_cutoff_ns(retention_days: u64) -> Result<u64> {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        retention_cutoff_ns(now_ns, retention_days).map_err(Into::into)
    }

    /// Store multiple analysis results
    ///
    /// Returns the number of successfully stored records.
    pub fn store_all(&self, results: &[AnalysisResult]) -> Result<usize> {
        let mut count = 0;
        for result in results {
            self.store(result)?;
            count += 1;
        }
        Ok(count)
    }

    /// Execute WAL checkpoint on all store connections.
    ///
    /// Flushes WAL data back to the main database file and truncates the
    /// `-wal` / `-shm` files. Should be called during graceful shutdown.
    ///
    /// Since all stores share the same database file, a successful checkpoint
    /// on any one connection covers the entire database. We try all connections
    /// and report the first error (if any).
    pub fn checkpoint(&self) -> Result<()> {
        // Only need one successful checkpoint since all stores share the same db,
        // but we try on audit_store first and fall through if it fails.
        if let Err(e) = self.audit_store.checkpoint() {
            log::warn!("Audit store checkpoint failed: {e}, trying token store");
            if let Err(e2) = self.token_store.checkpoint() {
                log::warn!("Token store checkpoint failed: {e2}, trying http store");
                self.http_store.checkpoint()?;
            }
        }
        log::info!("WAL checkpoint completed");
        Ok(())
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        if let Err(e) = self.checkpoint() {
            log::warn!("WAL checkpoint during Storage drop failed: {e}");
        }
    }
}

/// Rows to delete from a table in one purge round: `pct` of its rows, at
/// least 1 (deleting from an empty table is a no-op).
fn purge_share(rows: u64, pct: f64) -> usize {
    ((rows as f64 * pct) as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::token::TokenRecord;

    #[test]
    fn test_noop_storage_is_noop() {
        let storage = Storage::noop().unwrap();
        assert!(storage.is_noop());
    }

    #[test]
    fn test_noop_storage_store_returns_zero() {
        let storage = Storage::noop().unwrap();
        assert!(storage.is_noop());
        // Call store to verify noop path returns Ok(0) without writing
        let token_record = crate::analyzer::token::TokenRecord {
            id: 0,
            timestamp_ns: 0,
            pid: 1,
            comm: "test".to_string(),
            agent: None,
            model: None,
            provider: "test".to_string(),
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            request_id: None,
            endpoint: None,
            tool_calls: vec![],
            reasoning_content: None,
        };
        let result = crate::analyzer::AnalysisResult::Token(token_record);
        let id = storage.store(&result).unwrap();
        assert_eq!(id, 0);
    }

    #[test]
    fn test_noop_storage_should_persist() {
        let storage = Storage::noop().unwrap();
        // Just verify it doesn't panic
        let _ = storage.is_noop();
        drop(storage);
    }

    /// Unique per-test directory under the system temp dir.
    fn unique_base_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agentsight_unified_{label}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_config(base_path: PathBuf, max_db_size_mb: u64) -> SqliteConfig {
        SqliteConfig {
            base_path,
            retention_days: 0,
            max_db_size_mb,
            ..Default::default()
        }
    }

    /// Token record with a payload of roughly `payload_bytes` for growing the
    /// database file quickly in size-limit tests.
    fn bulky_token_record(timestamp_ns: u64, payload_bytes: usize) -> TokenRecord {
        TokenRecord {
            id: 0,
            timestamp_ns,
            pid: 1,
            comm: "test".to_string(),
            agent: None,
            model: None,
            provider: "test".to_string(),
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            request_id: None,
            // `endpoint` is a persisted TEXT column, so the payload actually
            // lands in the database file (`reasoning_content` is not stored).
            endpoint: Some("x".repeat(payload_bytes)),
            tool_calls: vec![],
            reasoning_content: None,
        }
    }

    #[test]
    fn test_purge_oversized_disabled_when_limit_zero() {
        let dir = unique_base_dir("limit_zero");
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), 0)).unwrap();

        for i in 0..10 {
            storage
                .token_store
                .insert(&bulky_token_record(i, 64))
                .unwrap();
        }
        storage.purge_oversized().unwrap();

        assert_eq!(storage.token_store.count(), 10, "limit 0 must never delete");
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_purge_oversized_noop_when_below_limit() {
        let dir = unique_base_dir("below_limit");
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), 500)).unwrap();

        for i in 0..10 {
            storage
                .token_store
                .insert(&bulky_token_record(i, 64))
                .unwrap();
        }
        storage.purge_oversized().unwrap();

        assert_eq!(
            storage.token_store.count(),
            10,
            "below-limit purge must not delete"
        );
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_purge_oversized_shrinks_db_below_limit() {
        let dir = unique_base_dir("shrink");
        let limit_mb = 1u64;
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), limit_mb)).unwrap();

        // ~3MB of data: 300 rows × 10KB payload, well over the 1MB limit.
        for i in 0..300 {
            storage
                .token_store
                .insert(&bulky_token_record(i, 10 * 1024))
                .unwrap();
        }
        storage.checkpoint().unwrap();
        assert!(
            storage.total_db_size() > limit_mb * 1024 * 1024,
            "test setup must produce an oversized database"
        );

        storage.purge_oversized().unwrap();

        // Convergence is on logical size: the physical file keeps its peak
        // size (freed pages stay on the freelist, #2888), but the deleted
        // rows must be gone and the logical size must fit.
        assert!(
            storage.token_store.count() < 300,
            "purge must delete rows when oversized"
        );
        let effective = storage.effective_db_size();
        assert!(
            effective <= limit_mb * 1024 * 1024,
            "logical size must be within the limit after purge, got {effective} bytes"
        );
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for #2870: a database only slightly over the limit must be
    /// trimmed oldest-first, not wiped. The previous fixed-batch loop
    /// (20x1000 rows per table per round, without re-measuring between
    /// batches) emptied every table in the first round; this test fails under
    /// it because nothing would survive.
    #[test]
    fn test_purge_oversized_trims_oldest_keeps_newest() {
        let dir = unique_base_dir("trim_oldest");
        let limit_mb = 1u64;
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), limit_mb)).unwrap();

        // ~1.2MB over a 1MB limit (~17% overshoot): 110 rows x ~10KB payload.
        let base_ts = 1_000_000_000u64;
        for i in 0..110u64 {
            storage
                .token_store
                .insert(&bulky_token_record(base_ts + i, 10 * 1024))
                .unwrap();
        }
        storage.checkpoint().unwrap();
        let size_before = storage.total_db_size();
        assert!(
            size_before > limit_mb * 1024 * 1024,
            "setup must exceed the limit, got {size_before}"
        );

        storage.purge_oversized().unwrap();

        let remaining = storage.token_store.count();
        assert!(remaining > 0, "purge must not wipe the table (#2870)");
        assert!(remaining < 110, "oversized db must be trimmed");
        let effective = storage.effective_db_size();
        assert!(
            effective <= limit_mb * 1024 * 1024,
            "logical size must converge below the limit (#2888: the physical \
             file keeps its peak): {effective} bytes"
        );

        // Oldest-first trimming removes low timestamps, so the newest seeded
        // row must survive.
        let conn = rusqlite::Connection::open(dir.join("agentsight.db")).unwrap();
        let max_ts: u64 = conn
            .query_row("SELECT MAX(timestamp_ns) FROM token_records", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(max_ts, base_ts + 109, "newest row must survive");
        drop(conn);

        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the file is dominated by a few large NEW rows and the oldest rows
    /// are tiny, deleting the oldest fraction moves few bytes. The purge must
    /// still keep trimming (the logical size drops with every deletion) until
    /// it converges, rather than stalling on the small-row rounds.
    #[test]
    fn test_purge_oversized_skewed_row_sizes_still_converges() {
        let dir = unique_base_dir("skewed");
        let limit_mb = 1u64;
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), limit_mb)).unwrap();

        // 100 tiny old rows (~5KB total, sharing B-tree pages) + 4 large new
        // rows (~260KB each): ~1.05MB, slightly over the 1MB limit.
        for i in 0..100u64 {
            storage
                .token_store
                .insert(&bulky_token_record(1_000 + i, 50))
                .unwrap();
        }
        for i in 0..4u64 {
            storage
                .token_store
                .insert(&bulky_token_record(1_000_000 + i, 260 * 1024))
                .unwrap();
        }
        storage.checkpoint().unwrap();
        // Compact once up front so the physical baseline is deterministic
        // (purge itself never VACUUMs anymore, #2888).
        storage.audit_store.vacuum().unwrap();
        let size_before = storage.total_db_size();
        assert!(
            size_before > limit_mb * 1024 * 1024,
            "setup must exceed the limit, got {size_before}"
        );

        storage.purge_oversized().unwrap();

        let effective = storage.effective_db_size();
        assert!(
            effective <= limit_mb * 1024 * 1024,
            "logical size must converge below the limit even when early rounds \
             free little, got {effective} bytes"
        );
        let conn = rusqlite::Connection::open(dir.join("agentsight.db")).unwrap();
        let max_ts: u64 = conn
            .query_row("SELECT MAX(timestamp_ns) FROM token_records", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(max_ts, 1_000_003, "newest large row must survive");
        drop(conn);

        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When another connection holds a read snapshot, the truncating WAL
    /// checkpoint reports busy without an SQL error and the WAL stays intact.
    /// The purge must stop instead of deleting round after round against a
    /// size that can never converge (only the WAL keeps growing) — under the
    /// pre-fix behavior the loop kept deleting until the tables were empty.
    #[test]
    fn test_purge_oversized_stops_when_wal_checkpoint_busy() {
        let dir = unique_base_dir("busy_ckpt");
        let limit_mb = 1u64;
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), 0)).unwrap();
        for i in 0..300u64 {
            storage
                .token_store
                .insert(&bulky_token_record(i, 10 * 1024))
                .unwrap();
        }
        storage.checkpoint().unwrap();
        drop(storage);

        // Hold a read snapshot on a separate connection: this blocks
        // `PRAGMA wal_checkpoint(TRUNCATE)` from completing (busy).
        let reader = rusqlite::Connection::open(dir.join("agentsight.db")).unwrap();
        reader
            .execute_batch("BEGIN; SELECT COUNT(*) FROM token_records;")
            .unwrap();

        // Reopen with the limit enabled and append a WAL frame newer than the
        // reader snapshot, making the pre-maintenance truncating checkpoint busy.
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), limit_mb)).unwrap();
        storage
            .token_store
            .insert(&bulky_token_record(300, 10 * 1024))
            .unwrap();
        storage.maintain().unwrap();
        let remaining = storage.token_store.count();
        assert_eq!(
            remaining, 301,
            "a busy pre-maintenance checkpoint must prevent the first deletion"
        );
        drop(storage);
        drop(reader);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_open_does_not_run_synchronous_maintenance() {
        let dir = unique_base_dir("startup");
        let limit_mb = 1u64;

        {
            let storage = Storage::with_sqlite_config(&test_config(dir.clone(), 0)).unwrap();
            for i in 0..300 {
                storage
                    .token_store
                    .insert(&bulky_token_record(i, 10 * 1024))
                    .unwrap();
            }
            storage.checkpoint().unwrap();
        }

        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), limit_mb)).unwrap();
        assert_eq!(storage.token_store.count(), 300);
        storage.maintain().unwrap();
        assert!(storage.token_store.count() < 300);
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_total_db_size_counts_wal_file() {
        let dir = unique_base_dir("wal_size");
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), 500)).unwrap();

        // Without a checkpoint, recent writes stay in the -wal file.
        for i in 0..20 {
            storage
                .token_store
                .insert(&bulky_token_record(i, 1024))
                .unwrap();
        }

        let wal_len = std::fs::metadata(format!("{}-wal", storage.db_path.display()))
            .map(|m| m.len())
            .unwrap_or(0);
        assert!(
            wal_len > 0,
            "WAL file should hold the un-checkpointed writes"
        );

        let main_len = std::fs::metadata(&storage.db_path).unwrap().len();
        assert!(
            storage.total_db_size() >= main_len + wal_len,
            "size accounting must include the WAL file"
        );
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_oldest_batch_removes_oldest_rows() {
        let dir = unique_base_dir("oldest_batch");
        let storage = Storage::with_sqlite_config(&test_config(dir.clone(), 0)).unwrap();

        for ts in 1..=10 {
            storage
                .token_store
                .insert(&bulky_token_record(ts, 64))
                .unwrap();
        }

        let deleted = storage.token_store.delete_oldest_batch(4).unwrap();
        assert_eq!(deleted, 4);

        let remaining = storage.token_store.all();
        assert_eq!(remaining.len(), 6);
        let min_ts = remaining.iter().map(|r| r.timestamp_ns).min().unwrap();
        assert_eq!(min_ts, 5, "the four oldest rows (ts 1-4) must be gone");
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_store_does_not_run_synchronous_maintenance() {
        let dir = unique_base_dir("background_maintenance");
        let config = SqliteConfig {
            base_path: dir.clone(),
            retention_days: 1,
            max_db_size_mb: 500,
            ..Default::default()
        };
        let storage = Storage::with_sqlite_config(&config).unwrap();

        let expired = crate::analyzer::AnalysisResult::Token(bulky_token_record(1, 64));
        storage.store(&expired).unwrap();
        assert_eq!(storage.token_store.count(), 1);

        storage.maintain().unwrap();
        assert_eq!(storage.token_store.count(), 0);
        drop(storage);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
