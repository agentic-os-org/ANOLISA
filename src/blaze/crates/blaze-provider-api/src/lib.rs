// SPDX-License-Identifier: Apache-2.0
//! Source-level contract for data-plane implementations composed with Blaze at build time.
//!
//! Downstream crates can implement this contract to integrate custom storage
//! and restore resources without modifying the Blaze source tree.
//!
//! The contract is a Rust source interface, not a stable dynamic-library ABI.
//! A provider and the daemon that consumes it must be built with a compatible
//! source revision, Rust toolchain, and dependency lock.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::os::fd::OwnedFd;
use std::path::PathBuf;

use async_trait::async_trait;
use blaze_core::backend::BackendKind;
use blaze_core::checkpoint::ProviderCheckpointRecord;
use blaze_core::data_plane::{
    BackendProcessIdentity, DataPlaneLeaseRecord, DataPlaneLeaseState,
    DataPlaneRequestContextRecord, DataPlaneSuspensionRecord,
};
use blaze_core::storage::{StorageSlot, TemplateStorage};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// First source-level provider contract understood by Blaze.
pub const PROVIDER_CONTRACT_VERSION: u16 = 1;

/// Maximum UTF-8 byte length of an inventory continuation cursor.
///
/// Cursors are opaque to Blaze, but bounding them prevents an untrusted
/// provider from growing daemon memory without limit during restart.
pub const MAX_INVENTORY_CURSOR_BYTES: usize = 4 * 1024;

/// Maximum number of pages Blaze accepts from one frozen inventory.
///
/// Providers must put at least one lease in every non-final page, so this
/// bound also limits traversals whose cursors never repeat.
pub const MAX_INVENTORY_PAGES: usize = 4 * 1024;

/// Opaque identity and contract revision of one provider instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    /// Contract revision implemented by this provider.
    pub contract_version: u16,
    /// Stable identity used to reject a response from another provider.
    pub provider_instance_id: Uuid,
}

/// Operations implemented by one provider build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Prepare an ordinary image-backed sandbox.
    pub images: bool,
    /// Prepare a sandbox from a published runtime template.
    pub templates: bool,
    /// A template preparation may return typed, already-opened resources.
    pub opened_template_restore_resources: bool,
    /// A checkpoint restore may return typed, already-opened resources.
    pub opened_checkpoint_restore_resources: bool,
    /// A suspension resume may return typed, already-opened resources.
    pub opened_suspension_restore_resources: bool,
    /// Allow Blaze to manage path-backed resources through its configured
    /// `StorageProvider`.
    ///
    /// Set this only when every `PreparedResources::PathBacked` value returned
    /// by this provider belongs to that storage provider and remains
    /// reconstructible by sandbox identifier. Blaze may then use its standard
    /// synchronization, checkpoint, hibernation, restore, and release paths
    /// when no provider-specific lifecycle extension is selected.
    pub daemon_managed_storage: bool,
}

/// Stable identifiers chosen before a provider may create resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestContext {
    /// Public sandbox that will own the resources.
    pub instance_id: Uuid,
    /// Idempotency key for this provider call sequence.
    pub request_id: Uuid,
    /// One public lifecycle operation spanning all provider transitions.
    pub operation_id: Uuid,
    /// Preselected lease identity, including calls whose result is unknown.
    pub lease_id: Uuid,
    /// Expected first lease generation.
    pub generation: u64,
}

impl From<RequestContext> for DataPlaneRequestContextRecord {
    fn from(context: RequestContext) -> Self {
        Self {
            instance_id: context.instance_id,
            request_id: context.request_id,
            operation_id: context.operation_id,
            lease_id: context.lease_id,
            generation: context.generation,
        }
    }
}

impl From<DataPlaneRequestContextRecord> for RequestContext {
    fn from(context: DataPlaneRequestContextRecord) -> Self {
        Self {
            instance_id: context.instance_id,
            request_id: context.request_id,
            operation_id: context.operation_id,
            lease_id: context.lease_id,
            generation: context.generation,
        }
    }
}

/// Immutable source selected by the public control operation.
#[derive(Debug)]
pub enum PrepareSource {
    /// Allocate writable resources for an ordinary image identity.
    Image {
        /// Image identity already accepted by policy evaluation.
        image_digest: String,
    },
    /// Materialize one already-validated runtime template.
    Template(TemplateSource),
}

