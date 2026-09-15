// SPDX-License-Identifier: Apache-2.0
//! Restart recovery tests for provider calls interrupted at durable handoff boundaries.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use blaze_core::backend::{BackendKind, SnapshotKind};
use blaze_core::checkpoint::{CommitCheckpoint, ProviderCheckpointRecord, validate_checkpoint_id};
use blaze_core::config::DaemonConfig;
use blaze_core::data_plane::{
    BackendProcessIdentity, BackendRuntimeRecord, DataPlaneLeaseState,
    PendingProviderOperationKind, PendingProviderOperationRecord,
};
use blaze_core::lifecycle::{
    BackendOwnership, OperationKind, OperationPhase, PendingProviderTransitionRecord,
    ProviderLeaseSlot, ProviderTransitionKind, SandboxInstance, SandboxState,
};
use blaze_core::policy::WorkloadClass;
use blaze_core::storage::{AcquireOpts, StorageProvider, StorageSlot};
use blaze_provider_api::{
    AbortRequest, AbortResult, BeginInventoryRequest, CheckpointSubmission, CommitRequest,
    CommittedLease, DataPlaneCheckpoint, DataPlaneInventory, DataPlaneProvider, DataPlaneSuspend,
    FinalizeRequest, FinalizedLease, InspectRequest, InventoryLease, InventoryPage,
    InventoryPageRequest, InventorySnapshot, LeaseBinding, LeaseState, MAX_INVENTORY_CURSOR_BYTES,
    MAX_INVENTORY_PAGES, ObservedLease, PROVIDER_CONTRACT_VERSION, PrepareRequest, PrepareSource,
    PreparedLease, PreparedResources, ProviderCapabilities, ProviderCheckpointRef,
    ProviderCheckpointRequest, ProviderDescriptor, ProviderError, ProviderSuspensionRef,
    ReconcileAction, ReconcileRequest, ReconcileResult, ReleaseRequest, ReleaseResult,
    RequestContext, RestoreCheckpointRequest, ResumeRequest, RetireCheckpointRequest,
    RetireCheckpointResult, RetireSuspensionRequest, RetireSuspensionResult, StopRequest,
    StoppedLease, SuspendRequest, SuspensionSubmission,
};
use uuid::Uuid;

use super::manager::{SandboxManager, SandboxManagerInit};
use super::template::TemplateCatalog;
use crate::data_plane::FileDataPlaneProvider;
use crate::error::BlazeDaemonError;
use crate::file_provider::FileStorageProvider;
use crate::spawner::{MockSpawner, SpawnerRegistry};
use crate::state_store::StateStore;

struct RecoveryProvider {
    descriptor: ProviderDescriptor,
    binding: Mutex<Option<LeaseBinding>>,
    lose_prepare_response: AtomicBool,
    inspect_error: Mutex<Option<ProviderError>>,
    abort_error: Mutex<Option<ProviderError>>,
    inspect_calls: AtomicUsize,
}

impl RecoveryProvider {
    fn new() -> Self {
        Self {
            descriptor: ProviderDescriptor {
                contract_version: PROVIDER_CONTRACT_VERSION,
                provider_instance_id: Uuid::new_v4(),
            },
            binding: Mutex::new(None),
            lose_prepare_response: AtomicBool::new(false),
            inspect_error: Mutex::new(None),
            abort_error: Mutex::new(None),
            inspect_calls: AtomicUsize::new(0),
        }
    }

    fn binding(&self) -> Option<LeaseBinding> {
        *self.binding.lock().expect("provider binding")
    }

    fn advance(
        &self,
        binding: LeaseBinding,
        expected: LeaseState,
        next: LeaseState,
        remove: bool,
    ) -> Result<LeaseBinding, ProviderError> {
        let mut current = self
            .binding
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        if *current != Some(binding) || binding.state != expected {
            return Err(ProviderError::Conflict);
        }
        let next = LeaseBinding {
            generation: binding
                .generation
                .checked_add(1)
                .ok_or(ProviderError::OutcomeUnknown)?,
            state: next,
            ..binding
        };
        *current = (!remove).then_some(next);
        Ok(next)
    }
}

