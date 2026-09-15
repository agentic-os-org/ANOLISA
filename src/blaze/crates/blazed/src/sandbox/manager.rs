// SPDX-License-Identifier: Apache-2.0
//! Recoverable sandbox create, destroy, and startup cleanup.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blaze_core::BlazeError;
use blaze_core::backend::{BackendKind, RestoreRequest, SpawnRequest};
use blaze_core::checkpoint::{CheckpointMetadata, validate_checkpoint_id};
use blaze_core::data_plane::{
    BackendProcessIdentity, DataPlaneLeaseState, PendingProviderOperationKind,
    PendingProviderOperationRecord,
};
use blaze_core::lifecycle::{
    BackendOwnership, OperationKind, PendingProviderTransitionRecord, ProviderLeaseSlot,
    ProviderPublicTransitionRecord, ProviderTransitionKind, SandboxInstance, SandboxState,
};
use blaze_core::policy::RuntimeDecision;
use blaze_core::storage::{StorageProvider, StorageSlot};
use blaze_provider_api::{
    AbortRequest, BeginInventoryRequest, CapacityRequest, CapacityScope, CapacitySnapshot,
    CommitRequest, DataPlaneProvider, DrainRequest, DrainResult, FinalizeRequest, InspectRequest,
    InventoryPageRequest, LeaseBinding, LeaseState, MAX_INVENTORY_PAGES, PrepareRequest,
    PrepareSource, PreparedLease, PreparedResources, ProviderCheckpointRef, ProviderError,
    PublicTransitionRef, ReconcileAction, ReconcileRequest, ReleaseRequest, RequestContext,
    StopRequest, TemplateSource,
};
use blaze_provider_conformance::{
    validate_capacity_snapshot, validate_descriptor, validate_drain_result,
    validate_inventory_lease, validate_inventory_page, validate_inventory_snapshot,
    validate_prepared, validate_prepared_binding, validate_reconcile_result, validate_transition,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::checkpoint_store::CheckpointStore;
use crate::error::{BlazeDaemonError, Result};
use crate::guest::{GuestClient, GuestExecResult, MAX_GUEST_FILE_BYTES};
use crate::metrics::Metrics;
use crate::sandbox::template::{ResolvedTemplate, TemplateCatalog};
use crate::spawner::{
    BackendRestoreRequest, BackendSpawnRequest, DynBackendInstance, DynSpawner, PinnedExecutable,
    SpawnerRegistry, adopt_with_runtime_directory, restore_with_runtime_directory,
    spawn_with_runtime_directory,
};
use crate::state_store::{OwnedRunDir, StateStore};

pub(super) const GUEST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Inputs already parsed and policy-evaluated by the API.
#[derive(Debug, Clone)]
pub struct CreateSandbox {
    /// Policy decision for this request.
    pub decision: RuntimeDecision,
    /// Image identity used by storage allocation.
    pub image_digest: String,
    /// Concrete backend selected from the policy and daemon availability.
    pub runtime_backend: BackendKind,
    /// Executable selected during daemon startup.
    pub binary_path: PathBuf,
    /// Published template to restore from, when the request named one.
    pub template: Option<String>,
}

/// Prepared inputs for one template-backed create, validated before allocation.
struct TemplateCreate {
    resolved: ResolvedTemplate,
    spawner: DynSpawner,
    executable: Option<Arc<PinnedExecutable>>,
    /// Console-recording shape the matched policy would launch.
    ///
    /// A restore derives its effective backend config from the request, so this
    /// must carry the policy's setting instead of silently disabling recording.
    record_console_log: bool,
}

/// Restore inputs derived from a materialized template slot.
struct TemplateRestore {
    payload_dir: PathBuf,
    expected_version: Option<String>,
    snapshot_kind: blaze_core::backend::SnapshotKind,
    expose_guest_socket: bool,
    preserve_network: bool,
    record_console_log: bool,
}

/// Restore metadata retained while the provider prepares its payload.
struct TemplateRestorePlan {
    expected_version: Option<String>,
    snapshot_kind: blaze_core::backend::SnapshotKind,
    expose_guest_socket: bool,
    preserve_network: bool,
    record_console_log: bool,
}

/// Provider preparation converted into inputs understood by existing backends.
struct PreparedCreateResources {
    binding: LeaseBinding,
    storage: Option<StorageSlot>,
    provider_attachments: Option<crate::spawner::ProviderRestoreAttachments>,
}

/// Result of one managed create request.
#[derive(Debug, Clone)]
pub struct CreateSandboxResult {
    /// Persisted sandbox metadata.
    pub instance: SandboxInstance,
    /// Backend implementation that owns the runtime.
    pub selected_backend: BackendKind,
}

/// One startup cleanup failure. Other records continue to be reconciled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileFailure {
    /// Sandbox whose cleanup remains incomplete.
    pub instance_id: Uuid,
    /// Actionable failure description.
    pub error: String,
}

/// Aggregate startup cleanup outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Number of non-terminal records examined.
    pub attempted: usize,
    /// Number safely adopted or moved to the terminal state.
    pub completed: usize,
    /// Records that remain recoverable.
    pub failures: Vec<ReconcileFailure>,
}

/// Owns durable lifecycle metadata and non-serializable runtime handles.
///
/// The maps are shared with read-only and non-lifecycle API paths. All
/// Create, destroy, and restart cleanup mutations enter
/// through this type and are serialized by a per-sandbox async lock.
pub struct SandboxManager {
    instances: Arc<Mutex<HashMap<Uuid, SandboxInstance>>>,
    backend_instances: Arc<Mutex<HashMap<Uuid, DynBackendInstance>>>,
    operation_locks: Mutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>,
    pub(super) storage_sync_inflight: Arc<Mutex<HashSet<Uuid>>>,
    pub(super) storage_sync_permits: Arc<Semaphore>,
    spawners: Arc<SpawnerRegistry>,
    active_backend: BackendKind,
    pub(super) storage: Arc<dyn StorageProvider>,
    pub(super) data_plane: Arc<dyn DataPlaneProvider>,
    data_plane_leases: Mutex<HashMap<Uuid, LeaseBinding>>,
    state_store: StateStore,
    pub(super) checkpoints: CheckpointStore,
    rootfs_size: u64,
    mem_size: u64,
    metrics: Arc<Metrics>,
    pub(super) template_catalog: TemplateCatalog,
}

/// Construction inputs grouped to keep daemon wiring explicit.
pub struct SandboxManagerInit {
    pub instances: HashMap<Uuid, SandboxInstance>,
    pub spawners: SpawnerRegistry,
    pub active_backend: BackendKind,
    pub storage: Arc<dyn StorageProvider>,
    pub data_plane: Arc<dyn DataPlaneProvider>,
    pub state_store: StateStore,
    pub rootfs_size: u64,
    pub mem_size: u64,
    pub template_catalog: TemplateCatalog,
}

/// Shared resources returned to the daemon wiring and test harness.
pub struct SandboxManagerResources {
    #[cfg(test)]
    pub instances: Arc<Mutex<HashMap<Uuid, SandboxInstance>>>,
    pub metrics: Arc<Metrics>,
}

impl SandboxManager {
    /// Return the retained runtime-directory owner for one sandbox.
    pub(super) fn run_directory(&self, id: Uuid) -> Result<OwnedRunDir> {
        self.state_store.run_dir(id)
    }

    /// Build a manager around state loaded from the durable state directory.
    pub fn new(init: SandboxManagerInit) -> (Self, SandboxManagerResources) {
        let SandboxManagerInit {
            instances,
            spawners,
            active_backend,
            storage,
            data_plane,
            state_store,
            rootfs_size,
            mem_size,
            template_catalog,
        } = init;
        let operation_locks = instances
            .keys()
            .copied()
            .map(|id| (id, Arc::new(AsyncMutex::new(()))))
            .collect();
        let provider_instance_id = data_plane.descriptor().provider_instance_id;
        let data_plane_leases = instances
            .values()
            .filter_map(|instance| {
                instance
                    .data_plane_lease
                    .filter(|record| record.provider_instance_id == provider_instance_id)
                    .map(|record| (instance.id, LeaseBinding::from_record(instance.id, record)))
            })
            .collect();
        let instances = Arc::new(Mutex::new(instances));
        let backend_instances = Arc::new(Mutex::new(HashMap::new()));
        let metrics = Arc::new(Metrics::new());
        let checkpoints = CheckpointStore::new(state_store.clone());
        let resources = SandboxManagerResources {
            #[cfg(test)]
            instances: instances.clone(),
            metrics: metrics.clone(),
        };
        (
            Self {
                instances,
                backend_instances,
                operation_locks: Mutex::new(operation_locks),
                storage_sync_inflight: Arc::new(Mutex::new(HashSet::new())),
                // The periodic worker is sequential. Retain that bound when a
                // timed-out provider operation has to finish in the background.
                storage_sync_permits: Arc::new(Semaphore::new(1)),
                spawners: Arc::new(spawners),
                active_backend,
                storage,
                data_plane,
                data_plane_leases: Mutex::new(data_plane_leases),
                state_store,
                checkpoints,
                rootfs_size,
                mem_size,
                metrics,
                template_catalog,
            },
            resources,
        )
    }

    /// Return the async operation lock that serializes one sandbox mutation.
    pub fn operation_lock(&self, id: Uuid) -> Arc<AsyncMutex<()>> {
        match self.operation_locks.lock() {
            Ok(mut locks) => locks
                .entry(id)
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone(),
            Err(poisoned) => poisoned
                .into_inner()
                .entry(id)
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone(),
        }
    }

    pub(crate) fn backend_owner(&self, id: Uuid) -> Option<DynBackendInstance> {
        match self.backend_instances.lock() {
            Ok(instances) => instances.get(&id).cloned(),
            Err(poisoned) => poisoned.into_inner().get(&id).cloned(),
        }
    }

    pub(super) fn spawner(&self, backend: BackendKind) -> Option<DynSpawner> {
        self.spawners.get(backend)
    }

    pub(super) fn remove_backend_owner(&self, id: Uuid) -> Option<DynBackendInstance> {
        match self.backend_instances.lock() {
            Ok(mut instances) => instances.remove(&id),
            Err(poisoned) => poisoned.into_inner().remove(&id),
        }
    }

    #[cfg(test)]
    pub(crate) fn insert_backend_owner(&self, id: Uuid, owner: DynBackendInstance) -> Result<()> {
        self.backend_instances
            .lock()
            .map_err(|_| poisoned("backend_instances"))?
            .insert(id, owner);
        Ok(())
    }

    pub(super) async fn reconstruct_storage(&self, id: Uuid) -> Result<StorageSlot> {
        self.storage
            .reconstruct(&id.to_string())
            .await
            .map_err(Into::into)
    }

    pub(super) async fn sync_storage(&self, slot: &StorageSlot) -> Result<()> {
        self.storage.sync_artifacts(slot).await.map_err(Into::into)
    }