impl PrepareSource {
    /// Return a stable digest of the immutable source selected for preparation.
    ///
    /// The digest is independent of Rust layout and does not include opened
    /// file descriptors or local paths. A storage provider can therefore bind
    /// an idempotent prepare request to the same public image or validated
    /// template across daemon restarts.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"blaze.prepare-source.v1\0");
        match self {
            Self::Image { image_digest } => {
                hasher.update(b"image\0");
                hash_length_prefixed(&mut hasher, image_digest.as_bytes());
            }
            Self::Template(source) => {
                hasher.update(b"template\0");
                hash_length_prefixed(&mut hasher, source.image_digest.as_bytes());
                for (role, artifact) in [
                    (b"vm-state".as_slice(), &source.storage.vmstate),
                    (b"guest-memory".as_slice(), &source.storage.memory),
                    (b"root-filesystem".as_slice(), &source.storage.rootfs),
                ] {
                    hash_length_prefixed(&mut hasher, role);
                    hasher.update(artifact.size_bytes.to_be_bytes());
                    hash_length_prefixed(&mut hasher, artifact.sha256.as_bytes());
                }
            }
        }
        hasher.finalize().into()
    }
}

fn hash_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

/// Opened template artifacts and their public image identity.
#[derive(Debug)]
pub struct TemplateSource {
    /// Image identity recorded by the validated template manifest.
    pub image_digest: String,
    /// Opened VM state, memory, and root-filesystem artifacts.
    pub storage: TemplateStorage,
}

/// Request to prepare all provider-owned resources for one sandbox.
#[derive(Debug)]
pub struct PrepareRequest {
    /// Stable idempotency and ownership context.
    pub context: RequestContext,
    /// Ordinary image or validated template input.
    pub source: PrepareSource,
    /// Required logical root-filesystem extent.
    pub root_filesystem_bytes: u64,
    /// Required logical guest-memory extent.
    pub guest_memory_bytes: u64,
}

/// Backend-visible purpose of one opened attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentRole {
    /// Writable root drive required by captured virtual-machine state.
    RootDrive,
    /// Writable guest-memory backend consumed by snapshot loading.
    GuestMemory,
}

/// Filesystem object kind expected from descriptor metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// Ordinary file.
    RegularFile,
    /// Character device.
    CharacterDevice,
    /// Block device.
    BlockDevice,
}

/// One ownership-transferring resource attachment.
#[derive(Debug)]
pub struct OpenedAttachment {
    /// Purpose understood by the backend restore adapter.
    pub role: AttachmentRole,
    /// Read-write descriptor transferred exclusively to one backend owner.
    pub descriptor: OwnedFd,
    /// Declared object kind, checked again by Blaze.
    pub kind: AttachmentKind,
    /// Logical extent exposed to the backend.
    pub logical_size_bytes: u64,
    /// Pre-provisioned path required by captured backend state, if any.
    pub consumer_path: Option<PathBuf>,
}

/// Runtime resources produced by preparation.
#[derive(Debug)]
pub enum PreparedResources {
    /// Existing file-backed runtime layout.
    PathBacked {
        /// Writable storage slot owned by this lease.
        storage: StorageSlot,
        /// Provider-owned restore payload for template preparation.
        restore_payload_dir: Option<PathBuf>,
    },
    /// Restore payload plus already-opened resources transferred by descriptor.
    OpenedRestore {
        /// Provider-owned directory containing backend VM-state payload.
        restore_payload_dir: PathBuf,
        /// Root-drive and guest-memory descriptors transferred to Blaze.
        attachments: Vec<OpenedAttachment>,
    },
    /// Resources for an in-place restore whose backend payload remains in the
    /// daemon checkpoint catalog.
    CheckpointRestore {
        /// Path-backed replacement slot, when the provider exposes files.
        storage: Option<StorageSlot>,
        /// Opened root-drive and guest-memory attachments otherwise.
        attachments: Vec<OpenedAttachment>,
    },
    /// Resources for resuming from immutable provider-owned suspension content.
    SuspensionRestore {
        /// Path-backed replacement slot, when the provider exposes files.
        storage: Option<StorageSlot>,
        /// Opened root-drive and guest-memory attachments otherwise.
        attachments: Vec<OpenedAttachment>,
    },
}

/// Durable phase of one provider resource lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    /// Resources exist but the backend has not reached readiness.
    Prepared,
    /// Backend readiness was accepted by the provider.
    Committed,
    /// Public state was persisted and final ownership was handed over.
    Finalized,
    /// Backend use ended while provider resources remain retained.
    Stopped,
    /// Provider proved that all lease resources are absent.
    Released,
    /// Resources are retained until an operator resolves an ownership conflict.
    Quarantined,
}

/// Exact identity and state of one provider-owned resource lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseBinding {
    /// Provider that issued this binding.
    pub provider_instance_id: Uuid,
    /// Stable idempotency and public ownership context.
    pub context: RequestContext,
    /// Monotonic provider state generation.
    pub generation: u64,
    /// Current provider-side state.
    pub state: LeaseState,
}