#[async_trait]
impl DataPlaneProvider for RecoveryProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            images: true,
            ..ProviderCapabilities::default()
        }
    }

    async fn probe(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn prepare(&self, request: PrepareRequest) -> Result<PreparedLease, ProviderError> {
        let binding = LeaseBinding {
            provider_instance_id: self.descriptor.provider_instance_id,
            context: request.context,
            generation: request.context.generation,
            state: LeaseState::Prepared,
        };
        let mut current = self
            .binding
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        if current.is_some_and(|current| current != binding) {
            return Err(ProviderError::Conflict);
        }
        *current = Some(binding);
        drop(current);
        if self.lose_prepare_response.load(Ordering::Acquire) {
            return Err(ProviderError::OutcomeUnknown);
        }
        let root = PathBuf::from(request.context.instance_id.to_string());
        Ok(PreparedLease {
            binding,
            resources: PreparedResources::PathBacked {
                storage: StorageSlot {
                    id: request.context.instance_id.to_string(),
                    rootfs_path: root.join("rootfs"),
                    mem_path: root.join("memory"),
                    mem_diff_path: root.join("memory.diff"),
                    rootfs_diff_path: root.join("rootfs.diff"),
                    instance_dir: root,
                },
                restore_payload_dir: None,
            },
        })
    }

    async fn inspect(&self, request: InspectRequest) -> Result<ObservedLease, ProviderError> {
        self.inspect_calls.fetch_add(1, Ordering::AcqRel);
        if let Some(error) = *self
            .inspect_error
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
        {
            return Err(error);
        }
        let current = self
            .binding
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        let Some(binding) = *current else {
            return Err(ProviderError::NotFound);
        };
        if binding.context != request.context {
            return Err(ProviderError::Conflict);
        }
        Ok(ObservedLease { binding })
    }

    async fn commit(&self, request: CommitRequest) -> Result<CommittedLease, ProviderError> {
        Ok(CommittedLease {
            binding: self.advance(
                request.binding,
                LeaseState::Prepared,
                LeaseState::Committed,
                false,
            )?,
        })
    }

    async fn finalize(&self, request: FinalizeRequest) -> Result<FinalizedLease, ProviderError> {
        Ok(FinalizedLease {
            binding: self.advance(
                request.binding,
                LeaseState::Committed,
                LeaseState::Finalized,
                false,
            )?,
        })
    }

    async fn abort(&self, request: AbortRequest) -> Result<AbortResult, ProviderError> {
        if let Some(error) = *self
            .abort_error
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
        {
            return Err(error);
        }
        Ok(AbortResult {
            binding: self.advance(
                request.binding,
                LeaseState::Prepared,
                LeaseState::Released,
                true,
            )?,
        })
    }

    async fn stop(&self, request: StopRequest) -> Result<StoppedLease, ProviderError> {
        Ok(StoppedLease {
            binding: self.advance(
                request.binding,
                LeaseState::Finalized,
                LeaseState::Stopped,
                false,
            )?,
        })
    }

    async fn release(&self, request: ReleaseRequest) -> Result<ReleaseResult, ProviderError> {
        Ok(ReleaseResult {
            binding: self.advance(
                request.binding,
                LeaseState::Stopped,
                LeaseState::Released,
                true,
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetiredIdentity {
    Checkpoint(Uuid, Option<Uuid>),
    Suspension(Uuid, Option<Uuid>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InventoryBehavior {
    Normal,
    EmptyContinuation,
    OversizedCursor,
    EndlessDistinctCursors,
    ReleasedLease,
}

struct LifecycleRecoveryProvider {
    descriptor: ProviderDescriptor,
    leases: Mutex<HashMap<Uuid, LeaseBinding>>,
    checkpoints: Mutex<HashMap<Uuid, Uuid>>,
    suspensions: Mutex<HashMap<Uuid, Uuid>>,
    retirements: Mutex<Vec<RetiredIdentity>>,
    retirement_attempts: Mutex<Vec<(RetiredIdentity, Uuid)>>,
    stopped_generations: Mutex<Vec<u64>>,
    cleanup_calls: AtomicUsize,
    reject_retirement: AtomicBool,
    inventory_enabled: AtomicBool,
    inventory_behavior: Mutex<InventoryBehavior>,
    inventory_page_calls: AtomicUsize,
    snapshot_id: Uuid,
    reconcile_actions: Mutex<Vec<ReconcileAction>>,
}

impl LifecycleRecoveryProvider {
    fn new() -> Self {
        Self {
            descriptor: ProviderDescriptor {
                contract_version: PROVIDER_CONTRACT_VERSION,
                provider_instance_id: Uuid::new_v4(),
            },
            leases: Mutex::new(HashMap::new()),
            checkpoints: Mutex::new(HashMap::new()),
            suspensions: Mutex::new(HashMap::new()),
            retirements: Mutex::new(Vec::new()),
            retirement_attempts: Mutex::new(Vec::new()),
            stopped_generations: Mutex::new(Vec::new()),
            cleanup_calls: AtomicUsize::new(0),
            reject_retirement: AtomicBool::new(false),
            inventory_enabled: AtomicBool::new(false),
            inventory_behavior: Mutex::new(InventoryBehavior::Normal),
            inventory_page_calls: AtomicUsize::new(0),
            snapshot_id: Uuid::new_v4(),
            reconcile_actions: Mutex::new(Vec::new()),
        }
    }

    fn insert_lease(&self, binding: LeaseBinding) {
        self.leases
            .lock()
            .expect("provider leases")
            .insert(binding.context.lease_id, binding);
    }

    fn advance(
        &self,
        binding: LeaseBinding,
        expected: LeaseState,
        next: LeaseState,
        remove: bool,
    ) -> Result<LeaseBinding, ProviderError> {
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        if leases.get(&binding.context.lease_id).copied() != Some(binding)
            || binding.state != expected
        {
            return Err(ProviderError::Conflict);
        }
        let next = LeaseBinding {
            generation: binding
                .generation
                .checked_add(1)
                .ok_or(ProviderError::OutcomeUnknown)?,
            state: next,
            ..binding
        };
        if remove {
            leases.remove(&binding.context.lease_id);
        } else {
            leases.insert(binding.context.lease_id, next);
        }
        Ok(next)
    }

    fn prepare_replacement(
        &self,
        context: RequestContext,
        resources: PreparedResources,
    ) -> Result<PreparedLease, ProviderError> {
        let binding = LeaseBinding {
            provider_instance_id: self.descriptor.provider_instance_id,
            context,
            generation: context.generation,
            state: LeaseState::Prepared,
        };
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        if leases
            .get(&context.lease_id)
            .is_some_and(|previous| *previous != binding)
        {
            return Err(ProviderError::Conflict);
        }
        leases.insert(context.lease_id, binding);
        Ok(PreparedLease { binding, resources })
    }

    fn retirement_log(&self) -> Vec<RetiredIdentity> {
        self.retirements.lock().expect("retirement log").clone()
    }

    fn retirement_attempts(&self) -> Vec<(RetiredIdentity, Uuid)> {
        self.retirement_attempts
            .lock()
            .expect("retirement attempts")
            .clone()
    }

    fn lease_count(&self) -> usize {
        self.leases.lock().expect("provider leases").len()
    }

    fn stopped_generations(&self) -> Vec<u64> {
        self.stopped_generations
            .lock()
            .expect("stopped generations")
            .clone()
    }

    fn set_inventory_behavior(&self, behavior: InventoryBehavior) {
        *self.inventory_behavior.lock().expect("inventory behavior") = behavior;
    }

    fn synthetic_inventory_binding(&self, page: u128, state: LeaseState) -> LeaseBinding {
        let nonzero = |offset: u128| Uuid::from_u128(page * 8 + offset);
        LeaseBinding {
            provider_instance_id: self.descriptor.provider_instance_id,
            context: RequestContext {
                instance_id: nonzero(1),
                request_id: nonzero(2),
                operation_id: nonzero(3),
                lease_id: nonzero(4),
                generation: 1,
            },
            generation: 1,
            state,
        }
    }
}

#[async_trait]
impl DataPlaneProvider for LifecycleRecoveryProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            images: true,
            ..ProviderCapabilities::default()
        }
    }

    fn inventory(&self) -> Option<&dyn DataPlaneInventory> {
        self.inventory_enabled
            .load(Ordering::Acquire)
            .then_some(self)
    }

    fn checkpoints(&self) -> Option<&dyn DataPlaneCheckpoint> {
        Some(self)
    }

    fn suspension(&self) -> Option<&dyn DataPlaneSuspend> {
        Some(self)
    }

    async fn probe(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn prepare(&self, _request: PrepareRequest) -> Result<PreparedLease, ProviderError> {
        Err(ProviderError::Unsupported)
    }

    async fn inspect(&self, request: InspectRequest) -> Result<ObservedLease, ProviderError> {
        let leases = self
            .leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        let binding = leases
            .get(&request.context.lease_id)
            .copied()
            .ok_or(ProviderError::NotFound)?;
        if binding.context != request.context {
            return Err(ProviderError::Conflict);
        }
        Ok(ObservedLease { binding })
    }

    async fn commit(&self, request: CommitRequest) -> Result<CommittedLease, ProviderError> {
        Ok(CommittedLease {
            binding: self.advance(
                request.binding,
                LeaseState::Prepared,
                LeaseState::Committed,
                false,
            )?,
        })
    }

    async fn finalize(&self, request: FinalizeRequest) -> Result<FinalizedLease, ProviderError> {
        Ok(FinalizedLease {
            binding: self.advance(
                request.binding,
                LeaseState::Committed,
                LeaseState::Finalized,
                false,
            )?,
        })
    }

    async fn abort(&self, request: AbortRequest) -> Result<AbortResult, ProviderError> {
        self.cleanup_calls.fetch_add(1, Ordering::AcqRel);
        Ok(AbortResult {
            binding: self.advance(
                request.binding,
                LeaseState::Prepared,
                LeaseState::Released,
                true,
            )?,
        })
    }

    async fn stop(&self, request: StopRequest) -> Result<StoppedLease, ProviderError> {
        self.cleanup_calls.fetch_add(1, Ordering::AcqRel);
        self.stopped_generations
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .push(request.binding.generation);
        Ok(StoppedLease {
            binding: self.advance(
                request.binding,
                LeaseState::Finalized,
                LeaseState::Stopped,
                false,
            )?,
        })
    }

    async fn release(&self, request: ReleaseRequest) -> Result<ReleaseResult, ProviderError> {
        self.cleanup_calls.fetch_add(1, Ordering::AcqRel);
        Ok(ReleaseResult {
            binding: self.advance(
                request.binding,
                LeaseState::Stopped,
                LeaseState::Released,
                true,
            )?,
        })
    }
}

#[async_trait]
impl DataPlaneInventory for LifecycleRecoveryProvider {
    async fn begin_inventory(
        &self,
        _request: BeginInventoryRequest,
    ) -> Result<InventorySnapshot, ProviderError> {
        Ok(InventorySnapshot {
            provider_instance_id: self.descriptor.provider_instance_id,
            snapshot_id: self.snapshot_id,
        })
    }

    async fn inventory_page(
        &self,
        request: InventoryPageRequest,
    ) -> Result<InventoryPage, ProviderError> {
        self.inventory_page_calls.fetch_add(1, Ordering::AcqRel);
        if request.snapshot_id != self.snapshot_id {
            return Err(ProviderError::Conflict);
        }
        match *self
            .inventory_behavior
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
        {
            InventoryBehavior::EmptyContinuation => {
                return Ok(InventoryPage {
                    leases: Vec::new(),
                    next_cursor: Some("next".to_string()),
                });
            }
            InventoryBehavior::OversizedCursor => {
                return Ok(InventoryPage {
                    leases: vec![InventoryLease {
                        binding: self.synthetic_inventory_binding(1, LeaseState::Finalized),
                    }],
                    next_cursor: Some("x".repeat(MAX_INVENTORY_CURSOR_BYTES + 1)),
                });
            }
            InventoryBehavior::EndlessDistinctCursors => {
                let page = request
                    .cursor
                    .as_deref()
                    .map(str::parse::<u128>)
                    .transpose()
                    .map_err(|_| ProviderError::Conflict)?
                    .unwrap_or(0)
                    + 1;
                return Ok(InventoryPage {
                    leases: vec![InventoryLease {
                        binding: self.synthetic_inventory_binding(page, LeaseState::Finalized),
                    }],
                    next_cursor: Some(page.to_string()),
                });
            }
            InventoryBehavior::ReleasedLease => {
                return Ok(InventoryPage {
                    leases: vec![InventoryLease {
                        binding: self.synthetic_inventory_binding(1, LeaseState::Released),
                    }],
                    next_cursor: None,
                });
            }
            InventoryBehavior::Normal => {}
        }
        if request.cursor.is_some() {
            return Err(ProviderError::Conflict);
        }
        let leases = self
            .leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .values()
            .copied()
            .map(|binding| InventoryLease { binding })
            .collect();
        Ok(InventoryPage {
            leases,
            next_cursor: None,
        })
    }

    async fn reconcile(&self, request: ReconcileRequest) -> Result<ReconcileResult, ProviderError> {
        self.reconcile_actions
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .push(request.action);
        if matches!(request.action, ReconcileAction::Adopt { .. })
            && request.expected != Some(request.observed)
        {
            return Err(ProviderError::Conflict);
        }
        let target = match request.action {
            ReconcileAction::Adopt { .. }
                if matches!(
                    request.observed.state,
                    LeaseState::Committed | LeaseState::Finalized
                ) =>
            {
                LeaseState::Finalized
            }
            ReconcileAction::Quarantine if request.observed.state != LeaseState::Released => {
                LeaseState::Quarantined
            }
            _ => return Err(ProviderError::Conflict),
        };
        Ok(ReconcileResult {
            binding: self.advance(request.observed, request.observed.state, target, false)?,
        })
    }
}

#[async_trait]
impl DataPlaneCheckpoint for LifecycleRecoveryProvider {
    async fn checkpoint(
        &self,
        request: ProviderCheckpointRequest,
    ) -> Result<CheckpointSubmission, ProviderError> {
        let binding = self.advance(
            request.binding,
            LeaseState::Finalized,
            LeaseState::Finalized,
            false,
        )?;
        let reference_id = Uuid::new_v4();
        self.checkpoints
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .insert(request.checkpoint_id, reference_id);
        Ok(CheckpointSubmission {
            binding,
            checkpoint: ProviderCheckpointRef {
                provider_instance_id: self.descriptor.provider_instance_id,
                public_checkpoint_id: request.checkpoint_id,
                reference_id,
                content_digest: format!("sha256:{}", "a".repeat(64)),
                parent_reference_id: request.parent.map(|parent| parent.reference_id),
                source_lease_id: binding.context.lease_id,
                source_generation: binding.generation,
            },
        })
    }

    async fn restore_checkpoint(
        &self,
        request: RestoreCheckpointRequest,
    ) -> Result<PreparedLease, ProviderError> {
        self.prepare_replacement(
            request.context,
            PreparedResources::CheckpointRestore {
                storage: Some(test_storage_slot(request.context.instance_id)),
                attachments: Vec::new(),
            },
        )
    }

    async fn retire_checkpoint(
        &self,
        request: RetireCheckpointRequest,
    ) -> Result<RetireCheckpointResult, ProviderError> {
        self.retirement_attempts
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .push((
                RetiredIdentity::Checkpoint(request.public_checkpoint_id, request.reference_id),
                request.operation_id,
            ));
        if self.reject_retirement.load(Ordering::Acquire) {
            return Err(ProviderError::OutcomeUnknown);
        }
        if request.provider_instance_id != self.descriptor.provider_instance_id {
            return Err(ProviderError::Conflict);
        }
        let retired = self
            .checkpoints
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .remove(&request.public_checkpoint_id)
            .is_some();
        self.retirements
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .push(RetiredIdentity::Checkpoint(
                request.public_checkpoint_id,
                request.reference_id,
            ));
        Ok(RetireCheckpointResult {
            public_checkpoint_id: request.public_checkpoint_id,
            reference_id: request.reference_id,
            retired,
        })
    }
}

#[async_trait]
impl DataPlaneSuspend for LifecycleRecoveryProvider {
    async fn suspend(
        &self,
        request: SuspendRequest,
    ) -> Result<SuspensionSubmission, ProviderError> {
        let binding = self.advance(
            request.binding,
            LeaseState::Finalized,
            LeaseState::Finalized,
            false,
        )?;
        let reference_id = Uuid::new_v4();
        self.suspensions
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .insert(request.suspension_id, reference_id);
        Ok(SuspensionSubmission {
            binding,
            suspension: ProviderSuspensionRef {
                provider_instance_id: self.descriptor.provider_instance_id,
                suspension_id: request.suspension_id,
                reference_id,
                content_digest: format!("sha256:{}", "b".repeat(64)),
                source_lease_id: binding.context.lease_id,
                source_generation: binding.generation,
                root_filesystem_bytes: request.root_filesystem_bytes,
                guest_memory_bytes: request.guest_memory_bytes,
            },
        })
    }

    async fn resume(&self, request: ResumeRequest) -> Result<PreparedLease, ProviderError> {
        self.prepare_replacement(
            request.context,
            PreparedResources::SuspensionRestore {
                storage: Some(test_storage_slot(request.context.instance_id)),
                attachments: Vec::new(),
            },
        )
    }

    async fn retire_suspension(
        &self,
        request: RetireSuspensionRequest,
    ) -> Result<RetireSuspensionResult, ProviderError> {
        self.retirement_attempts
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .push((
                RetiredIdentity::Suspension(request.suspension_id, request.reference_id),
                request.operation_id,
            ));
        if self.reject_retirement.load(Ordering::Acquire) {
            return Err(ProviderError::OutcomeUnknown);
        }
        if request.provider_instance_id != self.descriptor.provider_instance_id {
            return Err(ProviderError::Conflict);
        }
        let retired = self
            .suspensions
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .remove(&request.suspension_id)
            .is_some();
        self.retirements
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .push(RetiredIdentity::Suspension(
                request.suspension_id,
                request.reference_id,
            ));
        Ok(RetireSuspensionResult {
            suspension_id: request.suspension_id,
            reference_id: request.reference_id,
            retired,
        })
    }
}

fn test_storage_slot(instance_id: Uuid) -> StorageSlot {
    let instance_dir = PathBuf::from(instance_id.to_string());
    StorageSlot {
        id: instance_id.to_string(),
        rootfs_path: instance_dir.join("rootfs"),
        mem_path: instance_dir.join("memory"),
        mem_diff_path: instance_dir.join("memory.diff"),
        rootfs_diff_path: instance_dir.join("rootfs.diff"),
        instance_dir,
    }
}

fn build_manager(
    temporary: &tempfile::TempDir,
    provider: Arc<RecoveryProvider>,
) -> Arc<SandboxManager> {
    let images = temporary.path().join("images");
    let instances_root = temporary.path().join("instances");
    let storage: Arc<dyn StorageProvider> =
        Arc::new(FileStorageProvider::with_images(images, instances_root));
    build_manager_with_storage(temporary, provider, storage)
}

fn build_manager_with_storage(
    temporary: &tempfile::TempDir,
    provider: Arc<dyn DataPlaneProvider>,
    storage: Arc<dyn StorageProvider>,
) -> Arc<SandboxManager> {
    let state_root = temporary.path().join("state");
    let templates = temporary.path().join("templates");
    for directory in [&state_root, &templates] {
        std::fs::create_dir_all(directory).expect("test directory");
    }
    let state_store = StateStore::new(state_root);
    let instances = state_store.scan().expect("scan lifecycle state");
    let mut config = DaemonConfig::default();
    config.template.dir = templates;
    config.template.import_root = None;
    let template_catalog = TemplateCatalog::open(&config.template).expect("template catalog");
    let mut spawners = SpawnerRegistry::new();
    spawners.insert(BackendKind::Mock, Arc::new(MockSpawner));
    let (manager, _) = SandboxManager::new(SandboxManagerInit {
        instances,
        spawners,
        active_backend: BackendKind::Mock,
        storage,
        data_plane: provider,
        state_store,
        rootfs_size: 4096,
        mem_size: 8192,
        template_catalog,
    });
    Arc::new(manager)
}

fn persist_pending_prepare(
    manager: &SandboxManager,
    provider_instance_id: Uuid,
) -> (Uuid, RequestContext) {
    let mut instance = SandboxInstance::new(
        BackendKind::Mock,
        WorkloadClass::AgentTool,
        "sha256:provider-recovery".to_string(),
        "provider-recovery".to_string(),
    );
    instance
        .transition(SandboxState::Creating)
        .expect("creating");
    instance.begin_operation(OperationKind::Create);
    let context = RequestContext {
        instance_id: instance.id,
        request_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        lease_id: Uuid::new_v4(),
        generation: 1,
    };
    instance
        .begin_provider_operation(PendingProviderOperationRecord {
            provider_instance_id,
            context: context.into(),
            generation_before_call: 0,
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
            kind: PendingProviderOperationKind::PrepareLease,
        })
        .expect("provider write-ahead identity");
    let id = instance.id;
    manager
        .persist_and_retain(instance)
        .expect("persist pending prepare");
    (id, context)
}

fn prepare_request(context: RequestContext) -> PrepareRequest {
    PrepareRequest {
        context,
        source: PrepareSource::Image {
            image_digest: "sha256:provider-recovery".to_string(),
        },
        root_filesystem_bytes: 4096,
        guest_memory_bytes: 8192,
    }
}

fn build_lifecycle_manager(
    temporary: &tempfile::TempDir,
    provider: Arc<LifecycleRecoveryProvider>,
) -> Arc<SandboxManager> {
    let images = temporary.path().join("images");
    let instances_root = temporary.path().join("instances");
    let storage: Arc<dyn StorageProvider> =
        Arc::new(FileStorageProvider::with_images(images, instances_root));
    build_manager_with_storage(temporary, provider, storage)
}

fn finalized_binding(
    provider_instance_id: Uuid,
    instance_id: Uuid,
    generation: u64,
) -> LeaseBinding {
    LeaseBinding {
        provider_instance_id,
        context: RequestContext {
            instance_id,
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
        },
        generation,
        state: LeaseState::Finalized,
    }
}

fn active_instance(
    provider: &LifecycleRecoveryProvider,
    generation: u64,
) -> (SandboxInstance, LeaseBinding) {
    let mut instance = SandboxInstance::new(
        BackendKind::Mock,
        WorkloadClass::AgentTool,
        "sha256:provider-lifecycle-recovery".to_string(),
        "provider-lifecycle-recovery".to_string(),
    );
    instance
        .transition(SandboxState::Creating)
        .expect("creating");
    instance.transition(SandboxState::Running).expect("running");
    instance.backend_ownership = BackendOwnership::Stopped;
    let binding = finalized_binding(
        provider.descriptor.provider_instance_id,
        instance.id,
        generation,
    );
    instance.data_plane_lease = Some(binding.to_record(4096, 8192));
    provider.insert_lease(binding);
    (instance, binding)
}

fn replacement_context(instance_id: Uuid) -> RequestContext {
    RequestContext {
        instance_id,
        request_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        lease_id: Uuid::new_v4(),
        generation: 1,
    }
}

fn pending_prepare(
    provider_instance_id: Uuid,
    context: RequestContext,
) -> PendingProviderOperationRecord {
    PendingProviderOperationRecord {
        provider_instance_id,
        context: context.into(),
        generation_before_call: 0,
        root_filesystem_bytes: 4096,
        guest_memory_bytes: 8192,
        kind: PendingProviderOperationKind::PrepareLease,
    }
}

fn pending_capture(
    binding: LeaseBinding,
    kind: PendingProviderOperationKind,
) -> PendingProviderOperationRecord {
    PendingProviderOperationRecord {
        provider_instance_id: binding.provider_instance_id,
        context: binding.context.into(),
        generation_before_call: binding.generation,
        root_filesystem_bytes: 4096,
        guest_memory_bytes: 8192,
        kind,
    }
}

fn checkpoint_reference(
    provider_instance_id: Uuid,
    public_checkpoint_id: Uuid,
    source: LeaseBinding,
) -> ProviderCheckpointRef {
    ProviderCheckpointRef {
        provider_instance_id,
        public_checkpoint_id,
        reference_id: Uuid::new_v4(),
        content_digest: format!("sha256:{}", "c".repeat(64)),
        parent_reference_id: None,
        source_lease_id: source.context.lease_id,
        source_generation: source.generation,
    }
}

fn suspension_reference(provider_instance_id: Uuid, suspension_id: Uuid) -> ProviderSuspensionRef {
    ProviderSuspensionRef {
        provider_instance_id,
        suspension_id,
        reference_id: Uuid::new_v4(),
        content_digest: format!("sha256:{}", "d".repeat(64)),
        source_lease_id: Uuid::new_v4(),
        source_generation: 3,
        root_filesystem_bytes: 4096,
        guest_memory_bytes: 8192,
    }
}

fn hibernated_instance(
    provider: &LifecycleRecoveryProvider,
) -> (SandboxInstance, ProviderSuspensionRef) {
    let mut instance = SandboxInstance::new(
        BackendKind::Mock,
        WorkloadClass::AgentTool,
        "sha256:provider-retirement-recovery".to_string(),
        "provider-retirement-recovery".to_string(),
    );
    instance
        .transition(SandboxState::Creating)
        .expect("creating");
    instance.transition(SandboxState::Running).expect("running");
    instance
        .begin_hibernate_operation()
        .expect("begin completed hibernation");
    instance
        .transition(SandboxState::Hibernating)
        .expect("hibernating");
    instance
        .advance_hibernate_phase(OperationPhase::HibernatePublished)
        .expect("published hibernation image");
    instance.backend_ownership = BackendOwnership::Stopped;
    let suspension = suspension_reference(provider.descriptor.provider_instance_id, Uuid::new_v4());
    instance.provider_suspension = Some(suspension.to_record());
    instance
        .transition(SandboxState::Hibernated)
        .expect("hibernated");
    instance.finish_operation();
    provider
        .suspensions
        .lock()
        .expect("provider suspensions")
        .insert(suspension.suspension_id, suspension.reference_id);
    (instance, suspension)
}

fn publish_provider_checkpoint(
    manager: &SandboxManager,
    instance: &SandboxInstance,
    provider_instance_id: Uuid,
) -> String {
    let stage = manager
        .checkpoints
        .begin(instance.id)
        .expect("begin checkpoint stage");
    std::fs::write(
        stage.backend_payload_dir().join("vmstate.snap"),
        b"provider checkpoint ownership preflight",
    )
    .expect("write backend checkpoint payload");
    let public_checkpoint_id =
        validate_checkpoint_id(stage.id()).expect("generated checkpoint identity");
    let provider_checkpoint = ProviderCheckpointRecord {
        provider_instance_id,
        public_checkpoint_id,
        reference_id: Uuid::new_v4(),
        content_digest: format!("sha256:{}", "e".repeat(64)),
        parent_reference_id: None,
        source_lease_id: Uuid::new_v4(),
        source_generation: 1,
    };
    let checkpoint_id = stage.id().to_string();
    manager
        .checkpoints
        .publish(
            &stage,
            CommitCheckpoint {
                parent: None,
                policy_name: instance.policy_name.clone(),
                image_digest: instance.image_digest.clone(),
                backend: instance.backend,
                backend_version: Some("mock-v1".to_string()),
                snapshot_kind: SnapshotKind::Full,
                provider_checkpoint: Some(provider_checkpoint),
            },
        )
        .expect("publish provider checkpoint");
    checkpoint_id
}

#[tokio::test]
async fn restart_clears_a_prepare_intent_when_the_complete_context_is_absent() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(RecoveryProvider::new());
    let first = build_manager(&temporary, provider.clone());
    let (id, _) = persist_pending_prepare(&first, provider.descriptor.provider_instance_id);
    drop(first);

    let restarted = build_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.attempted, 1);
    assert_eq!(report.completed, 1);
    assert!(report.failures.is_empty());
    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
    assert_eq!(provider.inspect_calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn restart_aborts_a_prepared_lease_after_its_response_was_lost() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(RecoveryProvider::new());
    let first = build_manager(&temporary, provider.clone());
    let (id, context) = persist_pending_prepare(&first, provider.descriptor.provider_instance_id);
    provider
        .lose_prepare_response
        .store(true, Ordering::Release);
    assert!(matches!(
        provider.prepare(prepare_request(context)).await,
        Err(ProviderError::OutcomeUnknown)
    ));
    assert!(provider.binding().is_some());
    drop(first);

    let restarted = build_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.completed, 1);
    assert!(report.failures.is_empty());
    assert!(provider.binding().is_none());
    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
}

#[tokio::test]
async fn restart_reconstructs_and_aborts_a_daemon_managed_prepare_after_response_loss() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let images = temporary.path().join("images");
    let instances_root = temporary.path().join("instances");
    std::fs::create_dir_all(&images).expect("images directory");
    std::fs::create_dir_all(&instances_root).expect("instances directory");
    let first_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        images.clone(),
        instances_root.clone(),
    ));
    let first_provider = Arc::new(FileDataPlaneProvider::new(first_storage.clone()));
    let first =
        build_manager_with_storage(&temporary, first_provider.clone(), first_storage.clone());
    let (id, context) =
        persist_pending_prepare(&first, first_provider.descriptor().provider_instance_id);
    first_provider
        .prepare(prepare_request(context))
        .await
        .expect("prepare response not yet handed into the durable lease ledger");
    assert!(instances_root.join(id.to_string()).is_dir());
    drop(first);
    drop(first_provider);
    drop(first_storage);

    let restarted_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        images,
        instances_root.clone(),
    ));
    let restarted_provider = Arc::new(FileDataPlaneProvider::new(restarted_storage.clone()));
    let restarted = build_manager_with_storage(&temporary, restarted_provider, restarted_storage);
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.completed, 1);
    assert!(report.failures.is_empty());
    assert!(!instances_root.join(id.to_string()).exists());
    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
}