    async fn prepare_data_plane(
        &self,
        instance: &SandboxInstance,
        request: PrepareRequest,
    ) -> Result<PreparedLease> {
        let descriptor = self.data_plane.descriptor();
        validate_descriptor(descriptor).map_err(|_| {
            BlazeDaemonError::Internal("data-plane descriptor is incompatible".to_string())
        })?;
        instance.validate_provider_operation().map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "create {} has an invalid provider write-ahead identity: {error}",
                instance.id
            ))
        })?;
        let pending = instance
            .operation
            .as_ref()
            .and_then(|operation| operation.provider_operation)
            .ok_or_else(|| {
                BlazeDaemonError::RecoveryRequired(format!(
                    "create {} has no durable provider write-ahead identity",
                    instance.id
                ))
            })?;
        let durable_context = RequestContext::from(pending.context);
        if pending.provider_instance_id != descriptor.provider_instance_id
            || pending.kind != PendingProviderOperationKind::PrepareLease
            || durable_context != request.context
            || pending.root_filesystem_bytes != request.root_filesystem_bytes
            || pending.guest_memory_bytes != request.guest_memory_bytes
        {
            return Err(self.retain_unresolved_prepare(
                instance,
                "provider request does not match its durable write-ahead identity".to_string(),
            ));
        }
        let capabilities = self.data_plane.capabilities();
        let supported = match &request.source {
            PrepareSource::Image { .. } => capabilities.images,
            PrepareSource::Template(_) => capabilities.templates,
        };
        if !supported {
            return Err(BlazeDaemonError::UnsupportedOperation(
                "configured data plane does not support the requested source".to_string(),
            ));
        }
        let context = durable_context;
        let template_source = matches!(&request.source, PrepareSource::Template(_));
        let root_filesystem_bytes = request.root_filesystem_bytes;
        let guest_memory_bytes = request.guest_memory_bytes;
        match self.data_plane.prepare(request).await {
            Ok(prepared) => {
                self.validate_prepared_response_binding(instance, context, prepared.binding)?;
                let validation = validate_prepared(
                    capabilities,
                    context,
                    template_source,
                    root_filesystem_bytes,
                    guest_memory_bytes,
                    &prepared,
                );
                if validation.is_err() {
                    let violation = "data-plane prepare returned an invalid response";
                    return match self
                        .data_plane
                        .abort(AbortRequest {
                            binding: prepared.binding,
                        })
                        .await
                    {
                        Ok(aborted)
                            if validate_transition(
                                prepared.binding,
                                aborted.binding,
                                LeaseState::Released,
                            )
                            .is_ok() =>
                        {
                            Err(BlazeDaemonError::Internal(violation.to_string()))
                        }
                        Ok(_) => Err(self.retain_unresolved_prepare(
                            instance,
                            format!(
                                "{violation}; provider compensation returned an invalid transition"
                            ),
                        )),
                        Err(error) => Err(self.retain_unresolved_prepare(
                            instance,
                            format!("{violation}; provider compensation failed: {error}"),
                        )),
                    };
                }
                Ok(prepared)
            }
            Err(original) => {
                let observed = match self.data_plane.inspect(InspectRequest { context }).await {
                    Ok(observed) => observed,
                    Err(ProviderError::NotFound) => {
                        return Err(BlazeDaemonError::DataPlane(original));
                    }
                    Err(error) => {
                        return Err(self.retain_unresolved_prepare(
                            instance,
                            format!(
                                "data-plane preparation failed with {original}; inspection failed: {error}"
                            ),
                        ));
                    }
                };
                self.validate_prepared_response_binding(instance, context, observed.binding)?;
                let aborted = match self
                    .data_plane
                    .abort(AbortRequest {
                        binding: observed.binding,
                    })
                    .await
                {
                    Ok(aborted) => aborted,
                    Err(error) => {
                        return Err(self.retain_unresolved_prepare(
                            instance,
                            format!(
                                "data-plane preparation was observed but compensation failed: {error}"
                            ),
                        ));
                    }
                };
                if validate_transition(observed.binding, aborted.binding, LeaseState::Released)
                    .is_err()
                {
                    return Err(self.retain_unresolved_prepare(
                        instance,
                        "data-plane preparation compensation returned an invalid transition"
                            .to_string(),
                    ));
                }
                Err(BlazeDaemonError::DataPlane(original))
            }
        }
    }

    pub(super) fn validate_prepared_response_binding(
        &self,
        instance: &SandboxInstance,
        context: RequestContext,
        binding: LeaseBinding,
    ) -> Result<()> {
        if validate_prepared_binding(context, binding).is_ok()
            && instance
                .operation
                .as_ref()
                .and_then(|operation| operation.provider_operation)
                .is_some_and(|pending| {
                    pending.kind == PendingProviderOperationKind::PrepareLease
                        && pending.provider_instance_id == binding.provider_instance_id
                        && RequestContext::from(pending.context) == context
                })
        {
            return Ok(());
        }
        let mut retained = instance.clone();
        if let Err(error) = retained.retain_unresolved_provider_prepare() {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("unsafe provider preparation cannot be recorded: {error}"),
            ));
        }
        Err(self.retain_unresolved_provider_operation(
            &retained,
            "provider preparation returned unsafe ownership; operator resolution is required"
                .to_string(),
        ))
    }

    fn retain_unresolved_prepare(
        &self,
        instance: &SandboxInstance,
        message: String,
    ) -> BlazeDaemonError {
        self.retain_unresolved_provider_operation(instance, message)
    }

    fn retain_unresolved_provider_operation(
        &self,
        instance: &SandboxInstance,
        message: String,
    ) -> BlazeDaemonError {
        let recovery = self.mark_instance_recovery(instance.clone()).err();
        BlazeDaemonError::RecoveryRequired(format!(
            "{message}; provider write-ahead identity retained{}",
            recovery
                .map(|error| format!("; recovery state persistence failed: {error}"))
                .unwrap_or_default()
        ))
    }

    /// Execute one provider transition behind a durable before-image.
    pub(super) async fn transition_data_plane(
        &self,
        instance: &mut SandboxInstance,
        lease_slot: ProviderLeaseSlot,
        kind: ProviderTransitionKind,
        public_transition: Option<PublicTransitionRef>,
        backend_process: Option<BackendProcessIdentity>,
    ) -> Result<LeaseBinding> {
        let before = match lease_slot {
            ProviderLeaseSlot::Active => instance.data_plane_lease,
            ProviderLeaseSlot::Replacement => instance.replacement_data_plane_lease,
        }
        .ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} has no {:?} lease for provider {kind:?}",
                instance.id, lease_slot
            ))
        })?;
        let target_state = match kind {
            ProviderTransitionKind::Commit => DataPlaneLeaseState::Committed,
            ProviderTransitionKind::Finalize | ProviderTransitionKind::Adopt => {
                DataPlaneLeaseState::Finalized
            }
            ProviderTransitionKind::Abort | ProviderTransitionKind::Release => {
                DataPlaneLeaseState::Released
            }
            ProviderTransitionKind::Stop => DataPlaneLeaseState::Stopped,
        };
        let pending = PendingProviderTransitionRecord {
            kind,
            lease_slot,
            before,
            target_state,
            public_transition: public_transition.map(|public| ProviderPublicTransitionRecord {
                instance_id: public.instance_id,
                operation_id: public.operation_id,
            }),
            backend_process,
        };
        instance.begin_provider_transition(pending)?;
        if let Err(error) = self.persist_and_retain(instance.clone()) {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "provider {kind:?} intent could not be persisted: {error}; provider was not called"
            )));
        }
        self.settle_pending_provider_transition(instance).await
    }

    pub(super) async fn settle_pending_provider_transition(
        &self,
        instance: &mut SandboxInstance,
    ) -> Result<LeaseBinding> {
        instance.validate_provider_transition().map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "provider transition WAL is inconsistent: {error}"
            ))
        })?;
        let pending = instance.provider_transition.ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(
                "provider transition settlement has no durable WAL".to_string(),
            )
        })?;
        let before = LeaseBinding::from_record(instance.id, pending.before);
        if before.provider_instance_id != self.data_plane.descriptor().provider_instance_id {
            return Err(BlazeDaemonError::RecoveryRequired(
                "provider transition belongs to another provider instance; WAL retained"
                    .to_string(),
            ));
        }
        let target = LeaseState::from(pending.target_state);

        let observed = match self
            .data_plane
            .inspect(InspectRequest {
                context: before.context,
            })
            .await
        {
            Ok(observed) => observed.binding,
            Err(ProviderError::NotFound) if target == LeaseState::Released => {
                let released = provider_transition_target(before, target)?;
                return self.accept_provider_transition(instance, pending, released);
            }
            Err(error) => {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "provider {:?} inspection failed: {error}; WAL retained",
                    pending.kind
                )));
            }
        };
        if validate_transition(before, observed, target).is_ok() {
            return self.accept_provider_transition(instance, pending, observed);
        }
        if observed != before {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "provider {:?} observed neither its before-image nor exact successor; WAL retained",
                pending.kind
            )));
        }

        let result = self.invoke_provider_transition(pending, before).await;
        match result {
            Ok(next) if validate_transition(before, next, target).is_ok() => {
                self.accept_provider_transition(instance, pending, next)
            }
            Ok(_) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "provider {:?} returned an invalid transition; WAL retained",
                pending.kind
            ))),
            Err(original) => {
                let after = self
                    .data_plane
                    .inspect(InspectRequest {
                        context: before.context,
                    })
                    .await;
                match after {
                    Ok(after) if validate_transition(before, after.binding, target).is_ok() => {
                        self.accept_provider_transition(instance, pending, after.binding)
                    }
                    Err(ProviderError::NotFound) if target == LeaseState::Released => {
                        let released = provider_transition_target(before, target)?;
                        self.accept_provider_transition(instance, pending, released)
                    }
                    Ok(after) if after.binding == before => {
                        let mut cleared = instance.clone();
                        cleared.finish_provider_transition();
                        self.commit_instance_update(instance, cleared)?;
                        Err(BlazeDaemonError::DataPlane(original))
                    }
                    Ok(_) => Err(BlazeDaemonError::RecoveryRequired(format!(
                        "provider {:?} failed with {original} and advanced to an unsafe state; WAL retained",
                        pending.kind
                    ))),
                    Err(error) => Err(BlazeDaemonError::RecoveryRequired(format!(
                        "provider {:?} failed with {original}; outcome inspection failed with {error}; WAL retained",
                        pending.kind
                    ))),
                }
            }
        }
    }

    async fn invoke_provider_transition(
        &self,
        pending: PendingProviderTransitionRecord,
        before: LeaseBinding,
    ) -> std::result::Result<LeaseBinding, ProviderError> {
        match pending.kind {
            ProviderTransitionKind::Commit => self
                .data_plane
                .commit(CommitRequest { binding: before })
                .await
                .map(|result| result.binding),
            ProviderTransitionKind::Finalize => {
                let public = pending
                    .public_transition
                    .ok_or(ProviderError::InvalidResponse)?;
                self.data_plane
                    .finalize(FinalizeRequest {
                        binding: before,
                        public_transition: PublicTransitionRef {
                            instance_id: public.instance_id,
                            operation_id: public.operation_id,
                        },
                    })
                    .await
                    .map(|result| result.binding)
            }
            ProviderTransitionKind::Abort => self
                .data_plane
                .abort(AbortRequest { binding: before })
                .await
                .map(|result| result.binding),
            ProviderTransitionKind::Stop => self
                .data_plane
                .stop(StopRequest { binding: before })
                .await
                .map(|result| result.binding),
            ProviderTransitionKind::Release => self
                .data_plane
                .release(ReleaseRequest { binding: before })
                .await
                .map(|result| result.binding),
            ProviderTransitionKind::Adopt => {
                let inventory = self
                    .data_plane
                    .inventory()
                    .ok_or(ProviderError::Unsupported)?;
                let backend_process = pending
                    .backend_process
                    .ok_or(ProviderError::InvalidResponse)?;
                inventory
                    .reconcile(ReconcileRequest {
                        expected: Some(before),
                        observed: before,
                        action: ReconcileAction::Adopt { backend_process },
                    })
                    .await
                    .map(|result| result.binding)
            }
        }
    }

    fn accept_provider_transition(
        &self,
        instance: &mut SandboxInstance,
        pending: PendingProviderTransitionRecord,
        next: LeaseBinding,
    ) -> Result<LeaseBinding> {
        let mut settled = instance.clone();
        let record = next.to_record(
            pending.before.root_filesystem_bytes,
            pending.before.guest_memory_bytes,
        );
        match pending.lease_slot {
            ProviderLeaseSlot::Active => settled.data_plane_lease = Some(record),
            ProviderLeaseSlot::Replacement => {
                settled.replacement_data_plane_lease = Some(record);
            }
        }
        settled.finish_provider_transition();
        self.commit_instance_update(instance, settled)?;
        if pending.lease_slot == ProviderLeaseSlot::Active {
            self.retain_data_plane_lease(instance.id, next)?;
        }
        Ok(next)
    }

    fn accept_prepared_data_plane_binding(
        &self,
        instance: &mut SandboxInstance,
        binding: LeaseBinding,
        extents: (u64, u64),
    ) -> Result<()> {
        let pending = instance
            .operation
            .as_ref()
            .and_then(|operation| operation.provider_operation)
            .ok_or_else(|| {
                BlazeDaemonError::RecoveryRequired(format!(
                    "sandbox {} has no provider write-ahead identity for the prepared lease",
                    instance.id
                ))
            })?;
        let context = RequestContext::from(pending.context);
        if pending.kind != PendingProviderOperationKind::PrepareLease
            || pending.provider_instance_id != binding.provider_instance_id
            || context != binding.context
            || pending.root_filesystem_bytes != extents.0
            || pending.guest_memory_bytes != extents.1
            || validate_prepared_binding(context, binding).is_err()
        {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} prepared lease does not match its provider write-ahead identity",
                instance.id
            )));
        }

        let mut accepted = instance.clone();
        accepted.data_plane_lease = Some(binding.to_record(extents.0, extents.1));
        accepted.finish_provider_operation();
        self.commit_instance_update(instance, accepted)?;
        self.retain_data_plane_lease(instance.id, binding)
    }

    pub(super) fn accept_prepared_replacement_data_plane_binding(
        &self,
        instance: &mut SandboxInstance,
        binding: LeaseBinding,
        extents: (u64, u64),
    ) -> Result<()> {
        let pending = instance
            .operation
            .as_ref()
            .and_then(|operation| operation.provider_operation)
            .ok_or_else(|| {
                BlazeDaemonError::RecoveryRequired(format!(
                    "sandbox {} has no provider write-ahead identity for the replacement lease",
                    instance.id
                ))
            })?;
        let context = RequestContext::from(pending.context);
        if pending.kind != PendingProviderOperationKind::PrepareLease
            || pending.provider_instance_id != binding.provider_instance_id
            || context != binding.context
            || pending.root_filesystem_bytes != extents.0
            || pending.guest_memory_bytes != extents.1
            || validate_prepared_binding(context, binding).is_err()
        {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} replacement lease does not match its provider write-ahead identity",
                instance.id
            )));
        }

        let mut accepted = instance.clone();
        accepted.replacement_data_plane_lease = Some(binding.to_record(extents.0, extents.1));
        accepted.finish_provider_operation();
        self.commit_instance_update(instance, accepted)
    }

    fn commit_instance_update(
        &self,
        instance: &mut SandboxInstance,
        next: SandboxInstance,
    ) -> Result<()> {
        self.state_store.persist(&next)?;
        let retained = self.retain_instance(next.clone());
        *instance = next;
        if let Some(error) = retained {
            return Err(BlazeDaemonError::RecoveryRequired(error));
        }
        Ok(())
    }

    pub(super) fn persist_data_plane_binding(
        &self,
        instance: &mut SandboxInstance,
        binding: LeaseBinding,
        extents: Option<(u64, u64)>,
    ) -> Result<()> {
        let (root_filesystem_bytes, guest_memory_bytes) = match extents {
            Some(extents) => extents,
            None => {
                let record = instance.data_plane_lease.ok_or_else(|| {
                    BlazeDaemonError::RecoveryRequired(format!(
                        "sandbox {} has no durable data-plane lease to advance",
                        instance.id
                    ))
                })?;
                (record.root_filesystem_bytes, record.guest_memory_bytes)
            }
        };
        if let Some(previous) = instance.data_plane_lease
            && (previous.provider_instance_id != binding.provider_instance_id
                || previous.lease_id != binding.context.lease_id
                || previous.request_id != binding.context.request_id
                || previous.operation_id != binding.context.operation_id
                || previous.initial_generation != binding.context.generation
                || binding.generation < previous.generation)
        {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} data-plane lease identity or generation changed unexpectedly",
                instance.id
            )));
        }
        instance.data_plane_lease =
            Some(binding.to_record(root_filesystem_bytes, guest_memory_bytes));
        self.state_store.persist(instance)?;
        if let Some(error) = self.retain_instance(instance.clone()) {
            return Err(BlazeDaemonError::RecoveryRequired(error));
        }
        self.retain_data_plane_lease(instance.id, binding)
    }

    pub(super) fn retain_data_plane_lease(&self, id: Uuid, binding: LeaseBinding) -> Result<()> {
        let mut leases = self
            .data_plane_leases
            .lock()
            .map_err(|_| poisoned("data_plane_leases"))?;
        if let Some(previous) = leases.insert(id, binding)
            && previous.context.lease_id != binding.context.lease_id
        {
            leases.insert(id, previous);
            return Err(BlazeDaemonError::Conflict(format!(
                "sandbox {id} already owns another data-plane lease"
            )));
        }
        Ok(())
    }

    fn data_plane_lease(&self, id: Uuid) -> Result<Option<LeaseBinding>> {
        Ok(self
            .data_plane_leases
            .lock()
            .map_err(|_| poisoned("data_plane_leases"))?
            .get(&id)
            .copied())
    }

    pub(super) fn remove_data_plane_lease(&self, id: Uuid) -> Result<()> {
        self.data_plane_leases
            .lock()
            .map_err(|_| poisoned("data_plane_leases"))?
            .remove(&id);
        Ok(())
    }

    /// Return all persisted sandbox metadata.
    pub fn list(&self) -> Result<Vec<SandboxInstance>> {
        Ok(self
            .instances
            .lock()
            .map_err(|_| poisoned("instances"))?
            .values()
            .cloned()
            .collect())
    }

    /// Return one persisted sandbox.
    pub fn get(&self, id: Uuid) -> Result<SandboxInstance> {
        self.instances
            .lock()
            .map_err(|_| poisoned("instances"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| BlazeDaemonError::NotFound(format!("instance {id}")))
    }

    /// Return one validated reusable-resource capacity partition.
    pub async fn provider_capacity(&self, scope: CapacityScope) -> Result<CapacitySnapshot> {
        if scope.class_digest == [0; 32] {
            return Err(BlazeDaemonError::BadRequest(
                "capacity class digest must not be zero".to_string(),
            ));
        }
        let extension = self.data_plane.capacity_control().ok_or_else(|| {
            BlazeDaemonError::UnsupportedOperation(
                "data-plane capacity management is not implemented".to_string(),
            )
        })?;
        let descriptor = self.data_plane.descriptor();
        validate_descriptor(descriptor).map_err(|_| {
            BlazeDaemonError::Internal("data-plane descriptor is incompatible".to_string())
        })?;
        let request = CapacityRequest { scope };
        let snapshot = extension.capacity(request).await?;
        validate_capacity_snapshot(descriptor, request, snapshot)
            .map_err(|_| BlazeDaemonError::DataPlane(ProviderError::InvalidResponse))?;
        Ok(snapshot)
    }

    /// Drain one exact capacity partition using an idempotent operation identity.
    pub async fn drain_provider_capacity(&self, request: DrainRequest) -> Result<DrainResult> {
        if request.scope.class_digest == [0; 32] || request.operation_id.is_nil() {
            return Err(BlazeDaemonError::BadRequest(
                "capacity drain requires nonzero class and operation identities".to_string(),
            ));
        }
        let extension = self.data_plane.capacity_control().ok_or_else(|| {
            BlazeDaemonError::UnsupportedOperation(
                "data-plane capacity management is not implemented".to_string(),
            )
        })?;
        let descriptor = self.data_plane.descriptor();
        validate_descriptor(descriptor).map_err(|_| {
            BlazeDaemonError::Internal("data-plane descriptor is incompatible".to_string())
        })?;
        let first = extension.drain(request).await;
        let result = match first {
            Err(ProviderError::OutcomeUnknown) => extension.drain(request).await?,
            result => result?,
        };
        validate_drain_result(descriptor, request, result)
            .map_err(|_| BlazeDaemonError::DataPlane(ProviderError::InvalidResponse))?;
        Ok(result)
    }

    /// Return every sandbox for which lifecycle cleanup still owns resources.
    ///
    /// Shutdown uses this snapshot to start cleanup concurrently while all
    /// mutations remain serialized by the manager's per-sandbox locks.
    pub(crate) fn owned_instance_ids(&self) -> Result<BTreeSet<Uuid>> {
        let daemon_managed_storage = self.data_plane.capabilities().daemon_managed_storage;
        let mut ids = self
            .instances
            .lock()
            .map_err(|_| poisoned("instances"))?
            .values()
            .filter(|instance| requires_automatic_cleanup(instance, daemon_managed_storage))
            .map(|instance| instance.id)
            .collect::<BTreeSet<_>>();
        ids.extend(
            self.backend_instances
                .lock()
                .map_err(|_| poisoned("backend_instances"))?
                .keys()
                .copied(),
        );
        Ok(ids)
    }

    /// Execute one command through the running sandbox guest.
    pub async fn exec(
        &self,
        id: Uuid,
        command: String,
        cwd: Option<String>,
        env: Option<HashMap<String, String>>,
        timeout_secs: u32,
    ) -> Result<GuestExecResult> {
        let _operation = self.lock_running(id).await?;
        self.guest_client(id)?
            .exec(command, cwd, env, timeout_secs)
            .await
            .map_err(BlazeDaemonError::from)
    }

    /// Read one file through the running sandbox guest.
    pub async fn read_file(&self, id: Uuid, path: String) -> Result<Vec<u8>> {
        let _operation = self.lock_running(id).await?;
        self.guest_client(id)?
            .read_file(path)
            .await
            .map_err(BlazeDaemonError::from)
    }

    /// Replace one file through the running sandbox guest.
    pub async fn write_file(&self, id: Uuid, path: String, data: &[u8]) -> Result<()> {
        let _operation = self.lock_running(id).await?;
        self.guest_client(id)?
            .write_file(path, data)
            .await
            .map_err(BlazeDaemonError::from)
    }

    /// Validate a template-backed create before any lifecycle state is written.
    ///
    /// Returns `None` for an ordinary create. For a template request it checks
    /// the policy allow-list, storage support, and catalog metadata, then
    /// confirms the published snapshot's image, backend, version, kernel
    /// command line, VM shape, and guest transport all match what the current
    /// policy would launch. The pinned executable and resolved artifacts are
    /// carried forward so the create path restores exactly what was validated.
    async fn prepare_template_create(
        &self,
        request: &CreateSandbox,
    ) -> Result<Option<TemplateCreate>> {
        let Some(name) = request.template.as_ref() else {
            return Ok(None);
        };
        if !request
            .decision
            .templates
            .iter()
            .any(|allowed| allowed == name)
        {
            return Err(BlazeDaemonError::Conflict(format!(
                "template {name} is not allowed by policy {}",
                request.decision.policy_name
            )));
        }
        let data_plane_capabilities = self.data_plane.capabilities();
        if !data_plane_capabilities.templates {
            return Err(BlazeDaemonError::UnsupportedOperation(
                "configured data plane does not support templates".to_string(),
            ));
        }

        let resolved = self.resolve_template_for_create(name.clone()).await?;
        if resolved.image_digest != request.image_digest {
            return Err(BlazeDaemonError::Conflict(format!(
                "template {name} image identity does not match the create request"
            )));
        }
        if resolved.backend != request.runtime_backend {
            return Err(BlazeDaemonError::Conflict(format!(
                "template {name} requires backend {}, but the request selected {}",
                resolved.backend, request.runtime_backend
            )));
        }

        if resolved.backend == BackendKind::Firecracker {
            let config = request
                .decision
                .backend
                .firecracker
                .as_ref()
                .cloned()
                .unwrap_or_default();
            if config.enable_vsock != resolved.expose_guest_socket
                || config.enable_network != resolved.network
            {
                return Err(BlazeDaemonError::Conflict(format!(
                    "template {name} guest transport shape does not match policy {}",
                    request.decision.policy_name
                )));
            }
            let effective_boot_args =
                crate::spawner::firecracker::effective_boot_args(&config, config.enable_network)?;
            validate_template_boot_args(
                name,
                resolved.boot_args.as_deref(),
                &effective_boot_args,
                &request.decision.policy_name,
            )?;
            let (vcpus, memory_mib) = crate::spawner::firecracker::effective_vm_shape(
                &config,
                request.decision.vm.as_ref(),
            )?;
            if resolved.vcpus != Some(vcpus) || resolved.memory_mib != Some(memory_mib) {
                return Err(BlazeDaemonError::Conflict(format!(
                    "template {name} VM shape does not match policy {}",
                    request.decision.policy_name
                )));
            }
        } else {
            if resolved.expose_guest_socket {
                return Err(BlazeDaemonError::UnsupportedOperation(format!(
                    "template {name} requests guest transport for unsupported backend {}",
                    resolved.backend
                )));
            }
            if resolved.network {
                return Err(BlazeDaemonError::UnsupportedOperation(format!(
                    "template {name} requests networking for unsupported backend {}",
                    resolved.backend
                )));
            }
        }

        let spawner = self.spawner(resolved.backend).ok_or_else(|| {
            BlazeDaemonError::UnsupportedOperation(format!(
                "template {name} has no restore adapter for {}",
                resolved.backend
            ))
        })?;
        // A backend that runs no separate program of its own carries no
        // configured path; pin one only when a real executable is configured.
        let executable = if request.binary_path.as_os_str().is_empty() {
            None
        } else {
            Some(Arc::new(PinnedExecutable::open(&request.binary_path)?))
        };
        let capability = spawner
            .restore_capability(executable.as_deref())
            .await?
            .ok_or_else(|| {
                BlazeDaemonError::UnsupportedOperation(format!(
                    "template {name} backend {} does not support restore",
                    resolved.backend
                ))
            })?;
        if capability.backend != resolved.backend
            || capability.version != resolved.backend_version
            || capability.snapshot_kind != resolved.snapshot_kind
        {
            return Err(BlazeDaemonError::UnsupportedOperation(format!(
                "template {name} is incompatible with the current restore adapter"
            )));
        }
        if data_plane_capabilities.opened_template_restore_resources
            && !capability.consumes_typed_opened_attachments
        {
            return Err(BlazeDaemonError::UnsupportedOperation(format!(
                "template {name} may use typed opened restore attachments, but backend {} cannot consume them",
                resolved.backend
            )));
        }

        Ok(Some(TemplateCreate {
            resolved,
            spawner,
            executable,
            record_console_log: request
                .decision
                .backend
                .firecracker
                .as_ref()
                .is_some_and(|config| config.serial_log),
        }))
    }

    /// Create a sandbox from a fresh runtime allocation or a published template.
    pub async fn create(&self, request: CreateSandbox) -> Result<CreateSandboxResult> {
        if request.template.is_none() && !self.data_plane.capabilities().images {
            return Err(BlazeDaemonError::UnsupportedOperation(
                "configured data plane does not support ordinary images".to_string(),
            ));
        }
        let template = self.prepare_template_create(&request).await?;
        let mut instance = SandboxInstance::new(
            request.runtime_backend,
            request.decision.workload_class,
            request.image_digest.clone(),
            request.decision.policy_name.clone(),
        );
        instance.template = template
            .as_ref()
            .map(|template| template.resolved.name.clone());
        let operation_lock = self.operation_lock(instance.id);
        let _operation = operation_lock.lock().await;
        instance.transition(SandboxState::Creating)?;
        instance.begin_operation(OperationKind::Create);
        let context = RequestContext {
            instance_id: instance.id,
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
        };
        let (root_filesystem_bytes, guest_memory_bytes) = template
            .as_ref()
            .map(|template| (template.resolved.rootfs_size, template.resolved.memory_size))
            .unwrap_or((self.rootfs_size, self.mem_size));
        instance.begin_provider_operation(PendingProviderOperationRecord {
            provider_instance_id: self.data_plane.descriptor().provider_instance_id,
            context: context.into(),
            generation_before_call: 0,
            root_filesystem_bytes,
            guest_memory_bytes,
            kind: PendingProviderOperationKind::PrepareLease,
        })?;

        // Publish the complete provider identity and create intent before allocation.
        if let Err(error) = self.state_store.persist(&instance) {
            match self.state_store.has_run_dir_residual(instance.id) {
                Ok(true) => {}
                Ok(false) => return Err(error),
                Err(residual_error) => {
                    return Err(BlazeDaemonError::RecoveryRequired(format!(
                        "create {}: initial state publication failed: {error}; could not inspect \
                         publication residual: {residual_error}",
                        instance.id
                    )));
                }
            }
            let rollback_errors = self.commit_create_rollback(&mut instance);
            if rollback_errors.is_empty() {
                return Err(error);
            }
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "create {}: initial state publication failed: {error}; {}",
                instance.id,
                rollback_errors.join("; ")
            )));
        }
        if let Some(error) = self.retain_instance(instance.clone()) {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "create {}: {error}",
                instance.id
            )));
        }

        let (prepare_request, template_plan, template) = match template {
            Some(TemplateCreate {
                resolved,
                spawner,
                executable,
                record_console_log,
            }) => {
                let ResolvedTemplate {
                    image_digest,
                    backend_version,
                    snapshot_kind,
                    expose_guest_socket,
                    network,
                    rootfs_size,
                    memory_size,
                    storage: source,
                    ..
                } = resolved;
                (
                    PrepareRequest {
                        context,
                        source: PrepareSource::Template(TemplateSource {
                            image_digest,
                            storage: source,
                        }),
                        root_filesystem_bytes: rootfs_size,
                        guest_memory_bytes: memory_size,
                    },
                    Some(TemplateRestorePlan {
                        expected_version: backend_version,
                        snapshot_kind,
                        expose_guest_socket,
                        // A new sandbox never inherits the source's network
                        // slot, so a networked template requests a fresh one.
                        preserve_network: network,
                        record_console_log,
                    }),
                    Some((spawner, executable)),
                )
            }
            None => (
                PrepareRequest {
                    context,
                    source: PrepareSource::Image {
                        image_digest: request.image_digest.clone(),
                    },
                    root_filesystem_bytes: self.rootfs_size,
                    guest_memory_bytes: self.mem_size,
                },
                None,
                None,
            ),
        };
        let lease_extents = (
            prepare_request.root_filesystem_bytes,
            prepare_request.guest_memory_bytes,
        );
        let prepared = match self.prepare_data_plane(&instance, prepare_request).await {
            Ok(prepared) => prepared,
            Err(error @ BlazeDaemonError::RecoveryRequired(_)) => return Err(error),
            Err(error) => {
                match self.data_plane_lease(instance.id) {
                    Ok(Some(binding)) => {
                        instance.data_plane_lease =
                            Some(binding.to_record(lease_extents.0, lease_extents.1));
                        let recovery = self.mark_instance_recovery(instance).err();
                        return Err(BlazeDaemonError::RecoveryRequired(format!(
                            "{error}; prepared provider ownership was retained{}",
                            recovery
                                .map(|error| format!(
                                    "; recovery state persistence failed: {error}"
                                ))
                                .unwrap_or_default()
                        )));
                    }
                    Ok(None) => {}
                    Err(retention_error) => {
                        let recovery = self.mark_instance_recovery(instance).err();
                        return Err(BlazeDaemonError::RecoveryRequired(format!(
                            "{error}; provider lease retention is unreadable: {retention_error}{}",
                            recovery
                                .map(|error| format!(
                                    "; recovery state persistence failed: {error}"
                                ))
                                .unwrap_or_default()
                        )));
                    }
                }
                let errors = self.commit_create_rollback(&mut instance);
                return if errors.is_empty() {
                    Err(error)
                } else {
                    Err(BlazeDaemonError::RecoveryRequired(format!(
                        "{error}; {}",
                        errors.join("; ")
                    )))
                };
            }
        };
        let binding = prepared.binding;
        if let Err(error) =
            self.accept_prepared_data_plane_binding(&mut instance, binding, lease_extents)
        {
            return Err(self
                .cleanup_failed_create(&mut instance, binding, None, false, error)
                .await);
        }
        let (resources, template_restore) = match (prepared.resources, template_plan) {
            (
                PreparedResources::PathBacked {
                    storage,
                    restore_payload_dir,
                },
                plan,
            ) => {
                let restore = match (restore_payload_dir, plan) {
                    (Some(payload_dir), Some(plan)) => Some(TemplateRestore {
                        payload_dir,
                        expected_version: plan.expected_version,
                        snapshot_kind: plan.snapshot_kind,
                        expose_guest_socket: plan.expose_guest_socket,
                        preserve_network: plan.preserve_network,
                        record_console_log: plan.record_console_log,
                    }),
                    (None, None) => None,
                    _ => {
                        return Err(self
                            .cleanup_failed_create(
                                &mut instance,
                                binding,
                                None,
                                false,
                                BlazeDaemonError::Internal(
                                    "data-plane restore payload does not match the create source"
                                        .to_string(),
                                ),
                            )
                            .await);
                    }
                };
                (
                    PreparedCreateResources {
                        binding,
                        storage: Some(storage),
                        provider_attachments: None,
                    },
                    restore,
                )
            }
            (
                PreparedResources::OpenedRestore {
                    restore_payload_dir,
                    attachments,
                },
                Some(plan),
            ) => {
                let provider_attachments = provider_restore_attachments(binding, attachments);
                (
                    PreparedCreateResources {
                        binding,
                        storage: None,
                        provider_attachments: Some(provider_attachments),
                    },
                    Some(TemplateRestore {
                        payload_dir: restore_payload_dir,
                        expected_version: plan.expected_version,
                        snapshot_kind: plan.snapshot_kind,
                        expose_guest_socket: plan.expose_guest_socket,
                        preserve_network: plan.preserve_network,
                        record_console_log: plan.record_console_log,
                    }),
                )
            }
            (PreparedResources::OpenedRestore { .. }, None) => {
                return Err(self
                    .cleanup_failed_create(
                        &mut instance,
                        binding,
                        None,
                        false,
                        BlazeDaemonError::UnsupportedOperation(
                            "ordinary image creation requires path-backed provider resources"
                                .to_string(),
                        ),
                    )
                    .await);
            }
            (
                PreparedResources::CheckpointRestore { .. }
                | PreparedResources::SuspensionRestore { .. },
                _,
            ) => {
                return Err(self
                    .cleanup_failed_create(
                        &mut instance,
                        binding,
                        None,
                        false,
                        BlazeDaemonError::Internal(
                            "data-plane provider returned lifecycle restore resources for creation"
                                .to_string(),
                        ),
                    )
                    .await);
            }
        };
        crate::failpoint::pause("create-after-storage-acquire").await;

        let work_dir = match self.state_store.run_dir(instance.id) {
            Ok(work_dir) => work_dir,
            Err(error) => {
                return Err(self
                    .cleanup_failed_create(&mut instance, resources.binding, None, false, error)
                    .await);
            }
        };
        let mut lease_binding = resources.binding;
        let storage = resources.storage;
        let provider_attachments = resources.provider_attachments;
        let (spawner, template_executable) = match template {
            Some((spawner, executable)) => (Some(spawner), executable),
            None => (self.spawners.get(self.active_backend), None),
        };
        let spawner = match spawner {
            Some(spawner) => spawner,
            None => {
                return Err(self
                    .cleanup_failed_create(
                        &mut instance,
                        lease_binding,
                        None,
                        false,
                        BlazeDaemonError::Internal(format!(
                            "active backend {} has no registered spawner",
                            self.active_backend
                        )),
                    )
                    .await);
            }
        };
        if let Err(error) = spawner.prepare_spawn(&work_dir).await {
            return Err(self
                .cleanup_failed_create(&mut instance, lease_binding, None, false, error.into())
                .await);
        }

        instance.backend_ownership = BackendOwnership::Starting;
        if let Err(error) = self.state_store.persist(&instance) {
            instance.backend_ownership = BackendOwnership::NotStarted;
            return Err(self
                .cleanup_failed_create(&mut instance, lease_binding, None, false, error)
                .await);
        }
        if let Some(error) = self.retain_instance(instance.clone()) {
            instance.backend_ownership = BackendOwnership::NotStarted;
            return Err(self
                .cleanup_failed_create(
                    &mut instance,
                    lease_binding,
                    None,
                    false,
                    BlazeDaemonError::Internal(error),
                )
                .await);
        }

        let template_backed = template_restore.is_some();
        let spawn = if let Some(template) = template_restore {
            let mut restore_request = match BackendRestoreRequest::new(
                RestoreRequest {
                    instance_id: instance.id,
                    binary_path: request.binary_path,
                    storage: storage.clone(),
                    payload_dir: template.payload_dir,
                    checkpoint_backend: instance.backend,
                    expected_version: template.expected_version,
                    snapshot_kind: template.snapshot_kind,
                    expose_guest_socket: template.expose_guest_socket,
                    preserve_network: template.preserve_network,
                    record_console_log: template.record_console_log,
                    // One published capture restores into many new sandboxes.
                    snapshot_from_other_sandbox: true,
                },
                work_dir.clone(),
                template_executable,
            ) {
                Ok(request) => request,
                Err(error) => {
                    instance.backend_ownership = BackendOwnership::NotStarted;
                    return Err(self
                        .cleanup_failed_create(
                            &mut instance,
                            lease_binding,
                            None,
                            false,
                            error.into(),
                        )
                        .await);
                }
            };
            restore_request.provider_attachments = provider_attachments;
            match crate::failpoint::backend("create-spawn") {
                Ok(()) => restore_with_runtime_directory(spawner.as_ref(), restore_request).await,
                Err(error) => Err(crate::spawner::SpawnFailure::clean(error)),
            }
        } else {
            let Some(storage) = storage.clone() else {
                instance.backend_ownership = BackendOwnership::NotStarted;
                return Err(self
                    .cleanup_failed_create(
                        &mut instance,
                        lease_binding,
                        None,
                        false,
                        BlazeDaemonError::UnsupportedOperation(
                            "ordinary image creation requires path-backed provider resources"
                                .to_string(),
                        ),
                    )
                    .await);
            };
            let backend_request = match BackendSpawnRequest::new(
                SpawnRequest {
                    instance_id: instance.id,
                    binary_path: request.binary_path,
                    storage: Some(storage),
                    backend: request.decision.backend,
                    vm: request.decision.vm,
                },
                work_dir.clone(),
            ) {
                Ok(request) => request,
                Err(error) => {
                    instance.backend_ownership = BackendOwnership::NotStarted;
                    return Err(self
                        .cleanup_failed_create(
                            &mut instance,
                            lease_binding,
                            None,
                            false,
                            error.into(),
                        )
                        .await);
                }
            };
            match crate::failpoint::backend("create-spawn") {
                Ok(()) => spawn_with_runtime_directory(spawner.as_ref(), backend_request).await,
                Err(error) => Err(crate::spawner::SpawnFailure::clean(error)),
            }
        };
        let (actual_backend, backend_runtime) = match spawn {
            Ok(backend_instance) => {
                instance.backend_ownership = BackendOwnership::Running;
                // A restore reloads a captured identity; refuse to adopt a
                // backend owner whose identity diverges from durable state.
                if template_backed
                    && (backend_instance.instance_id() != instance.id
                        || backend_instance.backend() != instance.backend)
                {
                    return Err(self
                        .cleanup_failed_create(
                            &mut instance,
                            lease_binding,
                            Some(backend_instance),
                            false,
                            BlazeDaemonError::Internal(
                                "restored backend owner identity does not match durable state"
                                    .to_string(),
                            ),
                        )
                        .await);
                }
                let actual_backend = backend_instance.backend();
                let backend_runtime = backend_instance.runtime_record();
                if let Err(error) = self
                    .wait_for_guest_ready(&backend_instance, "create-guest-ready")
                    .await
                {
                    return Err(self
                        .cleanup_failed_create(
                            &mut instance,
                            lease_binding,
                            Some(backend_instance),
                            false,
                            error.into(),
                        )
                        .await);
                }
                let mut backend_instance = Some(backend_instance);
                let registered = match self.backend_instances.lock() {
                    Ok(mut instances) => {
                        instances.insert(
                            instance.id,
                            backend_instance
                                .take()
                                .expect("backend instance is present"),
                        );
                        true
                    }
                    Err(_) => false,
                };
                if !registered {
                    return Err(self
                        .cleanup_failed_create(
                            &mut instance,
                            lease_binding,
                            backend_instance,
                            false,
                            BlazeDaemonError::Internal(
                                "backend_instances lock poisoned".to_string(),
                            ),
                        )
                        .await);
                }
                (actual_backend, backend_runtime)
            }
            Err(error) => {
                let (source, backend) = error.into_parts();
                instance.backend_ownership = if backend.is_some() {
                    BackendOwnership::Running
                } else {
                    BackendOwnership::Stopped
                };
                return Err(self
                    .cleanup_failed_create(
                        &mut instance,
                        lease_binding,
                        backend,
                        false,
                        source.into(),
                    )
                    .await);
            }
        };

        instance.backend_runtime = Some(backend_runtime);
        if let Err(error) = self.persist_data_plane_binding(&mut instance, lease_binding, None) {
            return Err(self
                .cleanup_failed_create(&mut instance, lease_binding, None, true, error)
                .await);
        }

        lease_binding = match self
            .transition_data_plane(
                &mut instance,
                ProviderLeaseSlot::Active,
                ProviderTransitionKind::Commit,
                None,
                None,
            )
            .await
        {
            Ok(binding) => binding,
            Err(error) => {
                if instance.provider_transition.is_some() {
                    return Err(error);
                }
                return Err(self
                    .cleanup_failed_create(&mut instance, lease_binding, None, true, error)
                    .await);
            }
        };
        if let Err(error) = instance.transition(SandboxState::Running) {
            return Err(self
                .cleanup_failed_create(&mut instance, lease_binding, None, true, error.into())
                .await);
        }
        instance.finish_operation();
        if let Err(error) = crate::failpoint::state("create-state-commit")
            .and_then(|_| self.state_store.persist(&instance))
        {
            return Err(self
                .cleanup_failed_create(&mut instance, lease_binding, None, true, error)
                .await);
        }
        if let Some(error) = self.retain_instance(instance.clone()) {
            return Err(self
                .cleanup_failed_create(
                    &mut instance,
                    lease_binding,
                    None,
                    true,
                    BlazeDaemonError::Internal(error),
                )
                .await);
        }
        let public_instance_id = instance.id;
        let finalized = match self
            .transition_data_plane(
                &mut instance,
                ProviderLeaseSlot::Active,
                ProviderTransitionKind::Finalize,
                Some(PublicTransitionRef {
                    instance_id: public_instance_id,
                    operation_id: lease_binding.context.operation_id,
                }),
                None,
            )
            .await
        {
            Ok(finalized) => finalized,
            Err(error) => {
                let _ = self.mark_recovery(instance.id);
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "create {}: public state is durable but data-plane finalize failed: {error}",
                    instance.id
                )));
            }
        };
        debug_assert_eq!(finalized.state, LeaseState::Finalized);
        self.metrics.inc(&self.metrics.instances_created);
        Ok(CreateSandboxResult {
            instance,
            selected_backend: actual_backend,
        })
    }

    /// Idempotently destroy one sandbox and its owned runtime resources.
    ///
    /// The supervised task retains per-sandbox serialization after a caller
    /// disconnects, so blocking filesystem cleanup cannot race a retry.
    pub async fn destroy(self: &Arc<Self>, id: Uuid) -> Result<bool> {
        let manager = Arc::clone(self);
        crate::failpoint::spawn(async move {
            let operation = manager.operation_lock(id).lock_owned().await;
            let result = manager.destroy_locked(id).await;
            drop(operation);
            result
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::Internal(format!("destroy supervisor failed: {error}"))
        })?
    }

    async fn destroy_locked(&self, id: Uuid) -> Result<bool> {
        let mut original = self.get(id)?;
        if original.state == SandboxState::Destroyed {
            return Ok(false);
        }
        self.ensure_provider_ownership_matches(&original)?;
        let checkpoint_metadata = self.checkpoint_ownership_preflight(id).await?;
        let resolved_pending_create_prepare =
            original.operation.as_ref().is_some_and(|operation| {
                operation.kind == OperationKind::Create
                    && operation.provider_operation.is_some_and(|pending| {
                        pending.kind == PendingProviderOperationKind::PrepareLease
                    })
            });
        self.consume_pending_provider_operation(&mut original)
            .await?;
        self.ensure_provider_ownership_matches(&original)?;

        if original.operation.as_ref().map(|operation| operation.kind)
            != Some(OperationKind::Destroy)
        {
            original.begin_operation(OperationKind::Destroy);
        }
        if let Err(error) = crate::failpoint::state("destroy-intent-state-commit")
            .and_then(|_| self.state_store.persist(&original))
        {
            let _ = self.mark_recovery(id);
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: intent persistence failed: {error}; resources retained"
            )));
        }
        if let Some(error) = self.retain_instance(original.clone()) {
            let _ = self.mark_recovery(id);
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: {error}; resources retained"
            )));
        }
        let mut data_plane_binding = self.data_plane_lease(id)?;
        let replacement_binding = original
            .replacement_data_plane_lease
            .map(|record| LeaseBinding::from_record(id, record));
        let mut data_plane_released = (resolved_pending_create_prepare
            && original.data_plane_lease.is_none())
            || ((original.provider_suspension.is_some()
                || !original.pending_provider_suspension_retirements.is_empty())
                && original.data_plane_lease.is_none()
                && original.replacement_data_plane_lease.is_none());

        let backend = self
            .backend_instances
            .lock()
            .map_err(|_| poisoned("backend_instances"))?
            .get(&id)
            .cloned();
        let stop_result = match crate::failpoint::backend("destroy-kill") {
            Ok(()) => {
                if let Some(backend) = backend.as_ref() {
                    backend.kill().await
                } else if matches!(
                    original.backend_ownership,
                    BackendOwnership::NotStarted | BackendOwnership::Stopped
                ) {
                    Ok(())
                } else {
                    match self.spawners.get(original.backend) {
                        Some(spawner) => match self.state_store.run_dir(id) {
                            Ok(run_dir) => spawner.cleanup_orphan(id, &run_dir).await,
                            Err(error) => Err(BlazeError::BackendError {
                                msg: format!(
                                    "open owned run directory for persisted instance {id}: {error}"
                                ),
                            }),
                        },
                        None => Err(BlazeError::BackendError {
                            msg: format!(
                                "no recovery spawner registered for persisted backend {}",
                                original.backend
                            ),
                        }),
                    }
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = stop_result {
            let recovery = self.mark_recovery(id).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: backend termination failed: {error}; owner and storage retained{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        original.backend_ownership = BackendOwnership::Stopped;

        if let Some(binding) = replacement_binding {
            match binding.state {
                LeaseState::Prepared | LeaseState::Committed => {
                    self.transition_data_plane(
                        &mut original,
                        ProviderLeaseSlot::Replacement,
                        ProviderTransitionKind::Abort,
                        None,
                        None,
                    )
                    .await?;
                }
                LeaseState::Finalized => {
                    self.transition_data_plane(
                        &mut original,
                        ProviderLeaseSlot::Replacement,
                        ProviderTransitionKind::Stop,
                        None,
                        None,
                    )
                    .await?;
                    self.transition_data_plane(
                        &mut original,
                        ProviderLeaseSlot::Replacement,
                        ProviderTransitionKind::Release,
                        None,
                        None,
                    )
                    .await?;
                }
                LeaseState::Stopped => {
                    self.transition_data_plane(
                        &mut original,
                        ProviderLeaseSlot::Replacement,
                        ProviderTransitionKind::Release,
                        None,
                        None,
                    )
                    .await?;
                }
                LeaseState::Released => {}
                LeaseState::Quarantined => {
                    return Err(BlazeDaemonError::RecoveryRequired(format!(
                        "destroy {id}: quarantined replacement resources require operator resolution"
                    )));
                }
            }
            original.replacement_data_plane_lease = None;
            self.persist_and_retain(original.clone())?;
            data_plane_released |= (original.provider_suspension.is_some()
                || !original.pending_provider_suspension_retirements.is_empty())
                && original.data_plane_lease.is_none()
                && original.replacement_data_plane_lease.is_none();
        }

        if let Some(binding) = data_plane_binding {
            match binding.state {
                LeaseState::Finalized => {
                    let stopped = self
                        .transition_data_plane(
                            &mut original,
                            ProviderLeaseSlot::Active,
                            ProviderTransitionKind::Stop,
                            None,
                            None,
                        )
                        .await?;
                    data_plane_binding = Some(stopped);
                }
                LeaseState::Prepared | LeaseState::Committed => {
                    let aborted = self
                        .transition_data_plane(
                            &mut original,
                            ProviderLeaseSlot::Active,
                            ProviderTransitionKind::Abort,
                            None,
                            None,
                        )
                        .await?;
                    data_plane_binding = Some(aborted);
                    data_plane_released = true;
                }
                LeaseState::Stopped => {}
                LeaseState::Released => data_plane_released = true,
                LeaseState::Quarantined => {
                    return Err(BlazeDaemonError::RecoveryRequired(format!(
                        "destroy {id}: quarantined data-plane resources require operator resolution"
                    )));
                }
            }
        }

        if let Err(error) = crate::failpoint::state("destroy-stop-state-commit")
            .and_then(|_| self.state_store.persist(&original))
        {
            let recovery = self.mark_instance_recovery(original.clone()).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: backend stopped but stop state persistence failed: {error}; \
                 storage retained{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        if let Some(error) = self.retain_instance(original.clone()) {
            let _ = self.mark_recovery(id);
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: backend stopped but lifecycle retention failed: {error}; \
                 storage retained"
            )));
        }

        for record in checkpoint_metadata
            .into_iter()
            .filter_map(|metadata| metadata.provider_checkpoint)
        {
            if !original
                .pending_provider_retirements
                .iter()
                .any(|pending| pending == &record)
            {
                original.pending_provider_retirements.push(record);
            }
        }
        if !original.pending_provider_retirements.is_empty() {
            if self.data_plane.checkpoints().is_none() {
                let recovery = self.mark_instance_recovery(original).err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "destroy {id}: provider checkpoint retirement is unavailable{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )));
            }
            self.persist_and_retain(original.clone())?;
        }

        let checkpoints = self.checkpoints.clone();
        let checkpoint_cleanup = crate::failpoint::spawn_blocking(move || {
            crate::failpoint::pause_blocking("checkpoint-before-store-remove");
            checkpoints.remove_sandbox(id)
        })
        .await;
        let checkpoint_cleanup_error = match checkpoint_cleanup {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => Some(format!("blocking task failed: {error}")),
        };
        if let Some(error) = checkpoint_cleanup_error {
            let recovery = self.mark_recovery(id).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: backend stopped but checkpoint cleanup failed: {error}; \
                 storage retained{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }

        for record in original.pending_provider_retirements.clone() {
            let checkpoint = ProviderCheckpointRef::from_record(&record);
            if let Err(error) = self.retire_provider_checkpoint(&checkpoint).await {
                let recovery = self.mark_instance_recovery(original).err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "destroy {id}: provider checkpoint retirement failed: {error}{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )));
            }
            original
                .pending_provider_retirements
                .retain(|pending| pending != &record);
            self.persist_and_retain(original.clone())?;
        }

        if let Some(record) = original.provider_suspension.clone()
            && !original
                .pending_provider_suspension_retirements
                .iter()
                .any(|pending| pending == &record)
        {
            original
                .pending_provider_suspension_retirements
                .push(record);
            // The retirement identity must be durable while the public
            // hibernation owner is still present. A crash after this commit is
            // recovered by deleting the owner before issuing retirement.
            self.persist_and_retain(original.clone())?;
        }
        if !original.pending_provider_suspension_retirements.is_empty()
            && self.data_plane.suspension().is_none()
        {
            let recovery = self.mark_instance_recovery(original).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: provider suspension retirement is unavailable{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        if original.provider_suspension.take().is_some() {
            // Remove the lifecycle half of the public owner before the
            // manifest is unlinked. Startup never retires from this state
            // until it has also proved the on-disk manifest is gone.
            self.persist_and_retain(original.clone())?;
        }
        if let Err(error) = self.cleanup_hibernate_artifacts(id).await {
            let recovery = self.mark_instance_recovery(original).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: backend stopped but hibernation owner removal failed: {error}; provider suspension retained{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        for record in original.pending_provider_suspension_retirements.clone() {
            let suspension = blaze_provider_api::ProviderSuspensionRef::from_record(&record);
            if let Err(error) = self.retire_provider_suspension(&suspension).await {
                let recovery = self.mark_instance_recovery(original).err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "destroy {id}: provider suspension retirement failed: {error}{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )));
            }
            original
                .pending_provider_suspension_retirements
                .retain(|pending| pending != &record);
            self.persist_and_retain(original.clone())?;
        }

        if !data_plane_released {
            if data_plane_binding.is_some() {
                self.transition_data_plane(
                    &mut original,
                    ProviderLeaseSlot::Active,
                    ProviderTransitionKind::Release,
                    None,
                    None,
                )
                .await?;
                data_plane_released = true;
            } else if self.data_plane.capabilities().daemon_managed_storage {
                if let Err(error) = self.storage.release_by_id(&id.to_string()).await {
                    let recovery = self.mark_recovery(id).err();
                    return Err(BlazeDaemonError::RecoveryRequired(format!(
                        "destroy {id}: backend stopped but daemon-managed storage release failed: {error}; \
                         lifecycle retained for retry{}",
                        recovery
                            .map(|error| format!("; recovery state persistence failed: {error}"))
                            .unwrap_or_default()
                    )));
                }
                data_plane_released = true;
            } else {
                let recovery = self.mark_recovery(id).err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "destroy {id}: no compatible data-plane lease is available and this provider \
                     does not use daemon-managed storage{}",
                    recovery
                        .map(|error| format!("; recovery state persistence failed: {error}"))
                        .unwrap_or_default()
                )));
            }
        }
        if data_plane_released {
            self.remove_data_plane_lease(id)?;
        }

        let mut destroyed = original;
        destroyed.data_plane_lease = None;
        destroyed.replacement_data_plane_lease = None;
        destroyed.provider_suspension = None;
        destroyed.pending_provider_suspension_retirements.clear();
        destroyed.backend_runtime = None;
        if destroyed.state != SandboxState::Destroyed {
            destroyed.transition(SandboxState::Destroyed)?;
        }
        destroyed.finish_operation();
        if let Err(error) = crate::failpoint::state("destroy-final-state-commit")
            .and_then(|_| self.state_store.persist(&destroyed))
        {
            let recovery = self.mark_recovery(id).err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: resources released but final state persistence failed: {error}{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        let retention_error = self.retain_instance(destroyed);
        match self.backend_instances.lock() {
            Ok(mut instances) => {
                instances.remove(&id);
            }
            Err(poisoned) => {
                poisoned.into_inner().remove(&id);
            }
        }
        if let Some(error) = retention_error {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "destroy {id}: resources released but {error}"
            )));
        }
        self.metrics.inc(&self.metrics.instances_destroyed);
        Ok(true)
    }

    /// Reconcile every non-terminal record.
    ///
    /// A provider inventory failure aborts daemon startup before the API is
    /// exposed. Per-sandbox conflicts remain visible in the returned report and
    /// are retained in recovery or quarantine state.
    pub async fn reconcile_startup(&self) -> Result<ReconcileReport> {
        let startup_instances = self.list()?;
        for instance in &startup_instances {
            if instance.state == SandboxState::Destroyed {
                continue;
            }
            self.ensure_provider_ownership_matches(instance)?;
            self.checkpoint_ownership_preflight(instance.id).await?;
        }

        // Settle every provider call whose exact before-image was durable before
        // taking an inventory snapshot. Otherwise the same lease can appear as
        // an apparent orphan or identity conflict solely because Blaze crashed
        // between the provider response and its state-file update.
        for id in self
            .list()?
            .into_iter()
            .filter(|instance| instance.provider_transition.is_some())
            .map(|instance| instance.id)
            .collect::<Vec<_>>()
        {
            let operation_lock = self.operation_lock(id);
            let _operation = operation_lock.lock().await;
            let mut instance = self.get(id)?;
            if instance
                .provider_transition
                .is_some_and(|pending| pending.kind == ProviderTransitionKind::Adopt)
            {
                if !self.adopt_running_instance(&mut instance).await? {
                    self.settle_abandoned_provider_adoption(&mut instance)
                        .await?;
                }
            } else {
                self.settle_pending_provider_transition(&mut instance)
                    .await?;
            }
        }

        // A prepare or immutable capture has its own write-ahead identity. Its
        // enclosing operation still has to be cleaned after the provider call
        // converges, so use the ordinary destruction path while all identities
        // remain available.
        if self.data_plane.inventory().is_some() {
            for id in self
                .list()?
                .into_iter()
                .filter(|instance| {
                    instance
                        .operation
                        .as_ref()
                        .is_some_and(|operation| operation.provider_operation.is_some())
                })
                .map(|instance| instance.id)
                .collect::<Vec<_>>()
            {
                let operation_lock = self.operation_lock(id);
                let _operation = operation_lock.lock().await;
                self.destroy_locked(id).await?;
            }
        }

        // Retry only retirement identities that no longer have a public owner.
        // A transient retirement failure must not quarantine an unrelated live
        // lease during the later inventory comparison.
        for id in self
            .list()?
            .into_iter()
            .filter(|instance| {
                instance.state != SandboxState::Destroyed
                    && (!instance.pending_provider_retirements.is_empty()
                        || !instance.pending_provider_suspension_retirements.is_empty())
            })
            .map(|instance| instance.id)
            .collect::<Vec<_>>()
        {
            let operation_lock = self.operation_lock(id);
            let _operation = operation_lock.lock().await;
            let mut instance = self.get(id)?;
            self.retry_pending_provider_retirements(&mut instance)
                .await?;
        }

        for instance in self.list()? {
            if instance
                .operation
                .as_ref()
                .is_some_and(|operation| operation.provider_operation.is_some())
            {
                continue;
            }
            self.ensure_provider_ownership_matches(&instance)?;
        }
        if self.data_plane.inventory().is_some() {
            // Resolve known interrupted lifecycle states before provider-wide
            // inventory. Quarantine is reserved for a real identity conflict,
            // not used as a substitute for deterministic transaction recovery.
            for id in self
                .list()?
                .into_iter()
                .filter(|instance| {
                    !is_clean_terminal(instance)
                        && !is_clean_hibernated(
                            instance,
                            self.data_plane.capabilities().daemon_managed_storage,
                        )
                        && !requires_explicit_cleanup(instance)
                        && !is_running_adoption_candidate(instance)
                        && !instance
                            .data_plane_lease
                            .is_some_and(|record| record.state == DataPlaneLeaseState::Quarantined)
                })
                .map(|instance| instance.id)
                .collect::<Vec<_>>()
            {
                let operation_lock = self.operation_lock(id);
                let _operation = operation_lock.lock().await;
                self.destroy_locked(id).await?;
            }
            return self.reconcile_provider_startup().await;
        }
        let mut classification_failures = self.classify_interrupted_hibernation();
        let mut report = self.cleanup_owned_instances().await;
        report.failures.append(&mut classification_failures);
        Ok(report)
    }

    async fn checkpoint_ownership_preflight(&self, id: Uuid) -> Result<Vec<CheckpointMetadata>> {
        let metadata_store = self.checkpoints.clone();
        let metadata = crate::failpoint::spawn_blocking(move || {
            metadata_store
                .list_metadata(id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {id} checkpoint ownership preflight task failed: {error}; original state and resources retained"
            ))
        })?
        .map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {id} checkpoint ownership preflight failed: {error}; original state and resources retained"
            ))
        })?;
        let selected_provider = self.data_plane.descriptor().provider_instance_id;
        if metadata.iter().any(|checkpoint| {
            checkpoint
                .provider_checkpoint
                .as_ref()
                .is_some_and(|record| record.provider_instance_id != selected_provider)
        }) {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {id} has a published checkpoint owned by another provider; original state and resources retained"
            )));
        }
        Ok(metadata)
    }

    async fn retry_pending_provider_retirements(
        &self,
        instance: &mut SandboxInstance,
    ) -> Result<()> {
        let durable = self.state_store.load(instance.id).map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} retirement ledger cannot be reread from durable lifecycle state: {error}",
                instance.id
            ))
        })?;
        let checkpoint_metadata = self.checkpoint_ownership_preflight(instance.id).await?;
        for record in instance.pending_provider_retirements.clone() {
            if !durable
                .pending_provider_retirements
                .iter()
                .any(|pending| pending == &record)
            {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "sandbox {} checkpoint retirement is not present in the durable ledger",
                    instance.id
                )));
            }
            if checkpoint_metadata.iter().any(|metadata| {
                metadata.provider_checkpoint.as_ref().is_some_and(|owner| {
                    owner.provider_instance_id == record.provider_instance_id
                        && owner.public_checkpoint_id == record.public_checkpoint_id
                })
            }) {
                // The catalog is still a public owner. Destruction removes it
                // and synchronizes the catalog before retrying retirement.
                continue;
            }
            let checkpoint = ProviderCheckpointRef::from_record(&record);
            self.retire_provider_checkpoint(&checkpoint).await?;
            let mut settled = instance.clone();
            settled
                .pending_provider_retirements
                .retain(|pending| pending != &record);
            self.commit_instance_update(instance, settled)?;
        }

        for record in instance.pending_provider_suspension_retirements.clone() {
            if !durable
                .pending_provider_suspension_retirements
                .iter()
                .any(|pending| pending == &record)
            {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "sandbox {} suspension retirement is not present in the durable ledger",
                    instance.id
                )));
            }
            let lifecycle_owner = durable.provider_suspension.as_ref().is_some_and(|owner| {
                owner.provider_instance_id == record.provider_instance_id
                    && owner.suspension_id == record.suspension_id
            });
            let manifest_owner = self
                .hibernation_may_reference_provider_suspension(instance.id, &record)
                .await?;
            if lifecycle_owner || manifest_owner {
                // The lifecycle record or manifest is still a public owner.
                // The later destruction pass removes both before retirement.
                continue;
            }
            let suspension = blaze_provider_api::ProviderSuspensionRef::from_record(&record);
            self.retire_provider_suspension(&suspension).await?;
            let mut settled = instance.clone();
            settled
                .pending_provider_suspension_retirements
                .retain(|pending| pending != &record);
            self.commit_instance_update(instance, settled)?;
        }
        Ok(())
    }

    async fn consume_pending_provider_operation(
        &self,
        instance: &mut SandboxInstance,
    ) -> Result<()> {
        if instance.provider_transition.is_some() {
            self.settle_pending_provider_transition(instance).await?;
        }
        let Some(pending) = instance
            .operation
            .as_ref()
            .and_then(|operation| operation.provider_operation)
        else {
            return Ok(());
        };
        if let Err(error) = instance.validate_provider_operation() {
            return Err(self.retain_unresolved_prepare(
                instance,
                format!("provider write-ahead identity is inconsistent: {error}"),
            ));
        }
        let operation_kind = instance
            .operation
            .as_ref()
            .map(|operation| operation.kind)
            .expect("pending provider operation has an enclosing lifecycle operation");
        let descriptor = self.data_plane.descriptor();
        if validate_descriptor(descriptor).is_err()
            || pending.provider_instance_id != descriptor.provider_instance_id
        {
            return Err(self.retain_unresolved_prepare(
                instance,
                "pending provider operation belongs to another provider instance".to_string(),
            ));
        }
        match (operation_kind, pending.kind) {
            (
                OperationKind::Create | OperationKind::Restore | OperationKind::Resume,
                PendingProviderOperationKind::PrepareLease,
            ) => self.settle_pending_provider_prepare(instance).await,
            (OperationKind::Checkpoint, PendingProviderOperationKind::CheckpointCapture)
            | (OperationKind::Hibernate, PendingProviderOperationKind::SuspensionCapture { .. }) => {
                self.settle_pending_provider_capture(instance).await
            }
            _ => Err(self.retain_unresolved_provider_operation(
                instance,
                "provider write-ahead operation is not recoverable by this lifecycle path"
                    .to_string(),
            )),
        }
    }

    pub(super) async fn settle_pending_provider_prepare(
        &self,
        instance: &mut SandboxInstance,
    ) -> Result<()> {
        let Some(operation) = instance.operation.as_ref() else {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                "provider preparation has no enclosing lifecycle operation".to_string(),
            ));
        };
        let Some(pending) = operation.provider_operation else {
            return Ok(());
        };
        let replacement = match (operation.kind, pending.kind) {
            (OperationKind::Create, PendingProviderOperationKind::PrepareLease) => false,
            (
                OperationKind::Restore | OperationKind::Resume,
                PendingProviderOperationKind::PrepareLease,
            ) => true,
            _ => {
                return Err(self.retain_unresolved_provider_operation(
                    instance,
                    "pending provider operation is not a lease preparation".to_string(),
                ));
            }
        };
        if let Err(error) = instance.validate_provider_operation() {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("provider write-ahead identity is inconsistent: {error}"),
            ));
        }
        let descriptor = self.data_plane.descriptor();
        if validate_descriptor(descriptor).is_err()
            || pending.provider_instance_id != descriptor.provider_instance_id
        {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                "pending provider operation belongs to another provider instance".to_string(),
            ));
        }

        let context = RequestContext::from(pending.context);
        let observed = match self.data_plane.inspect(InspectRequest { context }).await {
            Ok(observed) => observed,
            Err(ProviderError::NotFound) => {
                let mut settled = instance.clone();
                if replacement {
                    settled.replacement_data_plane_lease = None;
                } else {
                    settled.data_plane_lease = None;
                    self.remove_data_plane_lease(instance.id)?;
                }
                settled.finish_provider_operation();
                self.commit_instance_update(instance, settled)?;
                return Ok(());
            }
            Err(error) => {
                return Err(self.retain_unresolved_provider_operation(
                    instance,
                    format!("pending provider preparation inspection failed: {error}"),
                ));
            }
        };
        let prepared_before = LeaseBinding {
            provider_instance_id: pending.provider_instance_id,
            context,
            generation: context.generation,
            state: LeaseState::Prepared,
        };
        if validate_transition(prepared_before, observed.binding, LeaseState::Released).is_ok() {
            let mut settled = instance.clone();
            if replacement {
                settled.replacement_data_plane_lease = None;
            } else {
                settled.data_plane_lease = None;
            }
            settled.finish_provider_operation();
            self.commit_instance_update(instance, settled)?;
            if !replacement {
                self.remove_data_plane_lease(instance.id)?;
            }
            return Ok(());
        }
        self.validate_prepared_response_binding(instance, context, observed.binding)?;
        let mut observed_instance = instance.clone();
        let observed_record = observed
            .binding
            .to_record(pending.root_filesystem_bytes, pending.guest_memory_bytes);
        if replacement {
            observed_instance.replacement_data_plane_lease = Some(observed_record);
        } else {
            observed_instance.data_plane_lease = Some(observed_record);
        }
        if let Err(error) = self.commit_instance_update(instance, observed_instance) {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("observed provider preparation could not be persisted: {error}"),
            ));
        }
        if !replacement
            && let Err(error) = self.retain_data_plane_lease(instance.id, observed.binding)
        {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("observed provider preparation could not be retained: {error}"),
            ));
        }
        let aborted = match self
            .data_plane
            .abort(AbortRequest {
                binding: observed.binding,
            })
            .await
        {
            Ok(aborted) => aborted,
            Err(error) => {
                return Err(self.retain_unresolved_provider_operation(
                    instance,
                    format!("pending provider preparation abort failed: {error}"),
                ));
            }
        };
        if validate_transition(observed.binding, aborted.binding, LeaseState::Released).is_err() {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                "pending provider preparation abort returned an invalid transition".to_string(),
            ));
        }
        if !replacement {
            self.remove_data_plane_lease(instance.id)?;
        }
        let mut settled = instance.clone();
        if replacement {
            settled.replacement_data_plane_lease = None;
        } else {
            settled.data_plane_lease = None;
        }
        settled.finish_provider_operation();
        self.commit_instance_update(instance, settled)
    }

    pub(super) async fn settle_pending_provider_capture(
        &self,
        instance: &mut SandboxInstance,
    ) -> Result<()> {
        let Some(operation) = instance.operation.as_ref() else {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                "provider capture has no enclosing lifecycle operation".to_string(),
            ));
        };
        let Some(pending) = operation.provider_operation else {
            return Ok(());
        };
        let public_checkpoint_id = operation.checkpoint_id.clone();
        if let Err(error) = instance.validate_provider_operation() {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("provider capture write-ahead identity is inconsistent: {error}"),
            ));
        }
        let descriptor = self.data_plane.descriptor();
        if validate_descriptor(descriptor).is_err()
            || pending.provider_instance_id != descriptor.provider_instance_id
        {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                "pending provider capture belongs to another provider instance".to_string(),
            ));
        }
        let context = RequestContext::from(pending.context);
        let observed = match self.data_plane.inspect(InspectRequest { context }).await {
            Ok(observed) => observed,
            Err(error) => {
                return Err(self.retain_unresolved_provider_operation(
                    instance,
                    format!("pending provider capture inspection failed: {error}"),
                ));
            }
        };
        let generation_after_call = pending.generation_before_call.checked_add(1);
        if observed.binding.provider_instance_id != descriptor.provider_instance_id
            || observed.binding.context != context
            || observed.binding.state != LeaseState::Finalized
            || !(observed.binding.generation == pending.generation_before_call
                || generation_after_call == Some(observed.binding.generation))
        {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                "pending provider capture inspection returned an unsafe state".to_string(),
            ));
        }
        let mut observed_instance = instance.clone();
        observed_instance.data_plane_lease = Some(
            observed
                .binding
                .to_record(pending.root_filesystem_bytes, pending.guest_memory_bytes),
        );
        if let Err(error) = self.commit_instance_update(instance, observed_instance) {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("observed provider capture generation could not be persisted: {error}"),
            ));
        }
        if let Err(error) = self.retain_data_plane_lease(instance.id, observed.binding) {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("observed provider capture generation could not be retained: {error}"),
            ));
        }

        let retirement = match pending.kind {
            PendingProviderOperationKind::CheckpointCapture => {
                let checkpoint_id = public_checkpoint_id
                    .as_deref()
                    .ok_or_else(|| {
                        self.retain_unresolved_provider_operation(
                            instance,
                            "pending provider checkpoint has no public identity".to_string(),
                        )
                    })
                    .and_then(|checkpoint_id| {
                        validate_checkpoint_id(checkpoint_id).map_err(|error| {
                            self.retain_unresolved_provider_operation(
                                instance,
                                format!("pending provider checkpoint identity is invalid: {error}"),
                            )
                        })
                    })?;
                self.retire_provider_checkpoint_identity(
                    pending.provider_instance_id,
                    checkpoint_id,
                    None,
                )
                .await
                .map(|_| ())
            }
            PendingProviderOperationKind::SuspensionCapture { suspension_id } => self
                .retire_provider_suspension_identity(
                    pending.provider_instance_id,
                    suspension_id,
                    None,
                )
                .await
                .map(|_| ()),
            PendingProviderOperationKind::PrepareLease
            | PendingProviderOperationKind::UnresolvedPrepareLease => {
                return Err(self.retain_unresolved_provider_operation(
                    instance,
                    "pending provider capture uses a preparation identity".to_string(),
                ));
            }
        };
        if let Err(error) = retirement {
            return Err(self.retain_unresolved_provider_operation(
                instance,
                format!("pending provider capture retirement failed: {error}"),
            ));
        }

        let mut settled = instance.clone();
        settled.finish_provider_operation();
        self.commit_instance_update(instance, settled)
    }

    fn ensure_provider_ownership_matches(&self, instance: &SandboxInstance) -> Result<()> {
        let selected = self.data_plane.descriptor().provider_instance_id;
        let foreign = instance
            .data_plane_lease
            .iter()
            .map(|record| record.provider_instance_id)
            .chain(
                instance
                    .provider_transition
                    .iter()
                    .map(|record| record.before.provider_instance_id),
            )
            .chain(
                instance
                    .operation
                    .iter()
                    .filter_map(|operation| operation.provider_operation)
                    .map(|record| record.provider_instance_id),
            )
            .chain(
                instance
                    .replacement_data_plane_lease
                    .iter()
                    .map(|record| record.provider_instance_id),
            )
            .chain(
                instance
                    .pending_provider_retirements
                    .iter()
                    .map(|record| record.provider_instance_id),
            )
            .chain(
                instance
                    .provider_suspension
                    .iter()
                    .map(|record| record.provider_instance_id),
            )
            .chain(
                instance
                    .pending_provider_suspension_retirements
                    .iter()
                    .map(|record| record.provider_instance_id),
            )
            .any(|owner| owner != selected);
        if foreign {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} has durable data-plane ownership from another provider; resources and ownership records were retained",
                instance.id
            )));
        }
        Ok(())
    }

    async fn reconcile_provider_startup(&self) -> Result<ReconcileReport> {
        const INVENTORY_PAGE_SIZE: u32 = 256;
        const MAX_INVENTORY_LEASES: usize = 1_000_000;

        let inventory = self
            .data_plane
            .inventory()
            .expect("inventory extension was checked");
        let descriptor = self.data_plane.descriptor();
        let snapshot = inventory
            .begin_inventory(BeginInventoryRequest {
                page_size: INVENTORY_PAGE_SIZE,
            })
            .await?;
        if validate_inventory_snapshot(descriptor, snapshot).is_err() {
            return Err(BlazeDaemonError::RecoveryRequired(
                "data-plane inventory returned an invalid snapshot identity".to_string(),
            ));
        }

        let mut observed_by_lease = HashMap::new();
        let mut seen_cursors = HashSet::new();
        let mut cursor: Option<String> = None;
        let mut page_count = 0_usize;
        loop {
            page_count += 1;
            if page_count > MAX_INVENTORY_PAGES {
                return Err(BlazeDaemonError::RecoveryRequired(
                    "data-plane inventory exceeded the page-count safety bound".to_string(),
                ));
            }
            if let Some(value) = cursor.as_ref()
                && !seen_cursors.insert(value.clone())
            {
                return Err(BlazeDaemonError::RecoveryRequired(
                    "data-plane inventory repeated a page cursor".to_string(),
                ));
            }
            let page = inventory
                .inventory_page(InventoryPageRequest {
                    snapshot_id: snapshot.snapshot_id,
                    cursor: cursor.clone(),
                    page_size: INVENTORY_PAGE_SIZE,
                })
                .await?;
            if validate_inventory_page(&page, INVENTORY_PAGE_SIZE).is_err() {
                return Err(BlazeDaemonError::RecoveryRequired(
                    "data-plane inventory returned an invalid page".to_string(),
                ));
            }
            for lease in page.leases {
                let binding = lease.binding;
                if validate_inventory_lease(descriptor, binding).is_err() {
                    return Err(BlazeDaemonError::RecoveryRequired(
                        "data-plane inventory contains an invalid lease identity".to_string(),
                    ));
                }
                if observed_by_lease
                    .insert(binding.context.lease_id, binding)
                    .is_some()
                {
                    return Err(BlazeDaemonError::RecoveryRequired(
                        "data-plane inventory contains a duplicate lease".to_string(),
                    ));
                }
                if observed_by_lease.len() > MAX_INVENTORY_LEASES {
                    return Err(BlazeDaemonError::RecoveryRequired(
                        "data-plane inventory exceeds the safety bound".to_string(),
                    ));
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        let persisted = self.list()?;
        let daemon_managed_storage = self.data_plane.capabilities().daemon_managed_storage;
        let mut report = ReconcileReport {
            attempted: persisted
                .iter()
                .filter(|instance| requires_automatic_cleanup(instance, daemon_managed_storage))
                .count(),
            ..ReconcileReport::default()
        };
        for mut instance in persisted {
            let id = instance.id;
            let expected = instance
                .data_plane_lease
                .map(|record| LeaseBinding::from_record(id, record));
            let expected_replacement = instance
                .replacement_data_plane_lease
                .map(|record| LeaseBinding::from_record(id, record));
            if !requires_automatic_cleanup(&instance, daemon_managed_storage) {
                // Some recovery records intentionally wait for an explicit
                // destroy. Their exact leases remain publicly owned during
                // that wait and therefore are not provider orphans. Consume
                // only an exact inventory match; a same-ID mismatch stays in
                // the orphan set and is quarantined below.
                for expected in [expected, expected_replacement].into_iter().flatten() {
                    if observed_by_lease.get(&expected.context.lease_id) == Some(&expected) {
                        observed_by_lease.remove(&expected.context.lease_id);
                    }
                }
                continue;
            }
            let operation_lock = self.operation_lock(id);
            let _operation = operation_lock.lock().await;
            let observed =
                expected.and_then(|binding| observed_by_lease.remove(&binding.context.lease_id));
            let observed_replacement = expected_replacement
                .and_then(|binding| observed_by_lease.remove(&binding.context.lease_id));
            let adoptable = is_running_adoption_candidate(&instance)
                && expected.is_some()
                && expected == observed
                && expected.is_some_and(|binding| {
                    matches!(binding.state, LeaseState::Committed | LeaseState::Finalized)
                });

            if adoptable {
                if self.backend_owner(id).is_some() {
                    report.completed += 1;
                    continue;
                }
                match self.adopt_running_instance(&mut instance).await {
                    Ok(true) => {
                        report.completed += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        if instance.provider_transition.is_some() {
                            return Err(error);
                        }
                        report.failures.push(ReconcileFailure {
                            instance_id: id,
                            error: error.to_string(),
                        });
                    }
                }
            }

            if let Some(observed_replacement) = observed_replacement {
                match inventory
                    .reconcile(ReconcileRequest {
                        expected: expected_replacement,
                        observed: observed_replacement,
                        action: ReconcileAction::Quarantine,
                    })
                    .await
                {
                    Ok(result)
                        if validate_reconcile_result(
                            observed_replacement,
                            result.binding,
                            ReconcileAction::Quarantine,
                        )
                        .is_ok() =>
                    {
                        if let Some(record) = instance.replacement_data_plane_lease {
                            instance.replacement_data_plane_lease = Some(result.binding.to_record(
                                record.root_filesystem_bytes,
                                record.guest_memory_bytes,
                            ));
                        }
                    }
                    Ok(_) => report.failures.push(ReconcileFailure {
                        instance_id: id,
                        error: "provider replacement quarantine returned an invalid transition"
                            .to_string(),
                    }),
                    Err(error) => report.failures.push(ReconcileFailure {
                        instance_id: id,
                        error: format!("provider replacement quarantine failed: {error}"),
                    }),
                }
            }

            if let Some(observed) = observed {
                let quarantine = inventory
                    .reconcile(ReconcileRequest {
                        expected,
                        observed,
                        action: ReconcileAction::Quarantine,
                    })
                    .await;
                match quarantine {
                    Ok(result)
                        if validate_reconcile_result(
                            observed,
                            result.binding,
                            ReconcileAction::Quarantine,
                        )
                        .is_ok() =>
                    {
                        if let Some(record) = instance.data_plane_lease {
                            instance.data_plane_lease = Some(result.binding.to_record(
                                record.root_filesystem_bytes,
                                record.guest_memory_bytes,
                            ));
                        }
                    }
                    Ok(_) => report.failures.push(ReconcileFailure {
                        instance_id: id,
                        error: "provider quarantine returned an invalid transition".to_string(),
                    }),
                    Err(error) => report.failures.push(ReconcileFailure {
                        instance_id: id,
                        error: format!("provider quarantine failed: {error}"),
                    }),
                }
            }
            if let Some(spawner) = self.spawner(instance.backend)
                && !matches!(
                    instance.backend_ownership,
                    BackendOwnership::NotStarted | BackendOwnership::Stopped
                )
                && let Ok(run_dir) = self.state_store.run_dir(id)
            {
                if let Err(error) = spawner.cleanup_orphan(id, &run_dir).await {
                    report.failures.push(ReconcileFailure {
                        instance_id: id,
                        error: format!("backend quarantine failed: {error}"),
                    });
                } else {
                    instance.backend_ownership = BackendOwnership::Stopped;
                }
            }
            if let Err(error) = self.mark_instance_recovery(instance) {
                report.failures.push(ReconcileFailure {
                    instance_id: id,
                    error: format!("recovery state persistence failed: {error}"),
                });
            } else if !report
                .failures
                .iter()
                .any(|failure| failure.instance_id == id)
            {
                report.failures.push(ReconcileFailure {
                    instance_id: id,
                    error: "provider, public state, and backend identity did not agree".to_string(),
                });
            }
        }

        for observed in observed_by_lease.into_values() {
            match inventory
                .reconcile(ReconcileRequest {
                    expected: None,
                    observed,
                    action: ReconcileAction::Quarantine,
                })
                .await
            {
                Ok(result)
                    if validate_reconcile_result(
                        observed,
                        result.binding,
                        ReconcileAction::Quarantine,
                    )
                    .is_ok() =>
                {
                    report.failures.push(ReconcileFailure {
                        instance_id: observed.context.instance_id,
                        error: "provider lease has no public owner and was quarantined".to_string(),
                    });
                }
                Ok(_) => report.failures.push(ReconcileFailure {
                    instance_id: observed.context.instance_id,
                    error: "orphan quarantine returned an invalid transition".to_string(),
                }),
                Err(error) => report.failures.push(ReconcileFailure {
                    instance_id: observed.context.instance_id,
                    error: format!("orphan quarantine failed: {error}"),
                }),
            }
        }
        Ok(report)
    }

    async fn adopt_running_instance(&self, instance: &mut SandboxInstance) -> Result<bool> {
        let runtime = instance.backend_runtime.as_ref().ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} has no durable backend identity",
                instance.id
            ))
        })?;
        let process = runtime.process.ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} has no adoptable backend process",
                instance.id
            ))
        })?;
        if let Some(pending) = instance.provider_transition
            && (pending.kind != ProviderTransitionKind::Adopt
                || pending.backend_process != Some(process))
        {
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} adoption WAL does not match its durable backend identity",
                instance.id
            )));
        }
        let spawner = self.spawner(instance.backend).ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} has no registered recovery backend",
                instance.id
            ))
        })?;
        let record = instance.data_plane_lease.ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} has no durable data-plane lease",
                instance.id
            ))
        })?;
        let run_dir = self.state_store.run_dir(instance.id)?;
        let Some(owner) = adopt_with_runtime_directory(
            spawner.as_ref(),
            instance.id,
            runtime,
            run_dir,
            record.guest_memory_bytes,
        )
        .await?
        else {
            return Ok(false);
        };
        if let Err(error) = self
            .wait_for_guest_ready(&owner, "startup-adopt-guest-ready")
            .await
        {
            let cleanup = owner.kill().await.err();
            return Err(BlazeDaemonError::RecoveryRequired(format!(
                "sandbox {} adopted backend failed readiness: {error}{}",
                instance.id,
                cleanup
                    .map(|error| format!("; backend cleanup failed: {error}"))
                    .unwrap_or_default()
            )));
        }
        instance.backend_runtime = Some(owner.runtime_record());
        let transition = if instance.provider_transition.is_some() {
            self.settle_pending_provider_transition(instance).await
        } else {
            self.transition_data_plane(
                instance,
                ProviderLeaseSlot::Active,
                ProviderTransitionKind::Adopt,
                None,
                Some(process),
            )
            .await
        };
        let reconciled = match transition {
            Ok(reconciled) => reconciled,
            Err(error) => {
                if instance.provider_transition.is_some() {
                    self.backend_instances
                        .lock()
                        .map_err(|_| poisoned("backend_instances"))?
                        .insert(instance.id, owner);
                    return Err(BlazeDaemonError::RecoveryRequired(format!(
                        "sandbox {} provider adoption outcome is unresolved; backend owner and WAL retained: {error}",
                        instance.id
                    )));
                }
                let cleanup = owner.kill().await.err();
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "sandbox {} provider adoption failed: {error}{}",
                    instance.id,
                    cleanup
                        .map(|error| format!("; backend cleanup failed: {error}"))
                        .unwrap_or_default()
                )));
            }
        };
        debug_assert_eq!(reconciled.state, LeaseState::Finalized);
        self.backend_instances
            .lock()
            .map_err(|_| poisoned("backend_instances"))?
            .insert(instance.id, owner);
        Ok(true)
    }

    /// Resolve an adoption WAL after the recorded backend is proven absent.
    ///
    /// Replaying `Adopt` against an exited process would attach provider
    /// ownership to a backend that can never serve. Accept only a transition
    /// that the provider already completed; otherwise clear the unissued
    /// intent and let the ordinary inventory comparison quarantine or reclaim
    /// the remaining lease without blocking every later daemon start.
    async fn settle_abandoned_provider_adoption(
        &self,
        instance: &mut SandboxInstance,
    ) -> Result<()> {
        instance.validate_provider_transition().map_err(|error| {
            BlazeDaemonError::RecoveryRequired(format!(
                "provider adoption WAL is inconsistent: {error}"
            ))
        })?;
        let pending = instance.provider_transition.ok_or_else(|| {
            BlazeDaemonError::RecoveryRequired(
                "abandoned provider adoption has no durable WAL".to_string(),
            )
        })?;
        if pending.kind != ProviderTransitionKind::Adopt {
            return Err(BlazeDaemonError::RecoveryRequired(
                "abandoned provider transition is not an adoption".to_string(),
            ));
        }
        let before = LeaseBinding::from_record(instance.id, pending.before);
        if before.provider_instance_id != self.data_plane.descriptor().provider_instance_id {
            return Err(BlazeDaemonError::RecoveryRequired(
                "provider adoption belongs to another provider instance; WAL retained".to_string(),
            ));
        }
        let observed = self
            .data_plane
            .inspect(InspectRequest {
                context: before.context,
            })
            .await;
        match observed {
            Ok(observed)
                if validate_transition(before, observed.binding, LeaseState::Finalized).is_ok() =>
            {
                self.accept_provider_transition(instance, pending, observed.binding)?;
            }
            Ok(observed) if observed.binding == before => {
                let mut cleared = instance.clone();
                cleared.finish_provider_transition();
                self.commit_instance_update(instance, cleared)?;
            }
            Err(ProviderError::NotFound) => {
                // Neither the recorded backend nor this exact provider lease
                // exists. Retain a local released tombstone so destruction can
                // remove public metadata without issuing an unsafe transition
                // against a resource the provider proved absent.
                let released = LeaseBinding {
                    state: LeaseState::Released,
                    ..before
                };
                let mut cleared = instance.clone();
                cleared.data_plane_lease = Some(released.to_record(
                    pending.before.root_filesystem_bytes,
                    pending.before.guest_memory_bytes,
                ));
                cleared.finish_provider_transition();
                self.commit_instance_update(instance, cleared)?;
                self.retain_data_plane_lease(instance.id, released)?;
            }
            Ok(_) => {
                return Err(BlazeDaemonError::RecoveryRequired(
                    "abandoned provider adoption observed neither its before-image nor exact successor; WAL retained"
                        .to_string(),
                ));
            }
            Err(error) => {
                return Err(BlazeDaemonError::RecoveryRequired(format!(
                    "abandoned provider adoption inspection failed: {error}; WAL retained"
                )));
            }
        }
        // Failure to re-adopt proves that the recorded process no longer owns
        // a live backend. Persist that fact before the deterministic cleanup
        // pass so the surviving provider lease is stopped and released instead
        // of repeatedly being treated as an adoptable running sandbox.
        let mut stopped = instance.clone();
        stopped.backend_ownership = BackendOwnership::Stopped;
        stopped.backend_runtime = None;
        self.commit_instance_update(instance, stopped)?;
        Ok(())
    }

    async fn lock_running(&self, id: Uuid) -> Result<OwnedMutexGuard<()>> {
        let operation = self.operation_lock(id).lock_owned().await;
        let instance = self.get(id)?;
        if instance.state != SandboxState::Running || instance.operation.is_some() {
            return Err(BlazeDaemonError::Conflict(format!(
                "instance {id} is not available for guest operations"
            )));
        }
        Ok(operation)
    }

    fn classify_interrupted_hibernation(&self) -> Vec<ReconcileFailure> {
        let interrupted = match self.instances.lock() {
            Ok(instances) => instances
                .values()
                .filter(|instance| {
                    matches!(
                        instance.state,
                        SandboxState::Hibernating | SandboxState::Resuming
                    ) || matches!(
                        instance.operation.as_ref().map(|operation| operation.kind),
                        Some(OperationKind::Hibernate | OperationKind::Resume)
                    )
                })
                .cloned()
                .collect::<Vec<_>>(),
            Err(poisoned) => poisoned
                .into_inner()
                .values()
                .filter(|instance| {
                    matches!(
                        instance.state,
                        SandboxState::Hibernating | SandboxState::Resuming
                    ) || matches!(
                        instance.operation.as_ref().map(|operation| operation.kind),
                        Some(OperationKind::Hibernate | OperationKind::Resume)
                    )
                })
                .cloned()
                .collect::<Vec<_>>(),
        };
        interrupted
            .into_iter()
            .filter_map(|instance| {
                let id = instance.id;
                self.mark_instance_recovery(instance)
                    .err()
                    .map(|error| ReconcileFailure {
                        instance_id: id,
                        error: format!("interrupted hibernation classification failed: {error}"),
                    })
            })
            .collect()
    }

    fn guest_client(&self, id: Uuid) -> Result<GuestClient> {
        let backend = self
            .backend_instances
            .lock()
            .map_err(|_| poisoned("backend_instances"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| {
                BlazeDaemonError::Conflict(format!("instance {id} has no backend owner"))
            })?;
        let socket = backend.guest_socket_path();
        if socket.as_os_str().is_empty() {
            return Err(BlazeDaemonError::Conflict(format!(
                "instance {id} has no guest transport"
            )));
        }
        Ok(GuestClient::new(
            socket.to_path_buf(),
            GUEST_REQUEST_TIMEOUT,
            MAX_GUEST_FILE_BYTES,
        ))
    }

    pub(super) async fn wait_for_guest_ready(
        &self,
        backend: &DynBackendInstance,
        failpoint: &str,
    ) -> crate::guest::Result<()> {
        let socket = backend.guest_socket_path();
        if socket.as_os_str().is_empty() {
            return Ok(());
        }
        crate::failpoint::guest(failpoint)?;
        GuestClient::new(
            socket.to_path_buf(),
            GUEST_REQUEST_TIMEOUT,
            MAX_GUEST_FILE_BYTES,
        )
        .wait_ready(GUEST_REQUEST_TIMEOUT, &CancellationToken::new())
        .await
    }

    /// Release every instance that lifecycle cleanup still owns.
    ///
    /// Startup reconciliation has no external deadline, so each record gets the
    /// full per-sandbox operation lock without a timeout.
    pub async fn cleanup_owned_instances(&self) -> ReconcileReport {
        let ids = match self.owned_instance_ids() {
            Ok(ids) => ids,
            Err(error) => {
                return ReconcileReport {
                    attempted: 0,
                    completed: 0,
                    failures: vec![ReconcileFailure {
                        instance_id: Uuid::nil(),
                        error: format!("owned instance inventory unavailable: {error}"),
                    }],
                };
            }
        };
        let mut report = ReconcileReport {
            attempted: ids.len(),
            ..ReconcileReport::default()
        };
        for id in ids {
            let operation_lock = self.operation_lock(id);
            let _operation = operation_lock.lock().await;
            match self.destroy_locked(id).await {
                Ok(_) => report.completed += 1,
                Err(error) => {
                    let recovery = self.mark_recovery(id).err();
                    report.failures.push(ReconcileFailure {
                        instance_id: id,
                        error: match recovery {
                            Some(recovery) => {
                                format!("{error}; recovery state persistence failed: {recovery}")
                            }
                            None => error.to_string(),
                        },
                    });
                }
            }
        }
        report
    }

    async fn cleanup_failed_create(
        &self,
        instance: &mut SandboxInstance,
        binding: LeaseBinding,
        backend: Option<DynBackendInstance>,
        registered: bool,
        original: BlazeDaemonError,
    ) -> BlazeDaemonError {
        if instance.provider_transition.is_some() {
            let recovery = self.mark_instance_recovery(instance.clone()).err();
            return BlazeDaemonError::RecoveryRequired(format!(
                "{original}; provider transition outcome is unresolved and its WAL was retained{}",
                recovery
                    .map(|error| format!("; recovery state persistence failed: {error}"))
                    .unwrap_or_default()
            ));
        }
        if instance.operation.is_none() {
            instance.begin_operation(OperationKind::Create);
        }
        let mut cleanup_errors = Vec::new();
        let backend = if registered {
            match self.backend_instances.lock() {
                Ok(mut instances) => instances.remove(&instance.id),
                Err(poisoned) => poisoned.into_inner().remove(&instance.id),
            }
        } else {
            backend
        };
        let mut backend_stopped = matches!(
            instance.backend_ownership,
            BackendOwnership::NotStarted | BackendOwnership::Stopped
        );
        if registered && backend.is_none() {
            backend_stopped = false;
            cleanup_errors.push("registered backend owner is missing".to_string());
        }
        if let Some(backend) = backend.as_ref() {
            match backend.kill().await {
                Ok(()) => {
                    backend_stopped = true;
                    instance.backend_ownership = BackendOwnership::Stopped;
                }
                Err(error) => {
                    backend_stopped = false;
                    cleanup_errors.push(format!("backend termination failed: {error}"));
                }
            }
        }

        let mut data_plane_released = false;
        if backend_stopped {
            if binding.state == LeaseState::Released {
                data_plane_released = true;
            } else if instance
                .data_plane_lease
                .is_some_and(|record| LeaseBinding::from_record(instance.id, record) == binding)
            {
                match self
                    .transition_data_plane(
                        instance,
                        ProviderLeaseSlot::Active,
                        ProviderTransitionKind::Abort,
                        None,
                        None,
                    )
                    .await
                {
                    Ok(_) => data_plane_released = true,
                    Err(error) => {
                        cleanup_errors.push(format!("data-plane abort failed: {error}"));
                    }
                }
            } else {
                cleanup_errors
                    .push("data-plane abort identity does not match its durable lease".to_string());
            }
            if data_plane_released && let Err(error) = self.remove_data_plane_lease(instance.id) {
                cleanup_errors.push(format!(
                    "data-plane lease retention cleanup failed: {error}"
                ));
            }
        } else {
            cleanup_errors.push(
                "data-plane resources retained until backend termination succeeds".to_string(),
            );
        }

        if backend_stopped && data_plane_released {
            cleanup_errors.extend(self.commit_create_rollback(instance));
            if cleanup_errors.is_empty() {
                self.metrics.inc(&self.metrics.instances_destroyed);
                return original;
            }
            return BlazeDaemonError::RecoveryRequired(format!(
                "{original}; cleanup completed but {}",
                cleanup_errors.join("; ")
            ));
        }

        if let Some(backend) = backend
            && let Some(error) = self.retain_backend(instance.id, backend)
        {
            cleanup_errors.push(error);
        }
        if instance.state != SandboxState::RecoveryRequired
            && let Err(error) = instance.transition(SandboxState::RecoveryRequired)
        {
            cleanup_errors.push(format!("recovery state update failed: {error}"));
        }
        if let Err(error) = self.state_store.persist(instance) {
            cleanup_errors.push(format!("state persistence failed: {error}"));
        }
        if let Some(error) = self.retain_instance(instance.clone()) {
            cleanup_errors.push(error);
        }
        BlazeDaemonError::RecoveryRequired(format!(
            "{original}; cleanup incomplete: {}",
            cleanup_errors.join("; ")
        ))
    }

    /// Commit a fully compensated create as terminal without losing the
    /// operation record when that terminal commit itself fails.
    fn commit_create_rollback(&self, instance: &mut SandboxInstance) -> Vec<String> {
        let recoverable = instance.clone();
        let mut terminal = recoverable.clone();
        terminal.backend_ownership = BackendOwnership::Stopped;
        terminal.backend_runtime = None;
        terminal.data_plane_lease = None;
        let terminal_result = (|| -> Result<()> {
            if terminal.state != SandboxState::Destroyed {
                terminal.transition(SandboxState::Destroyed)?;
            }
            terminal.finish_operation();
            crate::failpoint::state("create-rollback-final-state-commit")?;
            self.state_store.persist(&terminal)
        })();

        match terminal_result {
            Ok(()) => {
                *instance = terminal.clone();
                self.retain_instance(terminal).into_iter().collect()
            }
            Err(error) => {
                let mut errors = vec![format!("final state persistence failed: {error}")];
                let mut recovery = recoverable;
                recovery.backend_ownership = BackendOwnership::Stopped;
                if recovery.state != SandboxState::RecoveryRequired
                    && let Err(error) = recovery.transition(SandboxState::RecoveryRequired)
                {
                    errors.push(format!("recovery state update failed: {error}"));
                }
                if let Err(error) = self.state_store.persist(&recovery) {
                    errors.push(format!("recovery state persistence failed: {error}"));
                }
                if let Some(error) = self.retain_instance(recovery.clone()) {
                    errors.push(error);
                }
                *instance = recovery;
                errors
            }
        }
    }

    pub(super) fn mark_recovery(&self, id: Uuid) -> Result<()> {
        self.mark_instance_recovery(self.get(id)?)
    }

    pub(super) fn persist_and_retain(&self, instance: SandboxInstance) -> Result<()> {
        self.state_store.persist(&instance)?;
        if let Some(error) = self.retain_instance(instance) {
            return Err(BlazeDaemonError::RecoveryRequired(error));
        }
        Ok(())
    }

    pub(super) fn mark_instance_recovery(&self, mut instance: SandboxInstance) -> Result<()> {
        if instance.state != SandboxState::RecoveryRequired {
            instance.transition(SandboxState::RecoveryRequired)?;
        }
        let persist = self.state_store.persist(&instance);
        let retained = self.retain_instance(instance);
        match (persist, retained) {
            (Ok(()), None) => Ok(()),
            (Err(error), None) => Err(error),
            (Ok(()), Some(error)) => Err(BlazeDaemonError::Internal(error)),
            (Err(persist), Some(retain)) => Err(BlazeDaemonError::RecoveryRequired(format!(
                "recovery state persistence failed: {persist}; {retain}"
            ))),
        }
    }

    pub(super) fn retain_backend(&self, id: Uuid, backend: DynBackendInstance) -> Option<String> {
        match self.backend_instances.lock() {
            Ok(mut instances) => {
                instances.insert(id, backend);
                None
            }
            Err(poisoned) => {
                poisoned.into_inner().insert(id, backend);
                Some("backend owner retained in poisoned runtime map".to_string())
            }
        }
    }

    pub(super) fn retain_instance(&self, instance: SandboxInstance) -> Option<String> {
        match self.instances.lock() {
            Ok(mut instances) => {
                instances.insert(instance.id, instance);
                None
            }
            Err(poisoned) => {
                poisoned.into_inner().insert(instance.id, instance);
                Some("instance state retained in poisoned lifecycle map".to_string())
            }
        }
    }
}