impl LeaseBinding {
    /// Convert a provider response into the implementation-neutral durable ledger.
    pub fn to_record(
        self,
        root_filesystem_bytes: u64,
        guest_memory_bytes: u64,
    ) -> DataPlaneLeaseRecord {
        DataPlaneLeaseRecord {
            provider_instance_id: self.provider_instance_id,
            request_id: self.context.request_id,
            operation_id: self.context.operation_id,
            lease_id: self.context.lease_id,
            initial_generation: self.context.generation,
            generation: self.generation,
            state: self.state.into(),
            root_filesystem_bytes,
            guest_memory_bytes,
        }
    }

    /// Rebuild a provider binding from one sandbox's durable ledger record.
    pub fn from_record(instance_id: Uuid, record: DataPlaneLeaseRecord) -> Self {
        Self {
            provider_instance_id: record.provider_instance_id,
            context: RequestContext {
                instance_id,
                request_id: record.request_id,
                operation_id: record.operation_id,
                lease_id: record.lease_id,
                generation: record.initial_generation,
            },
            generation: record.generation,
            state: record.state.into(),
        }
    }
}

impl From<LeaseState> for DataPlaneLeaseState {
    fn from(state: LeaseState) -> Self {
        match state {
            LeaseState::Prepared => Self::Prepared,
            LeaseState::Committed => Self::Committed,
            LeaseState::Finalized => Self::Finalized,
            LeaseState::Stopped => Self::Stopped,
            LeaseState::Released => Self::Released,
            LeaseState::Quarantined => Self::Quarantined,
        }
    }
}

impl From<DataPlaneLeaseState> for LeaseState {
    fn from(state: DataPlaneLeaseState) -> Self {
        match state {
            DataPlaneLeaseState::Prepared => Self::Prepared,
            DataPlaneLeaseState::Committed => Self::Committed,
            DataPlaneLeaseState::Finalized => Self::Finalized,
            DataPlaneLeaseState::Stopped => Self::Stopped,
            DataPlaneLeaseState::Released => Self::Released,
            DataPlaneLeaseState::Quarantined => Self::Quarantined,
        }
    }
}

/// Prepared resources and their exact lease binding.
#[derive(Debug)]
pub struct PreparedLease {
    /// Binding confirmed by the provider.
    pub binding: LeaseBinding,
    /// Backend resources owned by this binding.
    pub resources: PreparedResources,
}

/// Read-only query for a preselected or returned lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectRequest {
    /// Stable request and lease identifiers.
    pub context: RequestContext,
}

/// State observed without changing provider resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedLease {
    /// Current exact provider binding.
    pub binding: LeaseBinding,
}

/// Mark a prepared backend as ready for public state publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitRequest {
    /// Prepared lease being committed.
    pub binding: LeaseBinding,
}

/// Provider-side result ready for public state publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedLease {
    /// Binding advanced to [`LeaseState::Committed`].
    pub binding: LeaseBinding,
}

/// Reference to the public transition persisted by Blaze.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicTransitionRef {
    /// Sandbox whose public state was changed.
    pub instance_id: Uuid,
    /// Lifecycle operation that published the state.
    pub operation_id: Uuid,
}

/// Complete provider handoff after public state is durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizeRequest {
    /// Committed provider binding.
    pub binding: LeaseBinding,
    /// Matching durable public transition.
    pub public_transition: PublicTransitionRef,
}

/// Final provider ownership returned to the manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizedLease {
    /// Binding advanced to [`LeaseState::Finalized`].
    pub binding: LeaseBinding,
}

/// Compensate a preparation that has no durable public transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbortRequest {
    /// Prepared or committed binding to release.
    pub binding: LeaseBinding,
}

/// Confirmed result of preparation compensation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbortResult {
    /// Binding advanced to [`LeaseState::Released`].
    pub binding: LeaseBinding,
}

/// End active backend use while retaining provider cleanup state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopRequest {
    /// Finalized binding whose backend has stopped.
    pub binding: LeaseBinding,
}

/// Provider result after active use ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoppedLease {
    /// Binding advanced to [`LeaseState::Stopped`].
    pub binding: LeaseBinding,
}

/// Release all resources after backend termination is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseRequest {
    /// Stopped binding to release.
    pub binding: LeaseBinding,
}

/// Confirmed terminal result of provider cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseResult {
    /// Binding advanced to [`LeaseState::Released`].
    pub binding: LeaseBinding,
}