#[tokio::test]
async fn restart_retains_a_daemon_managed_slot_without_a_published_owner() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let images = temporary.path().join("images");
    let instances_root = temporary.path().join("instances");
    std::fs::create_dir_all(&images).expect("images directory");
    std::fs::create_dir_all(&instances_root).expect("instances directory");
    let first_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        images.clone(),
        instances_root.clone(),
    ));
    let first_provider = Arc::new(FileDataPlaneProvider::new(first_storage.clone()));
    let first =
        build_manager_with_storage(&temporary, first_provider.clone(), first_storage.clone());
    let (id, _context) =
        persist_pending_prepare(&first, first_provider.descriptor().provider_instance_id);
    first_storage
        .acquire(&AcquireOpts {
            instance_id: id.to_string(),
            rootfs_size: 4096,
            mem_size: 8192,
        })
        .await
        .expect("prepare side effect before ownership publication");
    drop(first);
    drop(first_provider);
    drop(first_storage);

    let restarted_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        images,
        instances_root.clone(),
    ));
    let restarted_provider = Arc::new(FileDataPlaneProvider::new(restarted_storage.clone()));
    let restarted = build_manager_with_storage(&temporary, restarted_provider, restarted_storage);
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.attempted, 1);
    assert_eq!(report.completed, 0);
    assert_eq!(report.failures.len(), 1);
    let retained = restarted.get(id).expect("retained recovery state");
    assert_eq!(retained.state, SandboxState::RecoveryRequired);
    assert!(
        retained
            .operation
            .is_some_and(|operation| operation.provider_operation.is_some())
    );
    assert!(instances_root.join(id.to_string()).is_dir());
}