fn poisoned(name: &str) -> BlazeDaemonError {
    BlazeDaemonError::Internal(format!("{name} lock poisoned"))
}

fn provider_transition_target(before: LeaseBinding, target: LeaseState) -> Result<LeaseBinding> {
    let generation = before.generation.checked_add(1).ok_or_else(|| {
        BlazeDaemonError::RecoveryRequired(
            "provider transition generation overflowed; WAL retained".to_string(),
        )
    })?;
    Ok(LeaseBinding {
        provider_instance_id: before.provider_instance_id,
        context: before.context,
        generation,
        state: target,
    })
}

pub(super) fn checkpoint_retirement_operation_id(
    provider_instance_id: Uuid,
    public_checkpoint_id: Uuid,
    reference_id: Option<Uuid>,
) -> Uuid {
    retirement_operation_id(
        b"checkpoint",
        provider_instance_id,
        public_checkpoint_id,
        reference_id,
    )
}

pub(super) fn suspension_retirement_operation_id(
    provider_instance_id: Uuid,
    suspension_id: Uuid,
    reference_id: Option<Uuid>,
) -> Uuid {
    retirement_operation_id(
        b"suspension",
        provider_instance_id,
        suspension_id,
        reference_id,
    )
}

fn retirement_operation_id(
    domain: &[u8],
    provider_instance_id: Uuid,
    public_owner_id: Uuid,
    reference_id: Option<Uuid>,
) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"blaze-provider-retirement-v1\0");
    digest.update(domain);
    digest.update([0]);
    digest.update(provider_instance_id.as_bytes());
    digest.update(public_owner_id.as_bytes());
    match reference_id {
        Some(reference_id) => {
            digest.update([1]);
            digest.update(reference_id.as_bytes());
        }
        None => digest.update([0]),
    }
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 9562 version 8 identifies an application-defined UUID layout.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn is_clean_terminal(instance: &SandboxInstance) -> bool {
    instance.state == SandboxState::Destroyed
        && instance.operation.is_none()
        && instance.provider_transition.is_none()
        && instance.data_plane_lease.is_none()
        && instance.replacement_data_plane_lease.is_none()
        && instance.pending_provider_retirements.is_empty()
        && instance.provider_suspension.is_none()
        && instance.pending_provider_suspension_retirements.is_empty()
        && instance.backend_runtime.is_none()
        && matches!(
            instance.backend_ownership,
            BackendOwnership::NotStarted | BackendOwnership::Stopped
        )
}

