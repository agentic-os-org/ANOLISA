// SPDX-License-Identifier: Apache-2.0
//! Durable, provider-independent data-plane identities.
//!
//! These records define the portable side of the ownership ledger. Extension
//! implementations map their resource model into these identities so the daemon
//! can compare persisted intent with a provider inventory after restart.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Durable form of the stable identity chosen before a provider call.
///
/// Keeping the complete context in the public lifecycle record lets restart
/// recovery repeat an inspection without inventing a new idempotency key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPlaneRequestContextRecord {
    /// Public sandbox that owns the provider operation.
    pub instance_id: Uuid,
    /// Idempotency key for the provider call sequence.
    pub request_id: Uuid,
    /// Public lifecycle operation that owns the transition.
    pub operation_id: Uuid,
    /// Lease identity selected before the provider may create resources.
    pub lease_id: Uuid,
    /// Expected first lease generation.
    pub generation: u64,
}

/// Provider mutation whose result must remain recoverable across restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PendingProviderOperationKind {
    /// Prepare a fresh provider lease.
    PrepareLease,
    /// Retain a preparation whose returned ownership cannot be trusted.
    ///
    /// Inspecting the requested context cannot prove absence of resources
    /// described by an unsafe response. Operator resolution is required.
    UnresolvedPrepareLease,
    /// Capture immutable content for a public checkpoint.
    CheckpointCapture,
    /// Capture immutable content for hibernation.
    SuspensionCapture {
        /// Identity selected before suspension content may be created.
        suspension_id: Uuid,
    },
}

/// Write-ahead identity for one provider mutation.
///
/// This record intentionally contains only provider-independent identities and
/// logical extents. Provider-specific resource topology remains behind the
/// source-level provider contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProviderOperationRecord {
    /// Provider instance expected to inspect or compensate the operation.
    pub provider_instance_id: Uuid,
    /// Complete request identity used by inspection and idempotent retries.
    pub context: DataPlaneRequestContextRecord,
    /// Last accepted lease generation before the provider call started.
    pub generation_before_call: u64,
    /// Logical root-filesystem extent involved in the mutation.
    pub root_filesystem_bytes: u64,
    /// Logical guest-memory extent involved in the mutation.
    pub guest_memory_bytes: u64,
    /// Generic mutation being attempted.
    pub kind: PendingProviderOperationKind,
}

/// Durable lifecycle phase of one provider-owned resource lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataPlaneLeaseState {
    Prepared,
    Committed,
    Finalized,
    Stopped,
    Released,
    /// Resources are retained because ownership or safety cannot be proved.
    Quarantined,
}

/// Provider-independent identity of one resource lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPlaneLeaseRecord {
    /// Provider process or durable provider store that issued the lease.
    pub provider_instance_id: Uuid,
    /// Idempotency key of the preparation request.
    pub request_id: Uuid,
    /// Public lifecycle operation that owns the transition.
    pub operation_id: Uuid,
    /// Stable provider lease identity.
    pub lease_id: Uuid,
    /// Expected generation chosen before the first provider mutation.
    pub initial_generation: u64,
    /// Monotonic provider-side state generation.
    pub generation: u64,
    /// Last provider state accepted by Blaze.
    pub state: DataPlaneLeaseState,
    /// Logical root-filesystem extent promised to the backend.
    pub root_filesystem_bytes: u64,
    /// Logical guest-memory extent promised to the backend.
    pub guest_memory_bytes: u64,
}

/// Public identity of immutable provider content retained while a sandbox sleeps.
///
/// Extension implementations map retained content into the opaque identity and
/// integrity fields below. Blaze can therefore resume or retire the same object
/// without depending on one implementation's storage model. The presence of
/// this record denotes provider ownership of the complete writable-root and
/// guest-memory image; partial provider ownership is not supported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPlaneSuspensionRecord {
    /// Build-time provider instance that owns the immutable content.
    pub provider_instance_id: Uuid,
    /// Stable identity chosen before the provider may create suspension content.
    pub suspension_id: Uuid,
    /// Opaque provider-local immutable content reference.
    pub reference_id: Uuid,
    /// SHA-256 identity of the provider's canonical content manifest.
    pub content_digest: String,
    /// Active lease from which the suspension content was frozen.
    pub source_lease_id: Uuid,
    /// Source lease generation after the provider accepted suspension.
    pub source_generation: u64,
    /// Logical root-filesystem extent required by a fresh resume lease.
    pub root_filesystem_bytes: u64,
    /// Logical guest-memory extent required by a fresh resume lease.
    pub guest_memory_bytes: u64,
}

/// Linux process identity strong enough to detect PID reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendProcessIdentity {
    /// Process identifier observed when ownership was published.
    pub pid: u32,
    /// `/proc/<pid>/stat` start-time field, measured in clock ticks.
    pub start_time_ticks: u64,
}

/// Durable backend shape needed to adopt a live process after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendRuntimeRecord {
    /// Process identity; absent for backends that cannot be adopted.
    pub process: Option<BackendProcessIdentity>,
    /// Backend version frozen when the process started.
    pub version: Option<String>,
    /// Whether the owner exposes a guest-agent transport.
    pub guest_transport: bool,
    /// Whether the owner holds a host network slot.
    pub network_slot: bool,
    /// Whether console output is retained in the runtime directory.
    pub console_log: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspension_record_serializes_one_complete_owner() {
        let record = DataPlaneSuspensionRecord {
            provider_instance_id: Uuid::new_v4(),
            suspension_id: Uuid::new_v4(),
            reference_id: Uuid::new_v4(),
            content_digest: format!("sha256:{}", "a".repeat(64)),
            source_lease_id: Uuid::new_v4(),
            source_generation: 4,
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
        };

        let encoded = serde_json::to_value(record).expect("serialize suspension record");

        assert!(encoded.get("root_filesystem").is_none());
        assert!(encoded.get("guest_memory").is_none());
    }
}