#[tokio::test]
async fn restart_retains_the_write_ahead_identity_when_inspection_fails() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(RecoveryProvider::new());
    let first = build_manager(&temporary, provider.clone());
    let (id, _) = persist_pending_prepare(&first, provider.descriptor.provider_instance_id);
    *provider.inspect_error.lock().expect("inspect error") = Some(ProviderError::Unavailable);
    drop(first);

    let restarted = build_manager(&temporary, provider);
    let report = restarted
        .reconcile_startup()
        .await
        .expect("reconcile report");

    assert_eq!(report.completed, 0);
    assert_eq!(report.failures.len(), 1);
    let retained = restarted.get(id).expect("retained state");
    assert_eq!(retained.state, SandboxState::RecoveryRequired);
    assert!(
        retained
            .operation
            .is_some_and(|operation| operation.provider_operation.is_some())
    );
}

#[tokio::test]
async fn restart_retains_the_write_ahead_identity_when_abort_is_unknown() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(RecoveryProvider::new());
    let first = build_manager(&temporary, provider.clone());
    let (id, context) = persist_pending_prepare(&first, provider.descriptor.provider_instance_id);
    provider
        .prepare(prepare_request(context))
        .await
        .expect("prepare");
    *provider.abort_error.lock().expect("abort error") = Some(ProviderError::OutcomeUnknown);
    drop(first);

    let restarted = build_manager(&temporary, provider.clone());
    let report = restarted
        .reconcile_startup()
        .await
        .expect("reconcile report");

    assert_eq!(report.completed, 0);
    assert_eq!(report.failures.len(), 1);
    assert!(provider.binding().is_some());
    let retained = restarted.get(id).expect("retained state");
    assert_eq!(retained.state, SandboxState::RecoveryRequired);
    assert!(
        retained
            .operation
            .is_some_and(|operation| operation.provider_operation.is_some())
    );
}