/// Opaque, immutable provider content paired with one public checkpoint.
///
/// The presence of this reference means the provider owns the complete
/// data-plane image: both the writable root filesystem and guest memory.
/// Partial provider ownership is not supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCheckpointRef {
    /// Build-time provider instance that owns the immutable content.
    pub provider_instance_id: Uuid,
    /// UUID portion of the public checkpoint that owns this reference.
    pub public_checkpoint_id: Uuid,
    /// Opaque provider-local immutable content reference.
    pub reference_id: Uuid,
    /// SHA-256 identity of the provider's canonical content manifest.
    pub content_digest: String,
    /// Opaque provider reference of the public parent checkpoint.
    pub parent_reference_id: Option<Uuid>,
    /// Finalized lease frozen by this capture.
    pub source_lease_id: Uuid,
    /// Source lease generation after the provider accepted capture.
    pub source_generation: u64,
}

impl ProviderCheckpointRef {
    /// Convert the result into an implementation-neutral durable ownership record.
    pub fn to_record(&self) -> ProviderCheckpointRecord {
        ProviderCheckpointRecord {
            provider_instance_id: self.provider_instance_id,
            public_checkpoint_id: self.public_checkpoint_id,
            reference_id: self.reference_id,
            content_digest: self.content_digest.clone(),
            parent_reference_id: self.parent_reference_id,
            source_lease_id: self.source_lease_id,
            source_generation: self.source_generation,
        }
    }

    /// Reconstruct the source-level provider reference from a validated ledger.
    pub fn from_record(record: &ProviderCheckpointRecord) -> Self {
        Self {
            provider_instance_id: record.provider_instance_id,
            public_checkpoint_id: record.public_checkpoint_id,
            reference_id: record.reference_id,
            content_digest: record.content_digest.clone(),
            parent_reference_id: record.parent_reference_id,
            source_lease_id: record.source_lease_id,
            source_generation: record.source_generation,
        }
    }
}

/// Freeze provider-owned state while the backend is quiesced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCheckpointRequest {
    /// Finalized active lease captured while backend writes are stopped.
    pub binding: LeaseBinding,
    /// UUID portion of the already allocated public `ckpt-...` identity.
    pub checkpoint_id: Uuid,
    /// Exact parent reference selected from the public checkpoint head.
    pub parent: Option<ProviderCheckpointRef>,
}

/// Provider result for one immutable checkpoint capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointSubmission {
    /// Active lease advanced by exactly one generation and still finalized.
    pub binding: LeaseBinding,
    /// Immutable content reference paired with daemon-owned backend artifacts.
    pub checkpoint: ProviderCheckpointRef,
}

/// Prepare an independent replacement lease from immutable checkpoint data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreCheckpointRequest {
    /// Fresh identity for the independent replacement lease.
    pub context: RequestContext,
    /// Immutable provider content selected by the verified public catalog.
    pub checkpoint: ProviderCheckpointRef,
    /// Required writable root-filesystem extent.
    pub root_filesystem_bytes: u64,
    /// Required guest-memory extent.
    pub guest_memory_bytes: u64,
}

/// Retire provider content after the public catalog no longer references it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetireCheckpointRequest {
    /// Build-time provider instance expected to own the content.
    pub provider_instance_id: Uuid,
    /// Public checkpoint identity used to make unknown captures idempotent.
    pub public_checkpoint_id: Uuid,
    /// Absent only when capture had an unknown outcome before a reference was returned.
    pub reference_id: Option<Uuid>,
    /// Stable idempotency identity derived from the complete public owner tuple.
    ///
    /// Repeating retirement after a daemon restart uses the same value.
    pub operation_id: Uuid,
}

/// Confirm that a provider checkpoint reference no longer owns content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetireCheckpointResult {
    /// Public checkpoint identity accepted by the provider.
    pub public_checkpoint_id: Uuid,
    /// Exact opaque reference retired, or absent for an unknown capture.
    pub reference_id: Option<Uuid>,
    /// True when content was removed by this call, false when already absent.
    pub retired: bool,
}

/// Opaque, immutable provider content retained while a sandbox is hibernated.
///
/// The presence of this reference means the provider owns the complete
/// data-plane image: both the writable root filesystem and guest memory.
/// Partial provider ownership is not supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSuspensionRef {
    /// Build-time provider instance that owns the immutable content.
    pub provider_instance_id: Uuid,
    /// Stable identity selected before suspension can create content.
    pub suspension_id: Uuid,
    /// Opaque provider-local immutable content reference.
    pub reference_id: Uuid,
    /// SHA-256 identity of the provider's canonical content manifest.
    pub content_digest: String,
    /// Finalized lease frozen by this suspension.
    pub source_lease_id: Uuid,
    /// Source lease generation after the provider accepted suspension.
    pub source_generation: u64,
    /// Logical root-filesystem extent required by a fresh resume lease.
    pub root_filesystem_bytes: u64,
    /// Logical guest-memory extent required by a fresh resume lease.
    pub guest_memory_bytes: u64,
}

