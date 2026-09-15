// SPDX-License-Identifier: Apache-2.0
//! Generic storage provider abstraction.
//!
//! Different providers may offer different performance characteristics
//! (copy-on-write, content-addressable dedup) but present
//! a uniform interface to the daemon layer.

use std::fs::File;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use thiserror::Error;

use crate::data_plane::{DataPlaneLeaseState, DataPlaneRequestContextRecord};
use crate::error::{BlazeError, Result};

/// A storage slot allocated for one sandbox instance.
///
/// This capability is runtime-only. Persist the stable `id`, then ask the
/// configured provider to reconstruct every path after restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageSlot {
    /// Stable identifier used to reconstruct paths after daemon restart.
    pub id: String,
    /// Writable root filesystem exposed to the backend.
    pub rootfs_path: PathBuf,
    /// Base or merged guest memory file exposed to the backend.
    pub mem_path: PathBuf,
    /// Cumulative memory delta relative to the base image.
    pub mem_diff_path: PathBuf,
    /// Cumulative root filesystem delta relative to the base image.
    pub rootfs_diff_path: PathBuf,
    /// Provider-owned directory containing all slot artifacts.
    pub instance_dir: PathBuf,
}

/// Durable lookup and recovery authorization for one storage slot.
///
/// A sandbox identifier alone is not an ownership proof: an unrelated process
/// may already have created a directory with the same name. Implementations
/// that support durable ownership must compare every field in this key before
/// recovery. Allocation and publication additionally compare the complete
/// immutable [`StorageOwnershipRequest`]. Removal additionally requires the
/// current durable lease state and generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageOwnershipKey {
    /// Stable provider instance that created the slot.
    pub provider_instance_id: uuid::Uuid,
    /// Complete request context persisted before the provider call.
    pub context: DataPlaneRequestContextRecord,
}

/// Immutable facts recorded when a new storage owner is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageOwnershipRequest {
    /// Provider and request identity that may recover the slot.
    pub key: StorageOwnershipKey,
    /// Logical root-filesystem extent promised by the prepare request.
    pub root_filesystem_bytes: u64,
    /// Logical guest-memory extent promised by the prepare request.
    pub guest_memory_bytes: u64,
    /// Provider-independent digest of the immutable prepare source.
    pub source_fingerprint: [u8; 32],
    /// VM-state length when the source requires a template restore payload.
    ///
    /// `None` identifies an ordinary image. `Some` requires a provider-owned
    /// `backend` directory containing the VM-state and memory payload files.
    pub template_vmstate_bytes: Option<u64>,
}

/// Filesystem identity of the concrete directory created for one slot.
///
/// This value comes from metadata on the opened directory object. A pathname
/// or caller-supplied token is not sufficient authorization for recursive
/// cleanup because an unrelated object can later occupy the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageSlotIdentity {
    /// Filesystem device containing the opened slot directory.
    pub device: u64,
    /// Inode number of the opened slot directory.
    pub inode: u64,
}

/// Durable phase of a daemon-managed storage ownership record.
///
/// The record lives outside the removable slot tree. This lets recovery prove
/// ownership before allocation starts and retain that proof until recursive
/// removal has been synchronized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StorageOwnershipPhase {
    /// Ownership was recorded before slot allocation began.
    Preparing,
    /// Every required slot artifact has been synchronized.
    Ready,
    /// Removal was authorized and may be resumed after interruption.
    Deleting,
}

/// Durable ownership claim returned by a storage provider.
///
/// `storage_domain` identifies the canonical configured storage root. It keeps
/// a copied or relocated manifest from authorizing access in a different
/// provider domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageOwnershipClaim {
    /// Immutable request facts published with the slot.
    pub request: StorageOwnershipRequest,
    /// Provider-defined digest of the canonical storage domain.
    pub storage_domain: [u8; 32],
    /// Concrete slot directory, once the provider has created and opened it.
    ///
    /// A preparing record may omit this field only while the slot name is
    /// absent. Ready records always contain it, and cleanup verifies it before
    /// touching the directory tree.
    pub slot_identity: Option<StorageSlotIdentity>,
    /// Current durable allocation or removal phase.
    pub phase: StorageOwnershipPhase,
    /// Last provider lease state durably accepted for this slot.
    pub state: DataPlaneLeaseState,
    /// Monotonic generation associated with `state`.
    pub generation: u64,
}