#[tokio::test]
async fn restart_rejects_a_pending_operation_from_another_provider() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(RecoveryProvider::new());
    let first = build_manager(&temporary, provider.clone());
    let (id, _) = persist_pending_prepare(&first, Uuid::new_v4());
    drop(first);

    let restarted = build_manager(&temporary, provider.clone());
    let error = restarted
        .reconcile_startup()
        .await
        .expect_err("foreign provider identity must stop startup");

    assert!(error.to_string().contains("another provider"));
    assert_eq!(provider.inspect_calls.load(Ordering::Acquire), 0);
    let retained = restarted.get(id).expect("retained state");
    assert_eq!(retained.state, SandboxState::Creating);
    assert!(
        retained
            .operation
            .is_some_and(|operation| operation.provider_operation.is_some())
    );
}

#[tokio::test]
async fn restart_retires_an_unknown_checkpoint_capture_after_persisting_its_generation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    assert!(provider.inventory().is_none());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, source) = active_instance(&provider, 7);
    let public_checkpoint_id = Uuid::new_v4();
    let checkpoint_id = format!("ckpt-{public_checkpoint_id}");
    instance
        .begin_checkpoint_operation(checkpoint_id)
        .expect("begin checkpoint");
    instance
        .transition(SandboxState::Paused)
        .expect("pause for checkpoint");
    instance
        .advance_checkpoint_phase(OperationPhase::CheckpointPaused)
        .expect("record checkpoint pause");
    instance
        .begin_provider_operation(pending_capture(
            source,
            PendingProviderOperationKind::CheckpointCapture,
        ))
        .expect("checkpoint capture write-ahead identity");
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist checkpoint capture intent");
    provider
        .checkpoint(ProviderCheckpointRequest {
            binding: source,
            checkpoint_id: public_checkpoint_id,
            parent: None,
        })
        .await
        .expect("provider accepted checkpoint before response loss");
    drop(first);

    provider.reject_retirement.store(true, Ordering::Release);
    let first_restart = build_lifecycle_manager(&temporary, provider.clone());
    let first_report = first_restart
        .reconcile_startup()
        .await
        .expect("first reconciliation report");
    assert_eq!(first_report.attempted, 1);
    assert_eq!(first_report.completed, 0);
    assert_eq!(first_report.failures.len(), 1);
    let retained = first_restart.get(id).expect("retained checkpoint intent");
    assert_eq!(retained.state, SandboxState::RecoveryRequired);
    assert_eq!(
        retained
            .data_plane_lease
            .expect("observed active lease")
            .generation,
        8
    );
    assert!(
        retained
            .operation
            .is_some_and(|operation| operation.provider_operation.is_some())
    );
    drop(first_restart);

    provider.reject_retirement.store(false, Ordering::Release);
    let second_restart = build_lifecycle_manager(&temporary, provider.clone());
    let second_report = second_restart
        .reconcile_startup()
        .await
        .expect("second reconciliation report");
    assert_eq!(second_report.completed, 1);
    assert!(second_report.failures.is_empty());
    assert_eq!(
        second_restart.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
    assert!(
        provider
            .retirement_log()
            .contains(&RetiredIdentity::Checkpoint(public_checkpoint_id, None,))
    );
    assert_eq!(provider.lease_count(), 0);
}