impl ProviderSuspensionRef {
    /// Convert the result into an implementation-neutral durable ownership record.
    pub fn to_record(&self) -> DataPlaneSuspensionRecord {
        DataPlaneSuspensionRecord {
            provider_instance_id: self.provider_instance_id,
            suspension_id: self.suspension_id,
            reference_id: self.reference_id,
            content_digest: self.content_digest.clone(),
            source_lease_id: self.source_lease_id,
            source_generation: self.source_generation,
            root_filesystem_bytes: self.root_filesystem_bytes,
            guest_memory_bytes: self.guest_memory_bytes,
        }
    }

    /// Reconstruct the source-level provider reference from a durable ledger.
    pub fn from_record(record: &DataPlaneSuspensionRecord) -> Self {
        Self {
            provider_instance_id: record.provider_instance_id,
            suspension_id: record.suspension_id,
            reference_id: record.reference_id,
            content_digest: record.content_digest.clone(),
            source_lease_id: record.source_lease_id,
            source_generation: record.source_generation,
            root_filesystem_bytes: record.root_filesystem_bytes,
            guest_memory_bytes: record.guest_memory_bytes,
        }
    }
}

/// Freeze provider-owned state while the backend is quiesced for hibernation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuspendRequest {
    /// Finalized active lease captured while backend writes are stopped.
    pub binding: LeaseBinding,
    /// Stable identity selected before any provider-side mutation.
    pub suspension_id: Uuid,
    /// Logical root-filesystem extent retained by the suspension.
    pub root_filesystem_bytes: u64,
    /// Logical guest-memory extent retained by the suspension.
    pub guest_memory_bytes: u64,
}

/// Provider result for one immutable suspension capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspensionSubmission {
    /// Active lease advanced by exactly one generation and still finalized.
    pub binding: LeaseBinding,
    /// Immutable content retained after the active lease is released.
    pub suspension: ProviderSuspensionRef,
}

/// Prepare a fresh exclusive lease from immutable suspension content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRequest {
    /// Fresh identity for the independent replacement lease.
    pub context: RequestContext,
    /// Immutable provider content selected by the verified hibernation image.
    pub suspension: ProviderSuspensionRef,
    /// Required writable root-filesystem extent.
    pub root_filesystem_bytes: u64,
    /// Required guest-memory extent.
    pub guest_memory_bytes: u64,
}

/// Retire suspension content after no durable hibernation image references it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetireSuspensionRequest {
    /// Build-time provider instance expected to own the content.
    pub provider_instance_id: Uuid,
    /// Stable suspension identity used to make unknown captures idempotent.
    pub suspension_id: Uuid,
    /// Absent only when capture had an unknown outcome before a reference returned.
    pub reference_id: Option<Uuid>,
    /// Stable idempotency identity derived from the complete public owner tuple.
    ///
    /// Repeating retirement after a daemon restart uses the same value.
    pub operation_id: Uuid,
}

/// Confirm that a provider suspension reference no longer owns content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetireSuspensionResult {
    /// Suspension identity accepted by the provider.
    pub suspension_id: Uuid,
    /// Exact opaque reference retired, or absent for an unknown capture.
    pub reference_id: Option<Uuid>,
    /// True when content was removed by this call, false when already absent.
    pub retired: bool,
}

/// Provider-independent requirements shared by every resource in one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapacityClass {
    /// Largest logical root filesystem one resource in this class can serve.
    pub root_filesystem_capacity_bytes: u64,
    /// Largest logical guest-memory image one resource in this class can serve.
    pub guest_memory_capacity_bytes: u64,
}

impl CapacityClass {
    /// Derive the stable public identity of this exact requirement pair.
    ///
    /// Domain separation and big-endian integers make the digest independent
    /// of Rust layout, host endianness, and extension-defined labels.
    pub fn digest(self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"blaze.data-plane-capacity.v1\0");
        hasher.update(self.root_filesystem_capacity_bytes.to_be_bytes());
        hasher.update(self.guest_memory_capacity_bytes.to_be_bytes());
        hasher.finalize().into()
    }
}

/// Public partition whose reusable data-plane capacity is being observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapacityScope {
    /// Runtime backend that will consume resources from this partition.
    pub backend: BackendKind,
    /// Digest of the provider-independent capacity requirements.
    pub class_digest: [u8; 32],
}