fn is_clean_hibernated(instance: &SandboxInstance, daemon_managed_storage: bool) -> bool {
    // The standard file-backed hibernation path keeps its finalized storage
    // lease: resume reconstructs the root filesystem and memory files from
    // that durable owner. A provider-managed suspension instead releases the
    // active lease and records an immutable suspension owner. Both are clean
    // hibernated states, but a non-finalized live lease is never one.
    let data_plane_is_clean = match instance.data_plane_lease {
        None => true,
        Some(record) => {
            daemon_managed_storage
                && record.state == DataPlaneLeaseState::Finalized
                && instance.provider_suspension.is_none()
        }
    };
    instance.state == SandboxState::Hibernated
        && instance.operation.is_none()
        && instance.provider_transition.is_none()
        && data_plane_is_clean
        && instance.replacement_data_plane_lease.is_none()
        && instance.pending_provider_retirements.is_empty()
        && instance.pending_provider_suspension_retirements.is_empty()
        && instance
            .backend_runtime
            .as_ref()
            .is_none_or(|runtime| runtime.process.is_none())
        && instance.backend_ownership == BackendOwnership::Stopped
}

fn is_running_adoption_candidate(instance: &SandboxInstance) -> bool {
    instance.state == SandboxState::Running
        && instance.operation.is_none()
        && instance.provider_transition.is_none()
        && instance.data_plane_lease.is_some_and(|record| {
            matches!(
                record.state,
                DataPlaneLeaseState::Committed | DataPlaneLeaseState::Finalized
            )
        })
        && instance.replacement_data_plane_lease.is_none()
        && instance.pending_provider_retirements.is_empty()
        && instance.pending_provider_suspension_retirements.is_empty()
        && instance.backend_runtime.is_some()
        && instance.backend_ownership == BackendOwnership::Running
}