#[tokio::test]
async fn restart_retires_an_unknown_suspension_capture_without_provider_inventory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    assert!(provider.inventory().is_none());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, source) = active_instance(&provider, 5);
    let suspension_id = Uuid::new_v4();
    instance
        .begin_hibernate_operation()
        .expect("begin hibernation");
    instance
        .transition(SandboxState::Hibernating)
        .expect("enter hibernating state");
    instance
        .advance_hibernate_phase(OperationPhase::HibernatePaused)
        .expect("record hibernation pause");
    instance
        .begin_provider_operation(pending_capture(
            source,
            PendingProviderOperationKind::SuspensionCapture { suspension_id },
        ))
        .expect("suspension capture write-ahead identity");
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist suspension capture intent");
    provider
        .suspend(SuspendRequest {
            binding: source,
            suspension_id,
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
        })
        .await
        .expect("provider accepted suspension before response loss");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.attempted, 1);
    assert_eq!(report.completed, 1);
    assert!(report.failures.is_empty());
    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
    assert!(
        provider
            .retirement_log()
            .contains(&RetiredIdentity::Suspension(suspension_id, None,))
    );
    assert_eq!(provider.lease_count(), 0);
}

#[tokio::test]
async fn restart_aborts_a_checkpoint_restore_replacement_without_provider_inventory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    assert!(provider.inventory().is_none());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, active) = active_instance(&provider, 11);
    let public_checkpoint_id = Uuid::new_v4();
    let checkpoint = checkpoint_reference(
        provider.descriptor.provider_instance_id,
        public_checkpoint_id,
        active,
    );
    let context = replacement_context(instance.id);
    instance
        .begin_restore_operation(format!("ckpt-{public_checkpoint_id}"))
        .expect("begin restore");
    instance
        .begin_provider_operation(pending_prepare(
            provider.descriptor.provider_instance_id,
            context,
        ))
        .expect("restore preparation write-ahead identity");
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist restore preparation intent");
    provider
        .restore_checkpoint(RestoreCheckpointRequest {
            context,
            checkpoint,
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
        })
        .await
        .expect("provider prepared replacement before response loss");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.attempted, 1);
    assert_eq!(report.completed, 1);
    assert!(report.failures.is_empty());
    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
    assert_eq!(provider.stopped_generations(), vec![11]);
    assert_eq!(provider.lease_count(), 0);
}

#[tokio::test]
async fn restart_aborts_a_resume_replacement_without_provider_inventory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    assert!(provider.inventory().is_none());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let mut instance = SandboxInstance::new(
        BackendKind::Mock,
        WorkloadClass::AgentTool,
        "sha256:provider-resume-recovery".to_string(),
        "provider-resume-recovery".to_string(),
    );
    instance
        .transition(SandboxState::Creating)
        .expect("creating");
    instance.transition(SandboxState::Running).expect("running");
    instance
        .begin_hibernate_operation()
        .expect("begin completed hibernation");
    instance
        .transition(SandboxState::Hibernating)
        .expect("hibernating");
    instance
        .advance_hibernate_phase(OperationPhase::HibernatePublished)
        .expect("publish hibernation image");
    instance.backend_ownership = BackendOwnership::Stopped;
    let suspension = suspension_reference(provider.descriptor.provider_instance_id, Uuid::new_v4());
    instance.provider_suspension = Some(suspension.to_record());
    instance
        .transition(SandboxState::Hibernated)
        .expect("hibernated");
    instance.finish_operation();
    provider
        .suspensions
        .lock()
        .expect("provider suspensions")
        .insert(suspension.suspension_id, suspension.reference_id);
    let context = replacement_context(instance.id);
    instance
        .begin_resume_operation()
        .expect("begin resume operation");
    instance
        .transition(SandboxState::Resuming)
        .expect("resuming");
    instance
        .begin_provider_operation(pending_prepare(
            provider.descriptor.provider_instance_id,
            context,
        ))
        .expect("resume preparation write-ahead identity");
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist resume preparation intent");
    provider
        .resume(ResumeRequest {
            context,
            suspension: suspension.clone(),
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
        })
        .await
        .expect("provider prepared resume lease before response loss");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.attempted, 1);
    assert_eq!(report.completed, 1);
    assert!(report.failures.is_empty());
    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
    assert!(
        provider
            .retirement_log()
            .contains(&RetiredIdentity::Suspension(
                suspension.suspension_id,
                Some(suspension.reference_id),
            ))
    );
    assert_eq!(provider.lease_count(), 0);
}

#[tokio::test]
async fn failed_restore_prepare_compensation_keeps_the_old_running_generation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let manager = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, active) = active_instance(&provider, 13);
    let context = replacement_context(instance.id);
    let checkpoint_id = Uuid::new_v4();
    let checkpoint = checkpoint_reference(
        provider.descriptor.provider_instance_id,
        checkpoint_id,
        active,
    );
    instance
        .begin_restore_operation(format!("ckpt-{checkpoint_id}"))
        .expect("begin restore");
    instance
        .begin_provider_operation(pending_prepare(
            provider.descriptor.provider_instance_id,
            context,
        ))
        .expect("restore preparation write-ahead identity");
    let id = instance.id;
    manager
        .persist_and_retain(instance.clone())
        .expect("persist restore preparation intent");
    manager
        .retain_data_plane_lease(id, active)
        .expect("retain active lease cache");
    provider
        .restore_checkpoint(RestoreCheckpointRequest {
            context,
            checkpoint,
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
        })
        .await
        .expect("prepare replacement");

    let failure = manager
        .finish_failed_provider_restore_prepare(instance, ProviderError::InvalidResponse.into())
        .await;

    assert!(matches!(
        failure,
        BlazeDaemonError::DataPlane(ProviderError::InvalidResponse)
    ));
    let retained = manager.get(id).expect("compensated running state");
    assert_eq!(retained.state, SandboxState::Running);
    assert!(retained.operation.is_none());
    assert!(retained.replacement_data_plane_lease.is_none());
    assert_eq!(
        retained
            .data_plane_lease
            .expect("old active lease")
            .generation,
        13
    );
    assert_eq!(provider.lease_count(), 1);
}

#[tokio::test]
async fn destroy_rejects_a_foreign_published_checkpoint_before_any_cleanup() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let manager = build_lifecycle_manager(&temporary, provider.clone());
    let (instance, _) = active_instance(&provider, 3);
    let id = instance.id;
    manager
        .persist_and_retain(instance.clone())
        .expect("persist active instance");
    let checkpoint_id = publish_provider_checkpoint(&manager, &instance, Uuid::new_v4());

    let error = manager
        .destroy(id)
        .await
        .expect_err("foreign checkpoint must stop destroy");

    assert!(error.to_string().contains("another provider"));
    let retained = manager.get(id).expect("original lifecycle state");
    assert_eq!(retained.state, SandboxState::Running);
    assert!(retained.operation.is_none());
    assert_eq!(retained.data_plane_lease, instance.data_plane_lease);
    assert_eq!(provider.cleanup_calls.load(Ordering::Acquire), 0);
    assert_eq!(provider.lease_count(), 1);
    assert_eq!(
        manager.checkpoints.list(id).expect("checkpoint catalog")[0].id,
        checkpoint_id
    );
}

#[tokio::test]
async fn startup_rejects_a_foreign_published_checkpoint_before_any_cleanup() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (instance, _) = active_instance(&provider, 3);
    let id = instance.id;
    first
        .persist_and_retain(instance.clone())
        .expect("persist active instance");
    let checkpoint_id = publish_provider_checkpoint(&first, &instance, Uuid::new_v4());
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let error = restarted
        .reconcile_startup()
        .await
        .expect_err("foreign checkpoint must stop startup recovery");

    assert!(error.to_string().contains("another provider"));
    let retained = restarted.get(id).expect("original lifecycle state");
    assert_eq!(retained.state, SandboxState::Running);
    assert!(retained.operation.is_none());
    assert_eq!(retained.data_plane_lease, instance.data_plane_lease);
    assert_eq!(provider.cleanup_calls.load(Ordering::Acquire), 0);
    assert_eq!(provider.lease_count(), 1);
    assert_eq!(
        restarted.checkpoints.list(id).expect("checkpoint catalog")[0].id,
        checkpoint_id
    );
}