/// Reconstructed storage accompanied by its verified durable owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedStorageSlot {
    /// Runtime paths re-derived from the configured storage root.
    pub storage: StorageSlot,
    /// Claim read from and verified against durable storage.
    pub ownership: StorageOwnershipClaim,
}

/// Result of a ledger-wide ownership lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageOwnershipLookup {
    /// Neither the requested owner nor another owner with the same stable
    /// instance or lease identity exists.
    Absent,
    /// The complete ownership key matches a verified durable slot record.
    Owned(Box<OwnedStorageSlot>),
    /// A durable record reuses the requested instance or lease identity with
    /// a different context.
    Conflict,
}

/// Stable handle for one provider-owned rootfs restore transaction.
///
/// Callers must keep this handle from staging through activation and
/// finalization. Providers must validate both fields against durable state
/// before changing storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRestoreTransaction {
    /// Stable sandbox identifier whose rootfs is being replaced.
    pub instance_id: String,
    /// Unique transaction identifier used to reject stale handles.
    pub transaction_id: uuid::Uuid,
}

/// Storage provider capacity reported by the health endpoint.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PoolStatus {
    pub ready: usize,
    pub capacity: usize,
    pub pending: usize,
    /// Slots retained because cleanup must be retried.
    pub quarantined: usize,
}

/// Options for acquiring a storage slot.
#[derive(Debug, Clone)]
pub struct AcquireOpts {
    /// Stable sandbox identifier. Providers must reject path components.
    pub instance_id: String,
    /// Logical root filesystem size in bytes.
    pub rootfs_size: u64,
    /// Logical guest memory file size in bytes.
    pub mem_size: u64,
}

/// One already-open template artifact.
///
/// The open file object binds later materialization to the object the catalog
/// validated, even if its catalog path is replaced afterward.
#[derive(Debug)]
pub struct TemplateArtifact {
    /// Stable source object positioned at the beginning of the artifact.
    pub file: File,
    /// Exact byte length recorded by the template manifest.
    pub size_bytes: u64,
    /// Lowercase SHA-256 digest recorded by the template manifest.
    pub sha256: String,
}

/// Self-contained artifacts needed to restore one template.
#[derive(Debug)]
pub struct TemplateStorage {
    /// Backend VM-state snapshot.
    pub vmstate: TemplateArtifact,
    /// Guest-memory snapshot.
    pub memory: TemplateArtifact,
    /// Independent root filesystem snapshot.
    pub rootfs: TemplateArtifact,
}

/// Provider-owned storage produced from one template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateStorageSlot {
    /// Writable storage owned by the new sandbox.
    pub storage: StorageSlot,
    /// Provider-owned backend payload ready for the restore adapter.
    pub payload_dir: PathBuf,
}

/// Storage allocation failure with an optional residual slot owner.
///
/// A provider returns `residual` only when rollback could not remove resources
/// that were created for this request. The caller must retain the stable slot
/// ID until a later release succeeds.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct StorageAcquireError {
    #[source]
    source: BlazeError,
    residual: Option<StorageSlot>,
}

impl StorageAcquireError {
    /// Build a failure after the provider confirmed that no resources remain.
    pub fn clean(source: BlazeError) -> Self {
        Self {
            source,
            residual: None,
        }
    }

    /// Build a failure that transfers residual slot ownership to the caller.
    pub fn with_residual(source: BlazeError, residual: StorageSlot) -> Self {
        Self {
            source,
            residual: Some(residual),
        }
    }

    /// Split the original provider error from any residual slot owner.
    pub fn into_parts(self) -> (BlazeError, Option<StorageSlot>) {
        (self.source, self.residual)
    }
}

impl From<BlazeError> for StorageAcquireError {
    fn from(source: BlazeError) -> Self {
        Self::clean(source)
    }
}

/// Generic storage backend trait.
#[async_trait]
pub trait StorageProvider: Send + Sync {
    /// Return the stable identity of this storage ownership domain.
    ///
    /// Providers without daemon-managed ownership may return `None`. A
    /// provider that returns an identity must keep it stable across process
    /// restart and distinct for storage roots that cannot adopt each other's
    /// resources.
    fn ownership_domain_id(&self) -> Result<Option<uuid::Uuid>> {
        Ok(None)
    }