fn requires_automatic_cleanup(instance: &SandboxInstance, daemon_managed_storage: bool) -> bool {
    !(is_clean_terminal(instance)
        || is_clean_hibernated(instance, daemon_managed_storage)
        || requires_explicit_cleanup(instance))
}

// An interrupted hibernation or resume without a provider write-ahead identity
// is intentionally retained for explicit cleanup. Startup can no longer prove
// whether its published artifacts are the old or new public owner, so automatic
// destruction would turn a recoverable state into data loss. When the durable
// operation contains a provider identity, `destroy_locked` can first inspect
// that exact request and deterministically abort or retire its side effect; do
// not hide that recoverable transaction from automatic cleanup merely because
// the provider does not implement whole-provider inventory.
fn requires_explicit_cleanup(instance: &SandboxInstance) -> bool {
    instance.state == SandboxState::RecoveryRequired
        && matches!(
            instance.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Hibernate | OperationKind::Resume)
        )
        && instance
            .operation
            .as_ref()
            .is_none_or(|operation| operation.provider_operation.is_none())
}

/// Require the command line captured in a Firecracker snapshot to equal the
/// command line the matched policy would use for a cold start.
///
/// Restore loads the captured machine configuration and does not call
/// `write_vm_config`, so accepting a mismatch would silently bypass current
/// policy controls.
fn validate_template_boot_args(
    template_name: &str,
    captured: Option<&str>,
    expected: &str,
    policy_name: &str,
) -> Result<()> {
    if captured == Some(expected) {
        return Ok(());
    }
    Err(BlazeDaemonError::Conflict(format!(
        "template {template_name} kernel boot arguments do not match policy {policy_name}"
    )))
}