#[tokio::test]
async fn restart_accepts_a_released_prepare_tombstone_at_the_exact_next_generation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(RecoveryProvider::new());
    let first = build_manager(&temporary, provider.clone());
    let (id, context) = persist_pending_prepare(&first, provider.descriptor.provider_instance_id);
    *provider.binding.lock().expect("provider binding") = Some(LeaseBinding {
        provider_instance_id: provider.descriptor.provider_instance_id,
        context,
        generation: context.generation + 1,
        state: LeaseState::Released,
    });
    drop(first);

    let restarted = build_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.completed, 1);
    let terminal = restarted.get(id).expect("terminal state");
    assert_eq!(terminal.state, SandboxState::Destroyed);
    assert!(terminal.operation.is_none());
    assert!(terminal.data_plane_lease.is_none());
}

#[tokio::test]
async fn restart_persists_an_exact_transition_success_before_cleanup() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, active) = active_instance(&provider, 9);
    instance.begin_operation(OperationKind::Destroy);
    instance
        .begin_provider_transition(PendingProviderTransitionRecord {
            kind: ProviderTransitionKind::Stop,
            lease_slot: ProviderLeaseSlot::Active,
            before: active.to_record(4096, 8192),
            target_state: DataPlaneLeaseState::Stopped,
            public_transition: None,
            backend_process: None,
        })
        .expect("stop transition WAL");
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist transition WAL");
    provider
        .stop(StopRequest { binding: active })
        .await
        .expect("provider stopped before response loss");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.completed, 1);
    assert_eq!(provider.stopped_generations(), vec![9]);
    assert_eq!(provider.lease_count(), 0);
    let terminal = restarted.get(id).expect("terminal state");
    assert_eq!(terminal.state, SandboxState::Destroyed);
    assert!(terminal.provider_transition.is_none());
}

#[tokio::test]
async fn retirement_operation_identity_is_stable_across_restart() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, _) = hibernated_instance(&provider);
    let source = finalized_binding(provider.descriptor.provider_instance_id, instance.id, 4);
    let checkpoint = checkpoint_reference(
        provider.descriptor.provider_instance_id,
        Uuid::new_v4(),
        source,
    );
    provider
        .checkpoints
        .lock()
        .expect("provider checkpoints")
        .insert(checkpoint.public_checkpoint_id, checkpoint.reference_id);
    instance
        .pending_provider_retirements
        .push(checkpoint.to_record());
    first
        .persist_and_retain(instance)
        .expect("persist retirement ledger");
    drop(first);

    provider.reject_retirement.store(true, Ordering::Release);
    let rejected = build_lifecycle_manager(&temporary, provider.clone());
    rejected
        .reconcile_startup()
        .await
        .expect_err("retirement remains pending");
    drop(rejected);

    provider.reject_retirement.store(false, Ordering::Release);
    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    restarted
        .reconcile_startup()
        .await
        .expect("retirement converges");
    let matching = provider
        .retirement_attempts()
        .into_iter()
        .filter(|(identity, _)| {
            *identity
                == RetiredIdentity::Checkpoint(
                    checkpoint.public_checkpoint_id,
                    Some(checkpoint.reference_id),
                )
        })
        .map(|(_, operation_id)| operation_id)
        .collect::<Vec<_>>();
    assert!(matching.len() >= 3);
    assert!(
        matching
            .iter()
            .all(|operation_id| *operation_id == matching[0])
    );
}

#[tokio::test]
async fn startup_reads_the_checkpoint_catalog_before_retrying_retirement() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, _) = hibernated_instance(&provider);
    let id = instance.id;
    publish_provider_checkpoint(&first, &instance, provider.descriptor.provider_instance_id);
    let checkpoint = first
        .checkpoints
        .list_metadata(id)
        .expect("checkpoint catalog")
        .into_iter()
        .next()
        .and_then(|metadata| metadata.provider_checkpoint)
        .expect("provider checkpoint owner");
    provider
        .checkpoints
        .lock()
        .expect("provider checkpoints")
        .insert(checkpoint.public_checkpoint_id, checkpoint.reference_id);
    instance
        .pending_provider_retirements
        .push(checkpoint.clone());
    first
        .persist_and_retain(instance)
        .expect("persist retirement ledger beside public catalog owner");
    drop(first);

    provider.reject_retirement.store(true, Ordering::Release);
    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let report = restarted
        .reconcile_startup()
        .await
        .expect("retirement failure is reported per sandbox");
    assert_eq!(report.attempted, 1);
    assert_eq!(report.completed, 0);
    assert_eq!(report.failures.len(), 1);

    assert!(
        restarted
            .checkpoints
            .list_metadata(id)
            .expect("checkpoint catalog after cleanup")
            .is_empty(),
        "the catalog owner must be removed before provider retirement is attempted"
    );
    assert!(provider.retirement_attempts().iter().any(|(identity, _)| {
        *identity
            == RetiredIdentity::Checkpoint(
                checkpoint.public_checkpoint_id,
                Some(checkpoint.reference_id),
            )
    }));
    assert!(
        restarted
            .get(id)
            .expect("retained lifecycle")
            .pending_provider_retirements
            .contains(&checkpoint)
    );

    provider.reject_retirement.store(false, Ordering::Release);
    restarted
        .reconcile_startup()
        .await
        .expect("retirement retry after catalog removal");
    assert_eq!(
        restarted.get(id).expect("terminal lifecycle").state,
        SandboxState::Destroyed
    );
}

#[tokio::test]
async fn restart_retires_distinct_checkpoint_owners_that_share_one_reference() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, _) = hibernated_instance(&provider);
    let source = finalized_binding(provider.descriptor.provider_instance_id, instance.id, 4);
    let first_checkpoint = checkpoint_reference(
        provider.descriptor.provider_instance_id,
        Uuid::new_v4(),
        source,
    );
    let mut second_checkpoint = checkpoint_reference(
        provider.descriptor.provider_instance_id,
        Uuid::new_v4(),
        source,
    );
    second_checkpoint.reference_id = first_checkpoint.reference_id;
    {
        let mut checkpoints = provider.checkpoints.lock().expect("provider checkpoints");
        checkpoints.insert(
            first_checkpoint.public_checkpoint_id,
            first_checkpoint.reference_id,
        );
        checkpoints.insert(
            second_checkpoint.public_checkpoint_id,
            second_checkpoint.reference_id,
        );
    }
    instance
        .pending_provider_retirements
        .push(first_checkpoint.to_record());
    instance
        .pending_provider_retirements
        .push(second_checkpoint.to_record());
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist shared-reference retirements");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    restarted.reconcile_startup().await.expect("reconcile");

    let retained = restarted.get(id).expect("hibernated instance");
    assert_eq!(retained.state, SandboxState::Hibernated);
    assert!(retained.pending_provider_retirements.is_empty());
    let retirements = provider.retirement_log();
    assert!(retirements.contains(&RetiredIdentity::Checkpoint(
        first_checkpoint.public_checkpoint_id,
        Some(first_checkpoint.reference_id),
    )));
    assert!(retirements.contains(&RetiredIdentity::Checkpoint(
        second_checkpoint.public_checkpoint_id,
        Some(second_checkpoint.reference_id),
    )));
}

#[tokio::test]
async fn restart_retires_only_an_obsolete_suspension_with_a_shared_reference() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, current) = hibernated_instance(&provider);
    let mut obsolete =
        suspension_reference(provider.descriptor.provider_instance_id, Uuid::new_v4());
    obsolete.reference_id = current.reference_id;
    provider
        .suspensions
        .lock()
        .expect("provider suspensions")
        .insert(obsolete.suspension_id, obsolete.reference_id);
    instance
        .pending_provider_suspension_retirements
        .push(obsolete.to_record());
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist obsolete suspension");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    restarted.reconcile_startup().await.expect("reconcile");

    let retained = restarted.get(id).expect("hibernated instance");
    assert_eq!(retained.provider_suspension, Some(current.to_record()));
    assert!(retained.pending_provider_suspension_retirements.is_empty());
    let suspensions = provider.suspensions.lock().expect("provider suspensions");
    assert_eq!(
        suspensions.get(&current.suspension_id),
        Some(&current.reference_id)
    );
    assert!(!suspensions.contains_key(&obsolete.suspension_id));
}