    /// Probe whether this provider is available in the current environment.
    async fn probe(&self) -> Result<bool>;

    /// Acquire a storage slot for one sandbox.
    async fn acquire(
        &self,
        opts: &AcquireOpts,
    ) -> std::result::Result<StorageSlot, StorageAcquireError>;

    /// Materialize a self-contained template into a new owned slot.
    ///
    /// Providers must not retain paths into the catalog. Every artifact used
    /// by the restored sandbox must be copied into provider-owned storage.
    async fn acquire_template(
        &self,
        opts: &AcquireOpts,
        source: TemplateStorage,
    ) -> std::result::Result<TemplateStorageSlot, StorageAcquireError> {
        let _ = (opts, source);
        Err(StorageAcquireError::clean(BlazeError::StorageError {
            msg: "storage provider does not support templates".to_string(),
        }))
    }

    /// Report whether template materialization is implemented.
    ///
    /// The default is conservative so existing providers do not advertise a
    /// data path they have not implemented.
    fn supports_templates(&self) -> bool {
        false
    }

    /// Release a storage slot (cleanup all associated resources).
    async fn release(&self, slot: StorageSlot) -> Result<()>;

    /// Release a slot using only its stable identifier during crash recovery.
    ///
    /// Providers whose `release` operation is idempotent for a missing slot
    /// should override this method. The default requires reconstruction first.
    async fn release_by_id(&self, instance_id: &str) -> Result<()> {
        let slot = self.reconstruct(instance_id).await?;
        self.release(slot).await
    }

    /// Reconstruct a previously allocated slot from a stable instance id.
    ///
    /// Implementations must derive every returned path from their configured
    /// root and must not trust persisted path strings.
    async fn reconstruct(&self, instance_id: &str) -> Result<StorageSlot>;

    /// Write the durable owner before allocation can create a slot directory.
    ///
    /// Repeating an identical reservation is allowed. A different immutable
    /// request for the same sandbox identifier must fail closed.
    async fn reserve_ownership(
        &self,
        request: StorageOwnershipRequest,
    ) -> Result<StorageOwnershipClaim> {
        let _ = request;
        Err(durable_ownership_unsupported())
    }

    /// Atomically mark a reserved owner ready after synchronizing its slot.
    ///
    /// The default fails closed. Providers must implement this method together
    /// with [`Self::reconstruct_owned`] and [`Self::release_owned`] before a
    /// caller may rely on request-scoped crash recovery.
    async fn publish_ownership(
        &self,
        slot: &StorageSlot,
        request: StorageOwnershipRequest,
    ) -> Result<StorageOwnershipClaim> {
        let _ = (slot, request);
        Err(durable_ownership_unsupported())
    }

    /// Durably advance one ready slot's exact lease state and generation.
    async fn advance_ownership(
        &self,
        key: StorageOwnershipKey,
        expected_state: DataPlaneLeaseState,
        expected_generation: u64,
        next_state: DataPlaneLeaseState,
        next_generation: u64,
    ) -> Result<StorageOwnershipClaim> {
        let _ = (
            key,
            expected_state,
            expected_generation,
            next_state,
            next_generation,
        );
        Err(durable_ownership_unsupported())
    }

    /// Reconstruct ownership only when its durable recovery key matches `key`.
    ///
    /// Preparing and deleting records may describe an absent or partial slot so
    /// recovery can finish allocation cleanup. `Ok(None)` is reserved for the
    /// proven absence of both the ownership record and slot directory. Missing,
    /// unreadable, corrupt, or mismatched metadata must be errors.
    async fn reconstruct_owned(
        &self,
        key: StorageOwnershipKey,
    ) -> Result<Option<OwnedStorageSlot>> {
        let _ = key;
        Err(durable_ownership_unsupported())
    }

    /// Search the complete durable ownership index for `key`.
    ///
    /// The default preserves compatibility for providers whose keys are
    /// already globally unique. Filesystem providers should override this to
    /// detect a lease identifier reused under another instance after restart.
    async fn lookup_ownership(&self, key: StorageOwnershipKey) -> Result<StorageOwnershipLookup> {
        Ok(match self.reconstruct_owned(key).await? {
            Some(owned) => StorageOwnershipLookup::Owned(Box::new(owned)),
            None => StorageOwnershipLookup::Absent,
        })
    }

