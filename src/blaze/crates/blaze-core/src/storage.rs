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