#[tokio::test]
async fn inventory_preserves_an_exact_lease_awaiting_explicit_hibernate_cleanup() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    provider.inventory_enabled.store(true, Ordering::Release);
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, binding) = active_instance(&provider, 5);
    instance
        .begin_hibernate_operation()
        .expect("begin hibernation");
    let id = instance.id;
    first
        .mark_instance_recovery(instance)
        .expect("persist explicit cleanup state");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.attempted, 0);
    assert_eq!(report.completed, 0);
    assert!(report.failures.is_empty());
    assert_eq!(
        provider
            .leases
            .lock()
            .expect("provider leases")
            .get(&binding.context.lease_id)
            .copied(),
        Some(binding)
    );
    assert!(
        provider
            .reconcile_actions
            .lock()
            .expect("reconcile actions")
            .is_empty()
    );
    let retained = restarted.get(id).expect("explicit cleanup state");
    assert_eq!(retained.state, SandboxState::RecoveryRequired);
    assert_eq!(
        retained.operation.as_ref().map(|operation| operation.kind),
        Some(OperationKind::Hibernate)
    );

    assert!(restarted.destroy(id).await.expect("explicit destroy"));
    assert_eq!(provider.lease_count(), 0);
    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
}

#[tokio::test]
async fn inventory_quarantines_a_mismatched_lease_for_an_explicit_cleanup_record() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    provider.inventory_enabled.store(true, Ordering::Release);
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, expected) = active_instance(&provider, 7);
    instance
        .begin_hibernate_operation()
        .expect("begin hibernation");
    first
        .mark_instance_recovery(instance)
        .expect("persist explicit cleanup state");
    let observed = LeaseBinding {
        generation: expected.generation + 1,
        ..expected
    };
    provider
        .leases
        .lock()
        .expect("provider leases")
        .insert(expected.context.lease_id, observed);
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    let report = restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(report.attempted, 0);
    assert_eq!(report.completed, 0);
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].error.contains("no public owner"));
    assert_eq!(
        provider
            .reconcile_actions
            .lock()
            .expect("reconcile actions")
            .as_slice(),
        &[ReconcileAction::Quarantine]
    );
    assert_eq!(
        provider
            .leases
            .lock()
            .expect("provider leases")
            .get(&expected.context.lease_id)
            .map(|binding| binding.state),
        Some(LeaseState::Quarantined)
    );
}

#[tokio::test]
async fn startup_cleans_an_interrupted_replacement_before_inventory_classification() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    provider.inventory_enabled.store(true, Ordering::Release);
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, _) = active_instance(&provider, 6);
    instance
        .begin_restore_operation(format!("ckpt-{}", Uuid::new_v4()))
        .expect("begin restore");
    let replacement = provider
        .prepare_replacement(
            replacement_context(instance.id),
            PreparedResources::CheckpointRestore {
                storage: Some(test_storage_slot(instance.id)),
                attachments: Vec::new(),
            },
        )
        .expect("prepare replacement");
    instance.replacement_data_plane_lease = Some(replacement.binding.to_record(4096, 8192));
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist replacement ownership");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    restarted.reconcile_startup().await.expect("reconcile");

    assert_eq!(
        restarted.get(id).expect("terminal state").state,
        SandboxState::Destroyed
    );
    assert_eq!(provider.lease_count(), 0);
    assert!(
        provider
            .reconcile_actions
            .lock()
            .expect("actions")
            .is_empty()
    );
}

#[tokio::test]
async fn startup_settles_an_adoption_wal_after_the_recorded_backend_exits() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    provider.inventory_enabled.store(true, Ordering::Release);
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, binding) = active_instance(&provider, 8);
    let process = BackendProcessIdentity {
        pid: 41,
        start_time_ticks: 73,
    };
    instance.backend_ownership = BackendOwnership::Running;
    instance.backend_runtime = Some(BackendRuntimeRecord {
        process: Some(process),
        version: Some("mock-v1".to_string()),
        guest_transport: false,
        network_slot: false,
        console_log: false,
    });
    instance
        .begin_provider_transition(PendingProviderTransitionRecord {
            kind: ProviderTransitionKind::Adopt,
            lease_slot: ProviderLeaseSlot::Active,
            before: binding.to_record(4096, 8192),
            target_state: DataPlaneLeaseState::Finalized,
            public_transition: None,
            backend_process: Some(process),
        })
        .expect("persist adoption write-ahead identity");
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist interrupted adoption");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider.clone());
    restarted
        .reconcile_startup()
        .await
        .expect("an exited backend must not block startup");
    let terminal = restarted.get(id).expect("terminal lifecycle");
    assert_eq!(terminal.state, SandboxState::Destroyed);
    assert!(terminal.provider_transition.is_none());
    assert!(terminal.backend_runtime.is_none());
    assert_eq!(terminal.backend_ownership, BackendOwnership::Stopped);
    assert_eq!(provider.lease_count(), 0);
    drop(restarted);

    let second_restart = build_lifecycle_manager(&temporary, provider);
    second_restart
        .reconcile_startup()
        .await
        .expect("settled adoption must remain non-blocking on later starts");
}

#[tokio::test]
async fn startup_settles_an_adoption_wal_when_backend_and_provider_lease_are_absent() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    provider.inventory_enabled.store(true, Ordering::Release);
    let first = build_lifecycle_manager(&temporary, provider.clone());
    let (mut instance, binding) = active_instance(&provider, 8);
    provider
        .leases
        .lock()
        .expect("provider leases")
        .remove(&binding.context.lease_id);
    let process = BackendProcessIdentity {
        pid: 43,
        start_time_ticks: 79,
    };
    instance.backend_ownership = BackendOwnership::Running;
    instance.backend_runtime = Some(BackendRuntimeRecord {
        process: Some(process),
        version: Some("mock-v1".to_string()),
        guest_transport: false,
        network_slot: false,
        console_log: false,
    });
    instance
        .begin_provider_transition(PendingProviderTransitionRecord {
            kind: ProviderTransitionKind::Adopt,
            lease_slot: ProviderLeaseSlot::Active,
            before: binding.to_record(4096, 8192),
            target_state: DataPlaneLeaseState::Finalized,
            public_transition: None,
            backend_process: Some(process),
        })
        .expect("persist adoption write-ahead identity");
    let id = instance.id;
    first
        .persist_and_retain(instance)
        .expect("persist interrupted adoption");
    drop(first);

    let restarted = build_lifecycle_manager(&temporary, provider);
    restarted
        .reconcile_startup()
        .await
        .expect("absent backend and lease must not block startup");
    let terminal = restarted.get(id).expect("terminal lifecycle");
    assert_eq!(terminal.state, SandboxState::Destroyed);
    assert!(terminal.provider_transition.is_none());
    assert!(terminal.data_plane_lease.is_none());
}

#[tokio::test]
async fn startup_inventory_rejects_malformed_pages_and_released_leases() {
    for behavior in [
        InventoryBehavior::EmptyContinuation,
        InventoryBehavior::OversizedCursor,
        InventoryBehavior::ReleasedLease,
    ] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let provider = Arc::new(LifecycleRecoveryProvider::new());
        provider.inventory_enabled.store(true, Ordering::Release);
        provider.set_inventory_behavior(behavior);
        let manager = build_lifecycle_manager(&temporary, provider.clone());

        let error = manager
            .reconcile_startup()
            .await
            .expect_err("invalid inventory must fail closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(provider.inventory_page_calls.load(Ordering::Acquire), 1);
    }
}

#[tokio::test]
async fn startup_inventory_stops_at_the_total_page_bound() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let provider = Arc::new(LifecycleRecoveryProvider::new());
    provider.inventory_enabled.store(true, Ordering::Release);
    provider.set_inventory_behavior(InventoryBehavior::EndlessDistinctCursors);
    let manager = build_lifecycle_manager(&temporary, provider.clone());

    let error = manager
        .reconcile_startup()
        .await
        .expect_err("unbounded inventory must fail closed");

    assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
    assert_eq!(
        provider.inventory_page_calls.load(Ordering::Acquire),
        MAX_INVENTORY_PAGES
    );
}