    /// Remove a slot only after verifying its recovery key and current lease.
    ///
    /// Returns `false` only when the provider proves that both the ownership
    /// record and slot directory are absent. Ambiguous or mismatched state must
    /// remain an error and retained.
    async fn release_owned(
        &self,
        key: StorageOwnershipKey,
        expected_state: DataPlaneLeaseState,
        expected_generation: u64,
    ) -> Result<bool> {
        let _ = (key, expected_state, expected_generation);
        Err(durable_ownership_unsupported())
    }

    /// Synchronize already-written provider artifacts to persistent storage.
    ///
    /// This operation persists the files and directory metadata that already
    /// belong to `slot` and are visible to the provider call. Artifact updates
    /// that race with one call may become visible in that call or a later one.
    ///
    /// The daemon may stop waiting at its configured deadline, but keeps the
    /// future supervised under slot ownership until it completes. A later
    /// synchronization or cleanup must remain safe after completion.
    async fn sync_artifacts(&self, slot: &StorageSlot) -> Result<()>;

    /// Report whether this provider can capture a self-contained checkpoint.
    ///
    /// The default is conservative so existing providers do not advertise a
    /// data path they have not implemented.
    fn supports_checkpoint_capture(&self) -> bool {
        false
    }

    /// Capture the slot's writable root filesystem at `target`.
    async fn capture_checkpoint(&self, slot: &StorageSlot, target: &Path) -> Result<()> {
        let _ = (slot, target);
        Err(BlazeError::StorageError {
            msg: "storage provider does not support checkpoint capture".to_string(),
        })
    }

    /// Report whether this provider can restore a self-contained checkpoint.
    ///
    /// The default is conservative so existing providers cannot enter a
    /// partially implemented replacement flow.
    fn supports_checkpoint_restore(&self) -> bool {
        false
    }

    /// Copy a checkpoint rootfs into provider-owned staging storage.
    ///
    /// Staging must leave the live rootfs unchanged so callers may prepare the
    /// replacement before stopping the current runtime.
    async fn stage_checkpoint_restore(
        &self,
        slot: &StorageSlot,
        source: &Path,
    ) -> Result<StorageRestoreTransaction> {
        let _ = (slot, source);
        Err(checkpoint_restore_unsupported())
    }

    /// Select the staged rootfs while retaining the previous rootfs.
    ///
    /// A successful activation must remain abortable until
    /// [`Self::commit_checkpoint_restore`] starts.
    async fn activate_checkpoint_restore(
        &self,
        transaction: &StorageRestoreTransaction,
    ) -> Result<()> {
        let _ = transaction;
        Err(checkpoint_restore_unsupported())
    }

    /// Finalize an activated rootfs and release its retained predecessor.
    async fn commit_checkpoint_restore(
        &self,
        transaction: &StorageRestoreTransaction,
    ) -> Result<()> {
        let _ = transaction;
        Err(checkpoint_restore_unsupported())
    }

    /// Restore the predecessor retained by a staged or activated transaction.
    async fn abort_checkpoint_restore(
        &self,
        transaction: &StorageRestoreTransaction,
    ) -> Result<()> {
        let _ = transaction;
        Err(checkpoint_restore_unsupported())
    }

    /// Resolve an interrupted restore transaction after process restart.
    ///
    /// Implementations choose the outcome from durable transaction state:
    /// work not yet committed should roll back, while a durable commit intent
    /// should finish committing.
    async fn reconcile_checkpoint_restore(&self, instance_id: &str) -> Result<()> {
        let _ = instance_id;
        Err(checkpoint_restore_unsupported())
    }

    /// Return the provider's current storage capacity.
    fn pool_status(&self) -> PoolStatus;
}

fn checkpoint_restore_unsupported() -> BlazeError {
    BlazeError::StorageError {
        msg: "storage provider does not support checkpoint restore".to_string(),
    }
}

fn durable_ownership_unsupported() -> BlazeError {
    BlazeError::StorageError {
        msg: "storage provider does not support durable ownership claims".to_string(),
    }
}