/// Read-only request for one exact capacity partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityRequest {
    /// Backend and resource-class partition selected by the caller.
    pub scope: CapacityScope,
}

/// Mutually exclusive public states of reusable provider resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacitySnapshot {
    /// Build-time provider instance that produced this snapshot.
    pub provider_instance_id: Uuid,
    /// Exact partition represented by every count below.
    pub scope: CapacityScope,
    /// Public requirements whose digest identifies the selected class.
    pub class: CapacityClass,
    /// Monotonic partition revision used to order observations.
    pub revision: u64,
    /// Idle resources that are safe to claim.
    pub ready: u64,
    /// Resources still being created or verified.
    pub building: u64,
    /// Resources exclusively held by active leases and not marked for drain.
    pub in_use: u64,
    /// Resources that cannot be reused and will be removed when safe.
    pub draining: u64,
    /// Resources retained outside allocation until an operator resolves them.
    pub quarantined: u64,
    /// Whether `prepare` may claim capacity from this partition.
    pub accepting_allocations: bool,
}

impl CapacitySnapshot {
    /// Return the total accounted resources, or `None` on integer overflow.
    pub fn checked_total(self) -> Option<u64> {
        self.ready
            .checked_add(self.building)?
            .checked_add(self.in_use)?
            .checked_add(self.draining)?
            .checked_add(self.quarantined)
    }
}

/// Idempotent request to stop reusing and eventually remove one partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainRequest {
    /// Partition to drain without affecting any other partition.
    pub scope: CapacityScope,
    /// Stable identity reused when a drain result is unknown.
    pub operation_id: Uuid,
}

/// Provider-confirmed result of one drain request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainResult {
    /// Stable request identity echoed by the provider.
    pub operation_id: Uuid,
    /// Idle resources removed before this call returned.
    pub removed_ready: u64,
    /// Active resources that will be removed only after their lease releases.
    pub deferred_in_use: u64,
    /// Capacity state after the provider accepted the drain request.
    pub snapshot: CapacitySnapshot,
}

/// Provider-independent failures mapped into stable Blaze diagnostics.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ProviderError {
    /// Required runtime dependency is unavailable.
    #[error("data-plane provider is unavailable")]
    Unavailable,
    /// Request conflicts with retained provider state.
    #[error("data-plane provider state conflicts with the request")]
    Conflict,
    /// Mutation may have occurred and requires inspection or reconciliation.
    #[error("data-plane provider operation outcome is unknown")]
    OutcomeUnknown,
    /// Provider does not implement the requested generic operation.
    #[error("data-plane provider operation is unsupported")]
    Unsupported,
    /// Provider does not own the selected generic resource.
    #[error("data-plane provider resource was not found")]
    NotFound,
    /// Provider returned a value that violates the public contract.
    #[error("data-plane provider returned an invalid response")]
    InvalidResponse,
    /// Immutable source or combined contract is incompatible.
    #[error("data-plane provider is incompatible with the request")]
    Incompatible,
}

/// Start one consistent, paged view of all leases owned by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeginInventoryRequest {
    /// Maximum entries Blaze will accept in one page.
    pub page_size: u32,
}

/// Stable provider inventory frozen for one traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventorySnapshot {
    /// Provider that owns this snapshot.
    pub provider_instance_id: Uuid,
    /// Opaque snapshot identity used only in follow-up page requests.
    pub snapshot_id: Uuid,
}

/// Request one page from a previously frozen inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryPageRequest {
    /// Snapshot identity returned by the matching `begin_inventory` call.
    pub snapshot_id: Uuid,
    /// `None` for the first page, or the exact cursor returned by the preceding page.
    pub cursor: Option<String>,
    /// Maximum number of leases the provider may return in this page.
    pub page_size: u32,
}

/// One non-released provider lease visible in the frozen inventory.
///
/// Released leases no longer own resources and must be omitted. Returning a
/// released tombstone makes the complete inventory invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryLease {
    /// Complete non-released lease binding as it existed in the frozen view.
    pub binding: LeaseBinding,
}

/// Bounded inventory page. A missing cursor completes the traversal.
///
/// A continuation cursor must be non-empty and no longer than
/// [`MAX_INVENTORY_CURSOR_BYTES`]. A page carrying a continuation cursor must
/// contain at least one lease. Blaze also stops after [`MAX_INVENTORY_PAGES`]
/// even when every cursor is distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryPage {
    /// Unique, non-released lease bindings from the requested snapshot page.
    pub leases: Vec<InventoryLease>,
    /// Opaque cursor for the next non-empty page, or `None` when traversal is complete.
    pub next_cursor: Option<String>,
}

