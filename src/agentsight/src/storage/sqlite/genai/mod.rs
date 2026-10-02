//! GenAI semantic events SQLite storage
//!
//! Stores GenAI events (LLM calls, tool uses, etc.) to SQLite when SLS is not configured.
//! Implements the GenAIExporter trait for pluggable integration.
//!
//! # Lifecycle policy
//!
//! Retention and capacity are supplied explicitly by the caller. Maintenance
//! checkpoints WAL data and deletes oldest rows without running `VACUUM`.

mod events;
mod pending;
mod resource;
mod resource_timeline;
mod schema;
mod session;
mod stats;
#[cfg(test)]
mod tests;

use agentsight_sqlite_lifecycle::{ConnectionMode, ConnectionOptions, open_connection};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{BatchConfig, PeriodicStoragePolicy};

/// SQL mirror of [`TokenRecord::billed_input_tokens`], as a `CASE` expression
/// over the raw columns.
///
/// Anthropic reports `cache_creation_tokens` / `cache_read_tokens` outside
/// `input_tokens`, so they add to the billed input. OpenAI-compatible gateways
/// and Gemini already count cached tokens inside the reported input, so adding
/// them again inflates every cached call. These aggregations run in SQLite and
/// cannot call the Rust helper, so the rule is restated here — keep the two in
/// sync.
///
/// Deriving totals from the raw columns instead of reading the stored
/// `total_tokens` also corrects rows written before this rule existed, since
/// `input_tokens` and the cache columns have always held what the provider
/// reported.
///
/// Expands to a string literal so callers assemble their query with `concat!`
/// into a fixed `&'static str`, honouring the storage-layer rule that SQL is
/// built as parameterized literal text (`?` placeholders, no runtime string
/// building) while still stating this fragment once.
///
/// [`TokenRecord::billed_input_tokens`]: crate::analyzer::TokenRecord::billed_input_tokens
macro_rules! billed_input_col {
    () => {
        "CASE WHEN LOWER(COALESCE(provider, '')) = 'anthropic' \
         THEN input_tokens + COALESCE(cache_creation_tokens, 0) + COALESCE(cache_read_tokens, 0) \
         ELSE input_tokens END"
    };
}
pub(crate) use billed_input_col;

// Re-export public types from sub-modules
pub use events::TraceEventDetail;
pub use pending::{PendingCallInfo, PendingOrigin, SseEnrichment};
pub use resource::ResourceSample;
pub use resource_timeline::{SessionPhase, SessionResourceTimeline};
pub use session::{SavingsSessionSummary, SessionSummary, ToolCallTurnInfo, TraceSummary};
pub use stats::{
    AgentActivitySummary, AgentTokenSummary, LatencyMetricsSummary, MetricPercentiles,
    ModelTimeseriesBucket, TimeseriesBucket,
};

/// SQLite-backed GenAI event storage
pub struct GenAISqliteStore {
    conn: Mutex<Connection>,
    db_path: PathBuf,
    /// Batch insert configuration: events are buffered until `max_size` or
    /// `flush_ms` is reached, then written inside a single SQLite transaction.
    batch_config: BatchConfig,
    /// Buffered events waiting to be flushed.
    pending: Mutex<Vec<crate::genai::semantic::GenAISemanticEvent>>,
    /// Timestamp of the last successful flush.
    last_flush: Mutex<Instant>,
    /// Retention and capacity settings supplied by the owning runtime.
    storage_policy: PeriodicStoragePolicy,
}

impl GenAISqliteStore {
    /// Create a new GenAI SQLite store at the default path.
    pub fn new(storage_policy: PeriodicStoragePolicy) -> Result<Self, Box<dyn std::error::Error>> {
        let path = Self::default_path();
        Self::new_with_path(&path, storage_policy)
    }

    /// Create a new GenAI SQLite store at an arbitrary path with default batch config.
    pub fn new_with_path(
        path: &std::path::Path,
        storage_policy: PeriodicStoragePolicy,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_path_and_batch(path, None, storage_policy)
    }

    /// Opens an existing GenAI database without creating or modifying it.
    ///
    /// # Errors
    ///
    /// Returns an error when the database does not exist or cannot be opened read-only.
    pub fn open_read_only_existing(
        path: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = open_connection(
            path,
            ConnectionOptions {
                mode: ConnectionMode::ReadOnlyExisting,
                enable_wal: false,
                ..ConnectionOptions::default()
            },
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
            batch_config: BatchConfig::default(),
            pending: Mutex::new(Vec::new()),
            last_flush: Mutex::new(Instant::now()),
            storage_policy: PeriodicStoragePolicy::default(),
        })
    }

    /// Create a new GenAI SQLite store with explicit batch and lifecycle configuration.
    pub fn new_with_path_and_batch(
        path: &std::path::Path,
        batch: Option<BatchConfig>,
        storage_policy: PeriodicStoragePolicy,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = open_connection(path, ConnectionOptions::default())?;
        let batch_config = batch.unwrap_or_default();
        let store = GenAISqliteStore {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
            batch_config,
            pending: Mutex::new(Vec::new()),
            last_flush: Mutex::new(Instant::now()),
            storage_policy,
        };
        store.init_tables()?;

        let current_size = store.size_snapshot()?.physical_bytes;
        let max_size = storage_policy.max_db_size_mb.saturating_mul(1024 * 1024);
        let target = max_size.saturating_mul(9) / 10;
        log::info!(
            "GenAISqliteStore initialized: db_size={}MB, target={}MB, max={}MB, batch_max_size={}, batch_flush_ms={}",
            current_size / 1024 / 1024,
            target / 1024 / 1024,
            max_size / 1024 / 1024,
            store.batch_config.max_size,
            store.batch_config.flush_ms,
        );

        Ok(store)
    }

    /// Flush any buffered events to SQLite.
    ///
    /// Events are written through `store_event` which handles prune/retry.
    /// The batch value comes from reducing the number of flush calls (fewer
    /// fsync/WAL checkpoints), not from wrapping in a single transaction
    /// (which would require refactoring the Mutex-based conn access).
    pub fn flush(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.is_empty() {
            return;
        }
        let events: Vec<_> = pending.drain(..).collect();
        drop(pending); // release lock before writing

        let mut ok_count = 0usize;
        for event in &events {
            if let Err(e) = self.store_event(event) {
                log::warn!("Failed to store GenAI event in batch flush: {e}");
            } else {
                ok_count += 1;
            }
        }
        if ok_count > 0 {
            log::debug!("Batch-flushed {ok_count} GenAI events");
        }
        *self.last_flush.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// Check if batch flush is needed based on size or time.
    fn should_flush(&self, pending_len: usize) -> bool {
        if pending_len >= self.batch_config.max_size {
            return true;
        }
        let elapsed = self
            .last_flush
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed();
        elapsed >= Duration::from_millis(self.batch_config.flush_ms as u64)
    }

    /// Default database path
    pub fn default_path() -> PathBuf {
        crate::config::default_base_path().join("genai_events.db")
    }
}

impl Drop for GenAISqliteStore {
    fn drop(&mut self) {
        self.flush();
    }
}
