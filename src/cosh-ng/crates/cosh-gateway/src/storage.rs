//! SQLite-backed durable storage for the Gateway task plane.
//!
//! The store owns one write connection and commits task events, projections,
//! command receipts, and Outbox intents in one immediate transaction.
//!
//! The raw Task writer is intentionally not part of the production API:
//! ```compile_fail
//! let mut store = cosh_gateway::storage::SqliteTaskStore::open_in_memory().unwrap();
//! store.commit_task(todo!());
//! ```

mod backup;
#[cfg(debug_assertions)]
mod fault_harness;
mod inspect;
mod ledger;
mod schema;
mod sqlite;
mod task_store;

pub use inspect::{
    inspect_task_store, LegacyMarkerInspection, StoreCheckStatus, StoreInspection,
    StoreInspectionOutcome,
};
pub use ledger::{
    ApprovalRecord, ApprovalResolution, ApprovalState, BrokerExecutionState,
    BrokeredExecutionRecoveryReport, BrokeredExecutionResultRecord, BrokeredRequestRecord,
    BrokeredRuntimeDispatchKind, BrokeredRuntimeDispatchRecord, BrokeredRuntimeDispatchSource,
    BrokeredRuntimeDispatchState, ExecutionClaim, ExecutionCompletion, ExecutionRecord,
    ExecutionState, LeaseClaim, LeaseCommand, LedgerCommand, LedgerOutcome, PermitRecord,
    PermitState, ProviderPermissionDispatchDecision, ProviderPermissionDispatchRecord,
    ProviderPermissionDispatchState, RecoveryReport, RunLeaseRecord, RuntimeBindingRecord,
    RuntimeBindingState, SecurityAuditProof, TypedExecutionResultState,
};
pub(crate) use ledger::{
    RuntimeInputDispatchRecord, RuntimeInputDispatchState, RuntimeInputRequestRecord,
    RuntimeInputRequestState,
};
pub use sqlite::SqliteTaskStore;
pub(crate) use task_store::TaskSnapshotSwitchRecord;
pub use task_store::{CommitOutcome, CommitReceipt, OutboxClaim};
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use task_store::{OutboxIntent, TaskCommit};
#[cfg(not(debug_assertions))]
pub(crate) use task_store::{OutboxIntent, TaskCommit};

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::task::AggregateError;

/// Fail-closed storage errors exposed to Task coordination.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The configured database path or a companion file is unsafe.
    #[error("unsafe Gateway database path {path}: {message}")]
    UnsafePath {
        /// Path rejected before or during database open.
        path: PathBuf,
        /// Bounded developer-oriented reason.
        message: String,
    },
    /// A filesystem operation required for durable storage failed.
    #[error("Gateway storage I/O failed while {operation} at {path}: {source}")]
    Io {
        /// Stable operation name safe for diagnostics.
        operation: &'static str,
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
    /// The database uses a schema newer than this binary understands.
    #[error("Gateway database schema {found} is newer than supported schema {supported}")]
    NewerSchema {
        /// Schema version read from the database.
        found: u32,
        /// Highest version supported by this binary.
        supported: u32,
    },
    /// An already-applied migration does not match this binary.
    #[error("Gateway migration {version} checksum mismatch")]
    MigrationChecksum {
        /// Migration version whose content changed.
        version: u32,
    },
    /// A Task command reused an idempotency key with another digest.
    #[error("idempotency key was already used with a different command digest")]
    IdempotencyConflict,
    /// A command observed a Task revision different from its precondition.
    #[error("task revision conflict: expected {expected}, found {actual}")]
    RevisionConflict {
        /// Revision supplied by the command.
        expected: u64,
        /// Current durable revision.
        actual: u64,
    },
    /// A durable ledger entity does not exist.
    #[error("Gateway ledger entity not found: {entity}")]
    LedgerNotFound {
        /// Stable entity category and identifier safe for diagnostics.
        entity: String,
    },
    /// A successful pre-v8 execution has no reconstructible typed result payload.
    #[error("legacy brokered execution {execution_id} has no durable typed result")]
    LegacyBrokeredResultUnavailable {
        /// Successful execution migrated without inventing a result.
        execution_id: String,
    },
    /// A durable ledger transition violates a binding or lifecycle invariant.
    #[error("Gateway ledger conflict: {message}")]
    LedgerConflict {
        /// Bounded developer-oriented reason.
        message: String,
    },
    /// Runtime output belongs to a stale process generation.
    #[error("runtime generation fenced: expected {expected}, found {actual}")]
    GenerationFenced {
        /// Active durable runtime generation.
        expected: u64,
        /// Generation carried by the stale operation.
        actual: u64,
    },
    /// A requested Task does not exist.
    #[error("task not found")]
    TaskNotFound,
    /// A commit batch violates Task or Outbox invariants.
    #[error("invalid Gateway task commit: {message}")]
    InvalidCommit {
        /// Bounded developer-oriented reason.
        message: String,
    },
    /// A committed Task stream violates reducer invariants.
    #[error("Gateway task transition rejected: {0}")]
    Aggregate(#[from] AggregateError),
    /// Stored data violates the versioned Task contract.
    #[error("corrupt Gateway task storage: {message}")]
    Corrupt {
        /// Bounded decode or invariant detail.
        message: String,
    },
    /// SQLite rejected an operation.
    #[error("Gateway database operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A contract value could not be serialized or decoded.
    #[error("Gateway task contract serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl StoreError {
    /// Returns whether a caller can safely retry after refreshing state.
    pub fn recoverable(&self) -> bool {
        match self {
            Self::RevisionConflict { .. } | Self::TaskNotFound => true,
            Self::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ),
            _ => false,
        }
    }
}