/// Safe convergence action selected after comparing all ownership ledgers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Retain an exact live lease and associate it with the proven backend.
    Adopt {
        /// Backend process identity Blaze verified before requesting adoption.
        backend_process: BackendProcessIdentity,
    },
    /// Retain resources without allowing them to serve traffic or be reused.
    Quarantine,
}

/// Reconcile one observed provider lease against a public expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileRequest {
    /// Exact durable lease expected by Blaze, or `None` for a provider-only lease.
    pub expected: Option<LeaseBinding>,
    /// Exact lease binding returned by the frozen provider inventory.
    pub observed: LeaseBinding,
    /// Fail-closed convergence action selected from all ownership evidence.
    pub action: ReconcileAction,
}

/// Provider-confirmed convergence result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileResult {
    /// Successor binding with unchanged identity, the next generation, and requested state.
    pub binding: LeaseBinding,
}

/// Optional lease inventory and restart-convergence extension.
#[async_trait]
pub trait DataPlaneInventory: DataPlaneProvider {
    /// Freeze one stable provider view before Blaze requests any inventory pages.
    async fn begin_inventory(
        &self,
        request: BeginInventoryRequest,
    ) -> Result<InventorySnapshot, ProviderError>;

    /// Return one bounded page from the requested snapshot and continuation cursor.
    async fn inventory_page(
        &self,
        request: InventoryPageRequest,
    ) -> Result<InventoryPage, ProviderError>;

    /// Advance the observed lease by one generation to the requested safe state.
    async fn reconcile(&self, request: ReconcileRequest) -> Result<ReconcileResult, ProviderError>;
}

/// Optional immutable checkpoint capture, restore, and retirement extension.
#[async_trait]
pub trait DataPlaneCheckpoint: DataPlaneProvider {
    /// Freeze immutable provider content at the backend's capture boundary.
    async fn checkpoint(
        &self,
        request: ProviderCheckpointRequest,
    ) -> Result<CheckpointSubmission, ProviderError>;

    /// Prepare a new exclusive lease from immutable checkpoint content.
    async fn restore_checkpoint(
        &self,
        request: RestoreCheckpointRequest,
    ) -> Result<PreparedLease, ProviderError>;

    /// Idempotently release content after its public owner is removed.
    async fn retire_checkpoint(
        &self,
        request: RetireCheckpointRequest,
    ) -> Result<RetireCheckpointResult, ProviderError>;
}

/// Optional hibernation capture, fresh-lease resume, and retirement extension.
#[async_trait]
pub trait DataPlaneSuspend: DataPlaneProvider {
    /// Capture immutable provider content at the backend's quiesce boundary.
    async fn suspend(&self, request: SuspendRequest)
    -> Result<SuspensionSubmission, ProviderError>;

    /// Prepare a new exclusive lease from one verified suspension reference.
    async fn resume(&self, request: ResumeRequest) -> Result<PreparedLease, ProviderError>;

    /// Idempotently release content after its public hibernation owner is removed.
    async fn retire_suspension(
        &self,
        request: RetireSuspensionRequest,
    ) -> Result<RetireSuspensionResult, ProviderError>;
}

/// Optional reporting and drain control for reusable data-plane resources.
#[async_trait]
pub trait DataPlaneCapacity: DataPlaneProvider {
    /// Return one complete, mutually exclusive partition snapshot.
    async fn capacity(&self, request: CapacityRequest) -> Result<CapacitySnapshot, ProviderError>;

    /// Stop reusing a partition and remove each resource when it is safe.
    async fn drain(&self, request: DrainRequest) -> Result<DrainResult, ProviderError>;
}

/// Source-level data-plane provider compiled into one Blaze daemon binary.
#[async_trait]
pub trait DataPlaneProvider: Send + Sync {
    /// Return the provider identity and exact source-contract revision.
    fn descriptor(&self) -> ProviderDescriptor;

    /// Return optional operations implemented by this provider.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Return restart reconciliation support when this provider implements it.
    fn inventory(&self) -> Option<&dyn DataPlaneInventory> {
        None
    }

    /// Return checkpoint support when provider-owned immutable content exists.
    fn checkpoints(&self) -> Option<&dyn DataPlaneCheckpoint> {
        None
    }

    /// Return hibernation support when provider-owned immutable content exists.
    fn suspension(&self) -> Option<&dyn DataPlaneSuspend> {
        None
    }

    /// Return reusable-resource capacity support when this provider owns it.
    fn capacity_control(&self) -> Option<&dyn DataPlaneCapacity> {
        None
    }

