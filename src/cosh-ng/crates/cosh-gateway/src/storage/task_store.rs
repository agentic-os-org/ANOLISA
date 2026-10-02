//! Atomic Task event, projection, receipt, and Outbox persistence.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, ContractHeader, ContractSchema, Correlation, Digest, IdempotencyKey,
};
use cosh_gateway_contracts::ids::{ActorId, CheckpointId, DeliveryId, MessageId, RunId, TaskId};
use cosh_gateway_contracts::task::{
    CancelReason, CancellationStage, TaskEvent, TaskEventEnvelope, TaskState,
};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::daemon::{TaskSnapshotKind, TaskSnapshotSwitchView, TaskSnapshotView};
use crate::task::TaskAggregate;

use super::{SqliteTaskStore, StoreError};

pub(crate) const MAX_TASK_EVENTS_PER_COMMIT: usize = 64;
pub(crate) const MAX_OUTBOX_INTENTS_PER_COMMIT: usize = 64;
pub(crate) const MAX_TASK_PAYLOAD_BYTES: usize = 256 * 1024;
pub(crate) const MAX_TASK_COMMIT_SERIALIZED_BYTES: usize = 1024 * 1024;

/// One durable delivery intent created by a Task event transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutboxIntent {
    /// Stable identity used to deduplicate downstream delivery.
    pub delivery_id: DeliveryId,
    /// Event in the same commit that caused this delivery.
    pub event_id: MessageId,
    /// Stable bounded delivery route.
    pub delivery_kind: BoundedName,
    /// Versioned delivery payload.
    pub payload: serde_json::Value,
    /// Earliest delivery attempt time in Unix milliseconds.
    pub next_attempt_at_ms: u64,
}

/// Fenced claim for one durable Outbox delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxClaim {
    /// Stable delivery identity.
    pub delivery_id: DeliveryId,
    /// Task that caused the delivery.
    pub task_id: TaskId,
    /// Event that caused the delivery.
    pub event_id: MessageId,
    /// Stable delivery route.
    pub delivery_kind: BoundedName,
    /// Versioned delivery payload.
    pub payload: serde_json::Value,
    /// Monotonic delivery attempt used to fence a stale worker.
    pub attempt: u64,
    /// Worker holding this delivery lease.
    pub lease_owner: BoundedOpaque,
    /// Delivery lease deadline in Unix milliseconds.
    pub lease_expires_at_ms: u64,
}

/// Read-only next-delivery snapshot used to validate before taking a lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboxCandidate {
    pub(crate) delivery_id: DeliveryId,
    pub(crate) task_id: TaskId,
    pub(crate) payload: serde_json::Value,
    pub(crate) attempt: u64,
}

/// Complete unit of work admitted by the single Task writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskCommit {
    /// Authenticated actor that owns the replay namespace.
    pub actor_id: ActorId,
    /// Caller-scoped command replay key.
    pub idempotency_key: IdempotencyKey,
    /// Canonical digest of the admitted command.
    pub command_digest: Digest,
    /// Optional optimistic revision precondition.
    pub expected_revision: Option<u64>,
    /// Consecutive Task events produced by the command.
    pub events: Vec<TaskEventEnvelope>,
    /// Delivery intents caused by events in this commit.
    pub outbox: Vec<OutboxIntent>,
    /// Durable commit timestamp in Unix milliseconds.
    pub committed_at_ms: u64,
}

/// Stable response persisted for exact idempotent replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReceipt {
    /// Task changed by the command.
    pub task_id: TaskId,
    /// Latest Task revision after the command.
    pub revision: u64,
    /// Task event identities committed by the command.
    pub event_ids: Vec<MessageId>,
    /// Outbox identities committed by the command.
    pub delivery_ids: Vec<DeliveryId>,
}

/// Result of admitting a command at the durable writer boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The command produced a new atomic commit.
    Applied(CommitReceipt),
    /// The same actor, key, and digest returned its durable receipt.
    Replayed(CommitReceipt),
}

// Commit, Outbox, and projection fragments share atomic persistence helpers;
// one namespace keeps those helpers private rather than creating a wider API.
include!("task_store/legacy_recovery.rs");
include!("task_store/outbox.rs");
include!("task_store/baseline.rs");
include!("task_store/approval_checkpoint.rs");
include!("task_store/snapshot.rs");
include!("task_store/commit.rs");
include!("task_store/task_load.rs");
include!("task_store/guard_helpers.rs");
include!("task_store/validation_helpers.rs");
include!("task_store/persistence_helpers.rs");
include!("task_store/value_helpers.rs");

#[cfg(test)]
mod tests;