pub(super) fn provider_restore_attachments(
    binding: LeaseBinding,
    attachments: Vec<blaze_provider_api::OpenedAttachment>,
) -> crate::spawner::ProviderRestoreAttachments {
    use crate::spawner::{
        ProviderAttachmentAccess, ProviderAttachmentKind, ProviderAttachmentRole,
        ProviderAttachmentSharing, ProviderRestoreAttachment, ProviderRestoreAttachments,
    };
    use blaze_provider_api::{AttachmentKind, AttachmentRole};

    let attachments = attachments
        .into_iter()
        .map(|attachment| ProviderRestoreAttachment {
            role: match attachment.role {
                AttachmentRole::RootDrive => ProviderAttachmentRole::RootDrive,
                AttachmentRole::GuestMemory => ProviderAttachmentRole::GuestMemory,
            },
            file: Arc::new(std::fs::File::from(attachment.descriptor)),
            access: ProviderAttachmentAccess::ReadWrite,
            sharing: ProviderAttachmentSharing::Exclusive,
            kind: match attachment.kind {
                AttachmentKind::RegularFile => ProviderAttachmentKind::RegularFile,
                AttachmentKind::CharacterDevice => ProviderAttachmentKind::CharacterDevice,
                AttachmentKind::BlockDevice => ProviderAttachmentKind::BlockDevice,
            },
            logical_size_bytes: attachment.logical_size_bytes,
            consumer_path: attachment.consumer_path,
        })
        .collect();
    ProviderRestoreAttachments {
        instance_id: binding.context.instance_id,
        lease_id: binding.context.lease_id,
        generation: binding.generation,
        attachments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firecracker_template_boot_arguments_must_match_the_policy_exactly() {
        validate_template_boot_args(
            "runtime-base",
            Some("console=ttyS0 panic=1"),
            "console=ttyS0 panic=1",
            "agent-tool",
        )
        .expect("identical command lines");

        for captured in [None, Some("console=ttyS0 panic=2")] {
            let error = validate_template_boot_args(
                "runtime-base",
                captured,
                "console=ttyS0 panic=1",
                "agent-tool",
            )
            .expect_err("missing or different command lines must be rejected");
            assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        }
    }
}