    /// Check prerequisites without allocating sandbox resources.
    async fn probe(&self) -> Result<(), ProviderError>;

    /// Create or materialize one preselected resource lease.
    async fn prepare(&self, request: PrepareRequest) -> Result<PreparedLease, ProviderError>;

    /// Observe one lease without creating, stopping, or releasing resources.
    ///
    /// Return [`ProviderError::NotFound`] only when no provider state exists
    /// for the complete request context. A colliding lease identity with a
    /// different context is a conflict, not absence.
    async fn inspect(&self, request: InspectRequest) -> Result<ObservedLease, ProviderError>;

    /// Mark backend readiness before public state is published.
    async fn commit(&self, request: CommitRequest) -> Result<CommittedLease, ProviderError>;

    /// Complete ownership handoff after public state is durable.
    async fn finalize(&self, request: FinalizeRequest) -> Result<FinalizedLease, ProviderError>;

    /// Compensate a preparation that has no durable public transition.
    async fn abort(&self, request: AbortRequest) -> Result<AbortResult, ProviderError>;

    /// Retain cleanup state after backend use ends.
    async fn stop(&self, request: StopRequest) -> Result<StoppedLease, ProviderError>;

    /// Prove that all resources owned by a stopped lease are absent.
    async fn release(&self, request: ReleaseRequest) -> Result<ReleaseResult, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_lease_round_trip_preserves_every_identity() {
        let instance_id = Uuid::new_v4();
        let binding = LeaseBinding {
            provider_instance_id: Uuid::new_v4(),
            context: RequestContext {
                instance_id,
                request_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                lease_id: Uuid::new_v4(),
                generation: 7,
            },
            generation: 11,
            state: LeaseState::Finalized,
        };

        let record = binding.to_record(64 * 1024 * 1024, 512 * 1024 * 1024);

        assert_eq!(LeaseBinding::from_record(instance_id, record), binding);
        assert_eq!(record.initial_generation, 7);
        assert_eq!(record.generation, 11);
    }

    #[test]
    fn durable_request_context_round_trip_preserves_every_identity() {
        let context = RequestContext {
            instance_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 7,
        };

        let durable = DataPlaneRequestContextRecord::from(context);

        assert_eq!(RequestContext::from(durable), context);
    }

    #[test]
    fn durable_checkpoint_round_trip_preserves_complete_owner_identity() {
        let checkpoint = ProviderCheckpointRef {
            provider_instance_id: Uuid::new_v4(),
            public_checkpoint_id: Uuid::new_v4(),
            reference_id: Uuid::new_v4(),
            content_digest: format!("sha256:{}", "b".repeat(64)),
            parent_reference_id: Some(Uuid::new_v4()),
            source_lease_id: Uuid::new_v4(),
            source_generation: 9,
        };

        assert_eq!(
            ProviderCheckpointRef::from_record(&checkpoint.to_record()),
            checkpoint
        );
    }

    #[test]
    fn durable_suspension_round_trip_preserves_resume_contract() {
        let suspension = ProviderSuspensionRef {
            provider_instance_id: Uuid::new_v4(),
            suspension_id: Uuid::new_v4(),
            reference_id: Uuid::new_v4(),
            content_digest: format!("sha256:{}", "a".repeat(64)),
            source_lease_id: Uuid::new_v4(),
            source_generation: 9,
            root_filesystem_bytes: 8 * 1024 * 1024 * 1024,
            guest_memory_bytes: 512 * 1024 * 1024,
        };

        assert_eq!(
            ProviderSuspensionRef::from_record(&suspension.to_record()),
            suspension
        );
    }

    #[test]
    fn capacity_class_digest_has_stable_canonical_encoding() {
        let class = CapacityClass {
            root_filesystem_capacity_bytes: 4 * 1024 * 1024 * 1024,
            guest_memory_capacity_bytes: 512 * 1024 * 1024,
        };

        assert_eq!(
            class.digest(),
            [
                0x4c, 0xa1, 0x71, 0x9d, 0x90, 0xeb, 0x4e, 0xa1, 0x06, 0xf4, 0x7e, 0xc8, 0x25, 0x87,
                0xbb, 0xfe, 0x5a, 0xfc, 0x72, 0x63, 0x76, 0x66, 0x9d, 0xc1, 0xc0, 0xf1, 0xdc, 0xa5,
                0xda, 0xc6, 0xcd, 0x68,
            ]
        );
        assert_ne!(
            class.digest(),
            CapacityClass {
                guest_memory_capacity_bytes: class.guest_memory_capacity_bytes + 1,
                ..class
            }
            .digest()
        );
    }
}
