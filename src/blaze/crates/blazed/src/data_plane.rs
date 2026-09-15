// SPDX-License-Identifier: Apache-2.0
//! Standard file-backed implementation of the build-time provider contract.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use blaze_core::BlazeError;
use blaze_core::data_plane::DataPlaneLeaseState;
use blaze_core::storage::{
    AcquireOpts, StorageOwnershipClaim, StorageOwnershipKey, StorageOwnershipLookup,
    StorageOwnershipPhase, StorageOwnershipRequest, StorageProvider, StorageSlot,
};
use blaze_provider_api::{
    AbortRequest, AbortResult, CommitRequest, CommittedLease, DataPlaneProvider, FinalizeRequest,
    FinalizedLease, InspectRequest, LeaseBinding, LeaseState, PROVIDER_CONTRACT_VERSION,
    PrepareRequest, PrepareSource, PreparedLease, PreparedResources, ProviderCapabilities,
    ProviderDescriptor, ProviderError, ReleaseRequest, ReleaseResult, StopRequest, StoppedLease,
};
use uuid::Uuid;

/// Fallback identity for storage doubles without durable ownership domains.
///
/// Production file storage derives a distinct stable identity from its opened
/// instances root. This value keeps isolated contract tests deterministic.
const FILE_PROVIDER_INSTANCE_ID: Uuid = Uuid::from_u128(0x2e70_c3d5_9878_4a8d_8a69_0214_4f15_b3a1);

#[derive(Debug, Clone)]
struct FileLease {
    binding: LeaseBinding,
    storage: StorageSlot,
    ownership: StorageOwnershipClaim,
    restore_payload_dir: Option<std::path::PathBuf>,
}

impl FileLease {
    fn resources(&self) -> PreparedResources {
        PreparedResources::PathBacked {
            storage: self.storage.clone(),
            restore_payload_dir: self.restore_payload_dir.clone(),
        }
    }
}

/// File-backed provider used by the standard `blazed` binary.
pub(crate) struct FileDataPlaneProvider {
    descriptor: ProviderDescriptor,
    storage: Arc<dyn StorageProvider>,
    leases: Mutex<HashMap<Uuid, FileLease>>,
    mutations: tokio::sync::Mutex<()>,
    initialization_error: Option<String>,
}

impl FileDataPlaneProvider {
    /// Wrap the existing file storage implementation in the lifecycle contract.
    pub(crate) fn new(storage: Arc<dyn StorageProvider>) -> Self {
        let (provider_instance_id, initialization_error) = match storage.ownership_domain_id() {
            Ok(Some(identity)) if !identity.is_nil() => (identity, None),
            Ok(Some(_)) => (
                FILE_PROVIDER_INSTANCE_ID,
                Some("storage ownership domain returned a nil identity".to_string()),
            ),
            Ok(None) => (FILE_PROVIDER_INSTANCE_ID, None),
            Err(error) => (FILE_PROVIDER_INSTANCE_ID, Some(error.to_string())),
        };
        Self {
            descriptor: ProviderDescriptor {
                contract_version: PROVIDER_CONTRACT_VERSION,
                provider_instance_id,
            },
            storage,
            leases: Mutex::new(HashMap::new()),
            mutations: tokio::sync::Mutex::new(()),
            initialization_error,
        }
    }

    fn ensure_initialized(&self) -> Result<(), ProviderError> {
        if let Some(error) = &self.initialization_error {
            tracing::error!(%error, "file provider storage domain initialization failed");
            Err(ProviderError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn leases(&self) -> Result<MutexGuard<'_, HashMap<Uuid, FileLease>>, ProviderError> {
        self.leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)
    }

    fn forget_cached_lease(
        &self,
        context: blaze_provider_api::RequestContext,
    ) -> Result<(), ProviderError> {
        let mut leases = self.leases()?;
        if leases
            .get(&context.lease_id)
            .is_some_and(|lease| lease.binding.context == context)
        {
            leases.remove(&context.lease_id);
        }
        Ok(())
    }

    fn existing_prepare(
        &self,
        request: &PrepareRequest,
    ) -> Result<Option<PreparedLease>, ProviderError> {
        let leases = self.leases()?;
        let Some(lease) = leases.get(&request.context.lease_id) else {
            return Ok(None);
        };
        let expected_ownership = self.ownership_request(request);
        if lease.binding.context != request.context
            || lease.binding.state != LeaseState::Prepared
            || lease.ownership.request != expected_ownership
        {
            return Err(ProviderError::Conflict);
        }
        Ok(Some(PreparedLease {
            binding: lease.binding,
            resources: lease.resources(),
        }))
    }

    fn ownership_key(&self, context: blaze_provider_api::RequestContext) -> StorageOwnershipKey {
        StorageOwnershipKey {
            provider_instance_id: self.descriptor.provider_instance_id,
            context: context.into(),
        }
    }

    fn ownership_request(&self, request: &PrepareRequest) -> StorageOwnershipRequest {
        let template_vmstate_bytes = match &request.source {
            PrepareSource::Image { .. } => None,
            PrepareSource::Template(source) => Some(source.storage.vmstate.size_bytes),
        };
        StorageOwnershipRequest {
            key: self.ownership_key(request.context),
            root_filesystem_bytes: request.root_filesystem_bytes,
            guest_memory_bytes: request.guest_memory_bytes,
            source_fingerprint: request.source.fingerprint(),
            template_vmstate_bytes,
        }
    }

    fn prepared_from_lease(
        &self,
        request: &PrepareRequest,
        lease: FileLease,
    ) -> Result<PreparedLease, ProviderError> {
        if lease.binding.context != request.context
            || lease.binding.state != LeaseState::Prepared
            || lease.ownership.request != self.ownership_request(request)
        {
            return Err(ProviderError::Conflict);
        }
        Ok(PreparedLease {
            binding: lease.binding,
            resources: lease.resources(),
        })
    }

    fn binding_from_ownership(&self, ownership: StorageOwnershipClaim) -> LeaseBinding {
        LeaseBinding {
            provider_instance_id: ownership.request.key.provider_instance_id,
            context: ownership.request.key.context.into(),
            generation: ownership.generation,
            state: ownership.state.into(),
        }
    }

    async fn reconstruct_binding(
        &self,
        binding: LeaseBinding,
    ) -> Result<Option<FileLease>, ProviderError> {
        if binding.provider_instance_id != self.descriptor.provider_instance_id
            || binding.context.instance_id.is_nil()
            || binding.context.request_id.is_nil()
            || binding.context.operation_id.is_nil()
            || binding.context.lease_id.is_nil()
            || binding.context.generation == 0
            || binding.generation < binding.context.generation
        {
            return Err(ProviderError::Conflict);
        }
        let Some(recovered) = self.reconstruct_context(binding.context).await? else {
            return Ok(None);
        };
        if recovered.binding == binding {
            Ok(Some(recovered))
        } else {
            Err(ProviderError::Conflict)
        }
    }

    async fn reconstruct_context(
        &self,
        context: blaze_provider_api::RequestContext,
    ) -> Result<Option<FileLease>, ProviderError> {
        self.ensure_initialized()?;
        {
            let leases = self.leases()?;
            if let Some(lease) = leases.get(&context.lease_id) {
                return if lease.binding.context == context {
                    Ok(Some(lease.clone()))
                } else {
                    Err(ProviderError::Conflict)
                };
            }
            if leases
                .values()
                .any(|lease| lease.binding.context.instance_id == context.instance_id)
            {
                return Err(ProviderError::Conflict);
            }
        }
        let owned = match self
            .storage
            .lookup_ownership(self.ownership_key(context))
            .await
            .map_err(file_storage_reconstruction_error)?
        {
            StorageOwnershipLookup::Absent => return Ok(None),
            StorageOwnershipLookup::Conflict => return Err(ProviderError::Conflict),
            StorageOwnershipLookup::Owned(owned) => *owned,
        };
        let binding = self.binding_from_ownership(owned.ownership);
        if binding.context != context
            || binding.provider_instance_id != self.descriptor.provider_instance_id
        {
            return Err(ProviderError::Conflict);
        }
        let restore_payload_dir = owned
            .ownership
            .request
            .template_vmstate_bytes
            .map(|_| owned.storage.instance_dir.join("backend"));
        let recovered = FileLease {
            binding,
            storage: owned.storage,
            ownership: owned.ownership,
            restore_payload_dir,
        };
        let mut leases = self.leases()?;
        if let Some(existing) = leases
            .values()
            .find(|lease| lease.binding.context.instance_id == context.instance_id)
        {
            return if existing.binding == binding {
                Ok(Some(existing.clone()))
            } else {
                Err(ProviderError::Conflict)
            };
        }
        match leases.entry(context.lease_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(recovered.clone());
                Ok(Some(recovered))
            }
            std::collections::hash_map::Entry::Occupied(entry)
                if entry.get().binding == binding =>
            {
                Ok(Some(entry.get().clone()))
            }
            std::collections::hash_map::Entry::Occupied(_) => Err(ProviderError::Conflict),
        }
    }

    async fn advance(
        &self,
        binding: LeaseBinding,
        expected: LeaseState,
        next: LeaseState,
    ) -> Result<LeaseBinding, ProviderError> {
        let next_generation = binding
            .generation
            .checked_add(1)
            .ok_or(ProviderError::Conflict)?;
        let ownership = match self
            .storage
            .advance_ownership(
                self.ownership_key(binding.context),
                DataPlaneLeaseState::from(expected),
                binding.generation,
                DataPlaneLeaseState::from(next),
                next_generation,
            )
            .await
        {
            Ok(ownership) => ownership,
            Err(error) => {
                self.forget_cached_lease(binding.context)?;
                return Err(file_storage_reconstruction_error(error));
            }
        };
        let mut leases = self.leases()?;
        let lease = leases
            .get_mut(&binding.context.lease_id)
            .ok_or(ProviderError::Conflict)?;
        if lease.binding != binding || binding.state != expected {
            return Err(ProviderError::Conflict);
        }
        lease.binding.generation = next_generation;
        lease.binding.state = next;
        lease.ownership = ownership;
        Ok(lease.binding)
    }

    async fn retain_failed_acquire(
        &self,
        binding: LeaseBinding,
        source: blaze_core::BlazeError,
        residual: Option<StorageSlot>,
    ) -> ProviderError {
        let instance_id = binding.context.instance_id;
        if let Some(storage) = residual {
            tracing::warn!(error = %source, instance_id = %storage.id, "file provider preparation left a partial slot; removing it through the durable owner");
        } else {
            tracing::warn!(error = %source, %instance_id, "file provider preparation failed before a complete slot was produced");
        }
        match self
            .storage
            .release_owned(
                self.ownership_key(binding.context),
                DataPlaneLeaseState::Prepared,
                binding.generation,
            )
            .await
        {
            Ok(_) => ProviderError::Unavailable,
            Err(cleanup) => {
                tracing::error!(error = %cleanup, %instance_id, "file provider preparation cleanup remains incomplete");
                ProviderError::OutcomeUnknown
            }
        }
    }

    async fn release_binding(
        &self,
        binding: LeaseBinding,
        expected: &[LeaseState],
    ) -> Result<LeaseBinding, ProviderError> {
        if !expected.contains(&binding.state) {
            return Err(ProviderError::Conflict);
        }
        let released = released_binding(binding)?;
        let Some(lease) = self.reconstruct_binding(binding).await? else {
            return Ok(released);
        };
        let removed = match self
            .storage
            .release_owned(
                lease.ownership.request.key,
                DataPlaneLeaseState::from(binding.state),
                binding.generation,
            )
            .await
        {
            Ok(removed) => removed,
            Err(error) => {
                tracing::error!(%error, "file provider release remains incomplete");
                self.forget_cached_lease(binding.context)?;
                return Err(ProviderError::OutcomeUnknown);
            }
        };
        if !removed {
            tracing::warn!(
                instance_id = %binding.context.instance_id,
                "file provider storage was already absent during release"
            );
        }
        let mut leases = self.leases()?;
        let current = leases
            .remove(&binding.context.lease_id)
            .ok_or(ProviderError::OutcomeUnknown)?;
        if current.binding != binding {
            leases.insert(binding.context.lease_id, current);
            return Err(ProviderError::Conflict);
        }
        Ok(released)
    }
}

fn file_storage_reconstruction_error(error: BlazeError) -> ProviderError {
    tracing::error!(%error, "file provider could not verify durable storage ownership");
    ProviderError::OutcomeUnknown
}

fn released_binding(binding: LeaseBinding) -> Result<LeaseBinding, ProviderError> {
    Ok(LeaseBinding {
        generation: binding
            .generation
            .checked_add(1)
            .ok_or(ProviderError::Conflict)?,
        state: LeaseState::Released,
        ..binding
    })
}

#[async_trait]
impl DataPlaneProvider for FileDataPlaneProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            images: true,
            templates: self.storage.supports_templates(),
            opened_template_restore_resources: false,
            opened_checkpoint_restore_resources: false,
            opened_suspension_restore_resources: false,
            daemon_managed_storage: true,
        }
    }

    async fn probe(&self) -> Result<(), ProviderError> {
        self.ensure_initialized()?;
        match self.storage.probe().await {
            Ok(true) => Ok(()),
            Ok(false) => Err(ProviderError::Unavailable),
            Err(error) => {
                tracing::warn!(%error, "file provider probe failed");
                Err(ProviderError::Unavailable)
            }
        }
    }

    async fn prepare(&self, request: PrepareRequest) -> Result<PreparedLease, ProviderError> {
        self.ensure_initialized()?;
        if let Some(existing) = self.existing_prepare(&request)? {
            return Ok(existing);
        }
        if request.context.instance_id.is_nil()
            || request.context.request_id.is_nil()
            || request.context.operation_id.is_nil()
            || request.context.lease_id.is_nil()
            || request.context.generation == 0
            || request.context.generation.checked_add(4).is_none()
            || request.root_filesystem_bytes == 0
            || request.guest_memory_bytes == 0
            || matches!(
                &request.source,
                PrepareSource::Template(source) if source.storage.vmstate.size_bytes == 0
            )
        {
            return Err(ProviderError::InvalidResponse);
        }
        let _mutation = self.mutations.lock().await;
        if let Some(existing) = self.existing_prepare(&request)? {
            return Ok(existing);
        }
        if let Some(recovered) = self.reconstruct_context(request.context).await? {
            return self.prepared_from_lease(&request, recovered);
        }
        let binding = LeaseBinding {
            provider_instance_id: self.descriptor.provider_instance_id,
            context: request.context,
            generation: request.context.generation,
            state: LeaseState::Prepared,
        };
        let opts = AcquireOpts {
            instance_id: request.context.instance_id.to_string(),
            rootfs_size: request.root_filesystem_bytes,
            mem_size: request.guest_memory_bytes,
        };
        let ownership_request = self.ownership_request(&request);
        let reservation = self
            .storage
            .reserve_ownership(ownership_request)
            .await
            .map_err(|error| {
                tracing::error!(
                    %error,
                    instance_id = %request.context.instance_id,
                    "file provider could not establish write-ahead storage ownership"
                );
                ProviderError::OutcomeUnknown
            })?;
        if reservation.phase == StorageOwnershipPhase::Ready {
            let lease = self
                .reconstruct_context(request.context)
                .await?
                .ok_or(ProviderError::OutcomeUnknown)?;
            return self.prepared_from_lease(&request, lease);
        }
        if reservation.phase != StorageOwnershipPhase::Preparing
            || reservation.state != DataPlaneLeaseState::Prepared
            || reservation.generation != binding.generation
        {
            return Err(ProviderError::OutcomeUnknown);
        }
        let (storage, restore_payload_dir) = match request.source {
            PrepareSource::Image { .. } => match self.storage.acquire(&opts).await {
                Ok(storage) => (storage, None),
                Err(error) => {
                    let (source, residual) = error.into_parts();
                    return Err(self.retain_failed_acquire(binding, source, residual).await);
                }
            },
            PrepareSource::Template(source) => {
                if !self.storage.supports_templates() {
                    return Err(ProviderError::Unsupported);
                }
                match self.storage.acquire_template(&opts, source.storage).await {
                    Ok(materialized) => (materialized.storage, Some(materialized.payload_dir)),
                    Err(error) => {
                        let (source, residual) = error.into_parts();
                        return Err(self.retain_failed_acquire(binding, source, residual).await);
                    }
                }
            }
        };
        let ownership = self
            .storage
            .publish_ownership(&storage, ownership_request)
            .await
            .map_err(|error| {
                tracing::error!(
                    %error,
                    instance_id = %request.context.instance_id,
                    "file provider could not durably publish storage ownership"
                );
                ProviderError::OutcomeUnknown
            })?;
        let lease = FileLease {
            binding,
            storage,
            ownership,
            restore_payload_dir,
        };
        let resources = lease.resources();
        let collision = {
            let mut leases = self.leases()?;
            if let std::collections::hash_map::Entry::Vacant(entry) =
                leases.entry(request.context.lease_id)
            {
                entry.insert(lease.clone());
                false
            } else {
                true
            }
        };
        if collision {
            tracing::error!(
                instance_id = %request.context.instance_id,
                "file provider found an in-memory lease collision after serialized preparation"
            );
            return Err(ProviderError::OutcomeUnknown);
        }
        Ok(PreparedLease { binding, resources })
    }

    async fn inspect(
        &self,
        request: InspectRequest,
    ) -> Result<blaze_provider_api::ObservedLease, ProviderError> {
        let _mutation = self.mutations.lock().await;
        let lease = self
            .reconstruct_context(request.context)
            .await?
            .ok_or(ProviderError::NotFound)?;
        Ok(blaze_provider_api::ObservedLease {
            binding: lease.binding,
        })
    }

    async fn commit(&self, request: CommitRequest) -> Result<CommittedLease, ProviderError> {
        let _mutation = self.mutations.lock().await;
        if request.binding.state != LeaseState::Prepared {
            return Err(ProviderError::Conflict);
        }
        self.reconstruct_binding(request.binding)
            .await?
            .ok_or(ProviderError::NotFound)?;
        Ok(CommittedLease {
            binding: self
                .advance(request.binding, LeaseState::Prepared, LeaseState::Committed)
                .await?,
        })
    }

    async fn finalize(&self, request: FinalizeRequest) -> Result<FinalizedLease, ProviderError> {
        let _mutation = self.mutations.lock().await;
        if request.binding.state != LeaseState::Committed
            || request.public_transition.instance_id != request.binding.context.instance_id
            || request.public_transition.operation_id != request.binding.context.operation_id
        {
            return Err(ProviderError::Conflict);
        }
        self.reconstruct_binding(request.binding)
            .await?
            .ok_or(ProviderError::NotFound)?;
        Ok(FinalizedLease {
            binding: self
                .advance(
                    request.binding,
                    LeaseState::Committed,
                    LeaseState::Finalized,
                )
                .await?,
        })
    }

    async fn abort(&self, request: AbortRequest) -> Result<AbortResult, ProviderError> {
        let _mutation = self.mutations.lock().await;
        Ok(AbortResult {
            binding: self
                .release_binding(
                    request.binding,
                    &[LeaseState::Prepared, LeaseState::Committed],
                )
                .await?,
        })
    }

    async fn stop(&self, request: StopRequest) -> Result<StoppedLease, ProviderError> {
        let _mutation = self.mutations.lock().await;
        if request.binding.state != LeaseState::Finalized {
            return Err(ProviderError::Conflict);
        }
        self.reconstruct_binding(request.binding)
            .await?
            .ok_or(ProviderError::NotFound)?;
        Ok(StoppedLease {
            binding: self
                .advance(request.binding, LeaseState::Finalized, LeaseState::Stopped)
                .await?,
        })
    }

    async fn release(&self, request: ReleaseRequest) -> Result<ReleaseResult, ProviderError> {
        let _mutation = self.mutations.lock().await;
        Ok(ReleaseResult {
            binding: self
                .release_binding(request.binding, &[LeaseState::Stopped])
                .await?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use blaze_provider_api::{
        AbortRequest, CommitRequest, DataPlaneProvider, FinalizeRequest, InspectRequest,
        LeaseState, PrepareRequest, PrepareSource, PublicTransitionRef, ReleaseRequest,
        RequestContext, StopRequest,
    };
    use blaze_provider_conformance::exercise_create_delete;

    use crate::file_provider::FileStorageProvider;

    use super::*;

    fn request() -> PrepareRequest {
        PrepareRequest {
            context: RequestContext {
                instance_id: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                lease_id: Uuid::new_v4(),
                generation: 1,
            },
            source: PrepareSource::Image {
                image_digest: "sha256:test".to_string(),
            },
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 4096,
        }
    }

    fn ownership_manifest(
        instances: &std::path::Path,
        context: RequestContext,
    ) -> std::path::PathBuf {
        instances
            .join(".blaze-storage-ownership")
            .join(format!("{}.json", context.instance_id))
    }

    #[cfg(feature = "test-failpoints")]
    fn storage_ownership_request(
        provider_instance_id: Uuid,
        request: &PrepareRequest,
    ) -> StorageOwnershipRequest {
        StorageOwnershipRequest {
            key: StorageOwnershipKey {
                provider_instance_id,
                context: request.context.into(),
            },
            root_filesystem_bytes: request.root_filesystem_bytes,
            guest_memory_bytes: request.guest_memory_bytes,
            source_fingerprint: request.source.fingerprint(),
            template_vmstate_bytes: None,
        }
    }

    #[test]
    fn file_provider_identity_is_stable_across_reconstruction() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");

        let build = || {
            let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
                images.clone(),
                instances.clone(),
            ));
            FileDataPlaneProvider::new(storage).descriptor()
        };

        let first = build();
        let restarted = build();
        assert_eq!(restarted, first);
        assert!(!restarted.provider_instance_id.is_nil());

        let other_instances = temp.path().join("other-instances");
        std::fs::create_dir(&other_instances).expect("other instances");
        let other_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, other_instances));
        let other = FileDataPlaneProvider::new(other_storage).descriptor();
        assert_ne!(other.provider_instance_id, first.provider_instance_id);
    }

    #[tokio::test]
    async fn file_provider_rejects_an_old_binding_after_instances_root_changes() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let first_instances = temp.path().join("instances-a");
        let second_instances = temp.path().join("instances-b");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&first_instances).expect("first instances");
        std::fs::create_dir_all(&second_instances).expect("second instances");
        let first_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            first_instances.clone(),
        ));
        let first = FileDataPlaneProvider::new(first_storage);
        let request = request();
        let binding = first.prepare(request).await.expect("prepare").binding;
        let first_identity = first.descriptor().provider_instance_id;
        drop(first);

        let second_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images,
            second_instances.clone(),
        ));
        let second = FileDataPlaneProvider::new(second_storage);
        assert_ne!(second.descriptor().provider_instance_id, first_identity);
        assert_eq!(
            second.abort(AbortRequest { binding }).await,
            Err(ProviderError::Conflict)
        );
        assert!(
            first_instances
                .join(binding.context.instance_id.to_string())
                .is_dir()
        );
        assert!(
            !second_instances
                .join(binding.context.instance_id.to_string())
                .exists()
        );
    }

    #[tokio::test]
    async fn file_provider_prepares_inspects_and_aborts_one_lease() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            instances.clone(),
        ));
        let provider = FileDataPlaneProvider::new(storage);
        let request = request();
        let context = request.context;

        let prepared = provider.prepare(request).await.expect("prepare");
        assert_eq!(prepared.binding.state, LeaseState::Prepared);
        assert_eq!(
            provider
                .inspect(InspectRequest { context })
                .await
                .expect("inspect")
                .binding,
            prepared.binding
        );
        let committed = provider
            .commit(CommitRequest {
                binding: prepared.binding,
            })
            .await
            .expect("commit");
        let released = provider
            .abort(AbortRequest {
                binding: committed.binding,
            })
            .await
            .expect("abort");
        assert_eq!(released.binding.state, LeaseState::Released);
        assert_eq!(
            provider.inspect(InspectRequest { context }).await,
            Err(ProviderError::NotFound)
        );
    }

    #[tokio::test]
    async fn file_provider_rejects_generation_overflow_before_creating_or_deleting_storage() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let provider = FileDataPlaneProvider::new(storage);

        let mut invalid = request();
        invalid.context.generation = u64::MAX - 3;
        let invalid_context = invalid.context;
        assert!(matches!(
            provider.prepare(invalid).await,
            Err(ProviderError::InvalidResponse)
        ));
        assert!(
            !instances
                .join(invalid_context.instance_id.to_string())
                .exists()
        );
        assert!(!ownership_manifest(&instances, invalid_context).exists());

        let prepared = provider.prepare(request()).await.expect("prepare");
        let mut overflowing = prepared.binding;
        overflowing.generation = u64::MAX;
        assert_eq!(
            provider
                .abort(AbortRequest {
                    binding: overflowing
                })
                .await,
            Err(ProviderError::Conflict)
        );
        assert!(
            instances
                .join(prepared.binding.context.instance_id.to_string())
                .is_dir()
        );
        provider
            .abort(AbortRequest {
                binding: prepared.binding,
            })
            .await
            .expect("cleanup valid binding");
    }

    #[tokio::test]
    async fn file_provider_distinguishes_absence_from_context_collision() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            instances.clone(),
        ));
        let provider = FileDataPlaneProvider::new(storage);
        let prepare_request = request();
        let context = prepare_request.context;
        provider.prepare(prepare_request).await.expect("prepare");

        let wrong_lease = RequestContext {
            lease_id: Uuid::new_v4(),
            ..context
        };
        assert_eq!(
            provider
                .inspect(InspectRequest {
                    context: wrong_lease,
                })
                .await,
            Err(ProviderError::Conflict)
        );
        let collision = RequestContext {
            request_id: Uuid::new_v4(),
            ..context
        };
        assert_eq!(
            provider
                .inspect(InspectRequest { context: collision })
                .await,
            Err(ProviderError::Conflict)
        );
        let absent = request().context;
        assert_eq!(
            provider.inspect(InspectRequest { context: absent }).await,
            Err(ProviderError::NotFound)
        );
    }

    #[tokio::test]
    async fn file_provider_detects_a_lease_reused_for_another_instance_after_restart() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let first_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            instances.clone(),
        ));
        let first = FileDataPlaneProvider::new(first_storage);
        let first_request = request();
        let retained = first_request.context;
        first.prepare(first_request).await.expect("prepare");
        drop(first);

        let restarted_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let restarted = FileDataPlaneProvider::new(restarted_storage);
        let reused = RequestContext {
            instance_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: retained.lease_id,
            generation: retained.generation,
        };
        assert_eq!(
            restarted.inspect(InspectRequest { context: reused }).await,
            Err(ProviderError::Conflict)
        );
        let reused_prepare = PrepareRequest {
            context: reused,
            source: PrepareSource::Image {
                image_digest: "sha256:test".to_string(),
            },
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 4096,
        };
        assert!(matches!(
            restarted.prepare(reused_prepare).await,
            Err(ProviderError::Conflict)
        ));
        assert!(instances.join(retained.instance_id.to_string()).is_dir());
        assert!(!instances.join(reused.instance_id.to_string()).exists());
    }

    #[tokio::test]
    async fn file_provider_reconstructs_a_durable_lease_after_process_restart() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let first_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            instances.clone(),
        ));
        let first = FileDataPlaneProvider::new(first_storage);
        let request = request();
        let context = request.context;
        let prepared = first.prepare(request).await.expect("prepare");
        let committed = first
            .commit(CommitRequest {
                binding: prepared.binding,
            })
            .await
            .expect("commit");
        let finalized = first
            .finalize(FinalizeRequest {
                binding: committed.binding,
                public_transition: PublicTransitionRef {
                    instance_id: context.instance_id,
                    operation_id: context.operation_id,
                },
            })
            .await
            .expect("finalize");
        drop(first);

        let restarted_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let restarted = FileDataPlaneProvider::new(restarted_storage);
        let stopped = restarted
            .stop(StopRequest {
                binding: finalized.binding,
            })
            .await
            .expect("reconstruct and stop");
        let released = restarted
            .release(ReleaseRequest {
                binding: stopped.binding,
            })
            .await
            .expect("release");

        assert_eq!(released.binding.state, LeaseState::Released);
        assert!(!instances.join(context.instance_id.to_string()).exists());
    }

    #[tokio::test]
    async fn file_provider_fails_closed_for_incomplete_reconstructed_storage() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let absent_context = request().context;
        let absent = FileDataPlaneProvider::new(storage.clone());
        assert_eq!(
            absent
                .inspect(InspectRequest {
                    context: absent_context,
                })
                .await,
            Err(ProviderError::NotFound)
        );
        let context = request().context;
        let slot = storage
            .acquire(&AcquireOpts {
                instance_id: context.instance_id.to_string(),
                rootfs_size: 4096,
                mem_size: 4096,
            })
            .await
            .expect("storage side effect");
        std::fs::remove_file(slot.mem_diff_path).expect("remove one required artifact");
        let restarted = FileDataPlaneProvider::new(storage);

        assert_eq!(
            restarted.inspect(InspectRequest { context }).await,
            Err(ProviderError::OutcomeUnknown)
        );
        assert!(instances.join(context.instance_id.to_string()).is_dir());
    }

    #[tokio::test]
    async fn file_provider_does_not_claim_or_delete_an_unowned_directory_collision() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let request = request();
        let context = request.context;
        storage
            .acquire(&AcquireOpts {
                instance_id: context.instance_id.to_string(),
                rootfs_size: 4096,
                mem_size: 4096,
            })
            .await
            .expect("unowned colliding directory");
        let provider = FileDataPlaneProvider::new(storage);

        assert!(matches!(
            provider.prepare(request).await,
            Err(ProviderError::OutcomeUnknown)
        ));
        assert!(instances.join(context.instance_id.to_string()).is_dir());
        assert!(!ownership_manifest(&instances, context).exists());
    }

    #[tokio::test]
    async fn file_provider_rejects_a_different_context_after_restart() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            instances.clone(),
        ));
        let request = request();
        let context = request.context;
        FileDataPlaneProvider::new(storage)
            .prepare(request)
            .await
            .expect("prepare");
        let restarted_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let restarted = FileDataPlaneProvider::new(restarted_storage);
        let wrong = RequestContext {
            request_id: Uuid::new_v4(),
            ..context
        };

        assert_eq!(
            restarted.inspect(InspectRequest { context: wrong }).await,
            Err(ProviderError::Conflict)
        );
        assert!(instances.join(context.instance_id.to_string()).is_dir());
    }

    #[tokio::test]
    async fn file_provider_rejects_missing_and_corrupt_ownership_manifests() {
        for corrupt in [false, true] {
            let temp = tempfile::tempdir().expect("temp");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            std::fs::create_dir_all(&images).expect("images");
            std::fs::create_dir_all(&instances).expect("instances");
            let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
                images.clone(),
                instances.clone(),
            ));
            let request = request();
            let context = request.context;
            FileDataPlaneProvider::new(storage)
                .prepare(request)
                .await
                .expect("prepare");
            let manifest = ownership_manifest(&instances, context);
            if corrupt {
                std::fs::write(&manifest, b"{\"format\":1}\n").expect("corrupt manifest");
            } else {
                std::fs::remove_file(&manifest).expect("remove manifest");
            }

            let restarted_storage: Arc<dyn StorageProvider> =
                Arc::new(FileStorageProvider::with_images(images, instances.clone()));
            let restarted = FileDataPlaneProvider::new(restarted_storage);
            assert_eq!(
                restarted.inspect(InspectRequest { context }).await,
                Err(ProviderError::OutcomeUnknown)
            );
            assert!(instances.join(context.instance_id.to_string()).is_dir());
        }
    }

    #[tokio::test]
    async fn file_provider_rejects_a_manifest_copied_to_another_storage_domain() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let first_instances = temp.path().join("instances-a");
        let second_instances = temp.path().join("instances-b");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&first_instances).expect("first instances");
        std::fs::create_dir_all(&second_instances).expect("second instances");
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            first_instances.clone(),
        ));
        let request = request();
        let context = request.context;
        FileDataPlaneProvider::new(storage)
            .prepare(request)
            .await
            .expect("prepare");

        let source = first_instances.join(context.instance_id.to_string());
        let target = second_instances.join(context.instance_id.to_string());
        std::fs::create_dir(&target).expect("copied slot directory");
        for entry in std::fs::read_dir(&source).expect("source entries") {
            let entry = entry.expect("source entry");
            assert!(entry.file_type().expect("entry type").is_file());
            std::fs::copy(entry.path(), target.join(entry.file_name())).expect("copy slot entry");
        }
        let copied_ledger = second_instances.join(".blaze-storage-ownership");
        std::fs::create_dir(&copied_ledger).expect("copied ownership ledger");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&copied_ledger, std::fs::Permissions::from_mode(0o700))
                .expect("owner-only ledger permissions");
        }
        std::fs::copy(
            ownership_manifest(&first_instances, context),
            ownership_manifest(&second_instances, context),
        )
        .expect("copy ownership manifest");

        let moved_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images,
            second_instances.clone(),
        ));
        let moved = FileDataPlaneProvider::new(moved_storage);
        assert_eq!(
            moved.inspect(InspectRequest { context }).await,
            Err(ProviderError::OutcomeUnknown)
        );
        assert!(target.is_dir());
    }

    #[tokio::test]
    async fn file_provider_rejects_idempotent_prepare_with_changed_immutable_facts() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            instances.clone(),
        ));
        let provider = FileDataPlaneProvider::new(storage);
        let request = request();
        let context = request.context;
        provider.prepare(request).await.expect("prepare");

        let changed_source = PrepareRequest {
            context,
            source: PrepareSource::Image {
                image_digest: "sha256:different".to_string(),
            },
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 4096,
        };
        assert!(matches!(
            provider.prepare(changed_source).await,
            Err(ProviderError::Conflict)
        ));
        let changed_extent = PrepareRequest {
            context,
            source: PrepareSource::Image {
                image_digest: "sha256:test".to_string(),
            },
            root_filesystem_bytes: 8192,
            guest_memory_bytes: 4096,
        };
        assert!(matches!(
            provider.prepare(changed_extent).await,
            Err(ProviderError::Conflict)
        ));

        drop(provider);
        let restarted_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let restarted = FileDataPlaneProvider::new(restarted_storage);
        let changed_after_restart = PrepareRequest {
            context,
            source: PrepareSource::Image {
                image_digest: "sha256:different-after-restart".to_string(),
            },
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 4096,
        };
        assert!(matches!(
            restarted.prepare(changed_after_restart).await,
            Err(ProviderError::Conflict)
        ));
        assert!(instances.join(context.instance_id.to_string()).is_dir());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn ownership_ledger_recovers_both_reservation_publication_boundaries() {
        for failpoint_name in [
            "storage-ownership-before-reserve",
            "storage-ownership-after-reserve",
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            std::fs::create_dir_all(&images).expect("images");
            std::fs::create_dir_all(&instances).expect("instances");
            let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
                images.clone(),
                instances.clone(),
            ));
            let provider = FileDataPlaneProvider::new(storage);
            let request = request();
            let context = request.context;
            let failpoint = crate::failpoint::TestFailpoint::new(&[failpoint_name]);
            assert!(matches!(
                failpoint.run(provider.prepare(request)).await,
                Err(ProviderError::OutcomeUnknown)
            ));
            assert!(!instances.join(context.instance_id.to_string()).exists());
            assert_eq!(
                ownership_manifest(&instances, context).exists(),
                failpoint_name.ends_with("after-reserve")
            );
            drop(provider);

            let restarted_storage: Arc<dyn StorageProvider> =
                Arc::new(FileStorageProvider::with_images(images, instances.clone()));
            let restarted = FileDataPlaneProvider::new(restarted_storage);
            match restarted.inspect(InspectRequest { context }).await {
                Ok(observed) if failpoint_name.ends_with("after-reserve") => {
                    restarted
                        .abort(AbortRequest {
                            binding: observed.binding,
                        })
                        .await
                        .expect("remove write-ahead owner without a slot");
                }
                Err(ProviderError::NotFound) if failpoint_name.ends_with("before-reserve") => {}
                result => {
                    panic!("unexpected reservation recovery at {failpoint_name}: {result:?}")
                }
            }
            assert!(!ownership_manifest(&instances, context).exists());
        }
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn ownership_ledger_fails_closed_across_slot_identity_publication() {
        for failpoint_name in [
            "storage-acquire-before-mkdir",
            "storage-acquire-after-mkdir",
            "storage-ownership-before-slot-identity",
            "storage-ownership-after-slot-identity",
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            std::fs::create_dir_all(&images).expect("images");
            std::fs::create_dir_all(&instances).expect("instances");
            let request = request();
            let context = request.context;
            let opts = AcquireOpts {
                instance_id: context.instance_id.to_string(),
                rootfs_size: request.root_filesystem_bytes,
                mem_size: request.guest_memory_bytes,
            };
            let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
                images.clone(),
                instances.clone(),
            ));
            storage
                .reserve_ownership(storage_ownership_request(
                    storage
                        .ownership_domain_id()
                        .expect("storage domain")
                        .expect("file storage domain"),
                    &request,
                ))
                .await
                .expect("write-ahead owner");
            let failpoint = crate::failpoint::TestFailpoint::new(&[failpoint_name]);
            assert!(failpoint.run(storage.acquire(&opts)).await.is_err());
            let slot_exists = instances.join(context.instance_id.to_string()).exists();
            assert_eq!(
                slot_exists,
                failpoint_name != "storage-acquire-before-mkdir"
            );
            drop(storage);

            let restarted_storage: Arc<dyn StorageProvider> =
                Arc::new(FileStorageProvider::with_images(images, instances.clone()));
            let restarted = FileDataPlaneProvider::new(restarted_storage);
            match restarted.inspect(InspectRequest { context }).await {
                Err(ProviderError::OutcomeUnknown)
                    if matches!(
                        failpoint_name,
                        "storage-acquire-after-mkdir" | "storage-ownership-before-slot-identity"
                    ) =>
                {
                    assert!(instances.join(context.instance_id.to_string()).is_dir());
                    assert!(ownership_manifest(&instances, context).is_file());
                }
                Ok(observed) => {
                    restarted
                        .abort(AbortRequest {
                            binding: observed.binding,
                        })
                        .await
                        .expect("finish exactly identified allocation cleanup");
                    assert!(!instances.join(context.instance_id.to_string()).exists());
                    assert!(!ownership_manifest(&instances, context).exists());
                }
                result => {
                    panic!("unexpected slot identity recovery at {failpoint_name}: {result:?}")
                }
            }
        }
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn ownership_ledger_recovers_both_ready_publication_boundaries() {
        for failpoint_name in [
            "storage-ownership-before-ready",
            "storage-ownership-after-ready",
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            std::fs::create_dir_all(&images).expect("images");
            std::fs::create_dir_all(&instances).expect("instances");
            let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
                images.clone(),
                instances.clone(),
            ));
            let provider = FileDataPlaneProvider::new(storage);
            let request = request();
            let context = request.context;
            let failpoint = crate::failpoint::TestFailpoint::new(&[failpoint_name]);
            assert!(matches!(
                failpoint.run(provider.prepare(request)).await,
                Err(ProviderError::OutcomeUnknown)
            ));
            assert!(instances.join(context.instance_id.to_string()).is_dir());
            assert!(ownership_manifest(&instances, context).is_file());
            drop(provider);

            let restarted_storage: Arc<dyn StorageProvider> =
                Arc::new(FileStorageProvider::with_images(images, instances.clone()));
            let restarted = FileDataPlaneProvider::new(restarted_storage);
            let observed = restarted
                .inspect(InspectRequest { context })
                .await
                .expect("recover prepare across ready publication");
            restarted
                .abort(AbortRequest {
                    binding: observed.binding,
                })
                .await
                .expect("abort recovered prepare");
            assert!(!instances.join(context.instance_id.to_string()).exists());
            assert!(!ownership_manifest(&instances, context).exists());
        }
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn ownership_ledger_recovers_the_durable_lease_generation_after_response_loss() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            images.clone(),
            instances.clone(),
        ));
        let provider = FileDataPlaneProvider::new(storage);
        let request = request();
        let context = request.context;
        let prepared = provider.prepare(request).await.expect("prepare");
        let failpoint =
            crate::failpoint::TestFailpoint::new(&["storage-ownership-after-state-update"]);
        assert!(matches!(
            failpoint
                .run(provider.commit(CommitRequest {
                    binding: prepared.binding,
                }))
                .await,
            Err(ProviderError::OutcomeUnknown)
        ));
        let live_observed = provider
            .inspect(InspectRequest { context })
            .await
            .expect("inspect durable state after a lost response");
        assert_eq!(live_observed.binding.state, LeaseState::Committed);
        assert_eq!(
            live_observed.binding.generation,
            prepared.binding.generation + 1
        );
        drop(provider);

        let restarted_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances.clone()));
        let restarted = FileDataPlaneProvider::new(restarted_storage);
        let observed = restarted
            .inspect(InspectRequest { context })
            .await
            .expect("recover committed lease");
        assert_eq!(observed.binding.state, LeaseState::Committed);
        assert_eq!(observed.binding.generation, prepared.binding.generation + 1);
        restarted
            .abort(AbortRequest {
                binding: observed.binding,
            })
            .await
            .expect("abort recovered committed lease");
        assert!(!instances.join(context.instance_id.to_string()).exists());
        assert!(!ownership_manifest(&instances, context).exists());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn ownership_ledger_resumes_every_interrupted_removal_phase() {
        for failpoint_name in [
            "storage-release-before-mark-deleting",
            "storage-release-after-mark-deleting",
            "storage-release-during-slot-remove",
            "storage-release-after-slot-remove",
            "storage-release-after-ledger-remove",
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let images = temp.path().join("images");
            let instances = temp.path().join("instances");
            std::fs::create_dir_all(&images).expect("images");
            std::fs::create_dir_all(&instances).expect("instances");
            let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
                images.clone(),
                instances.clone(),
            ));
            let provider = FileDataPlaneProvider::new(storage);
            let request = request();
            let context = request.context;
            let prepared = provider.prepare(request).await.expect("prepare");
            let failpoint = crate::failpoint::TestFailpoint::new(&[failpoint_name]);
            assert!(matches!(
                failpoint
                    .run(provider.abort(AbortRequest {
                        binding: prepared.binding,
                    }))
                    .await,
                Err(ProviderError::OutcomeUnknown)
            ));
            drop(provider);

            let restarted_storage: Arc<dyn StorageProvider> =
                Arc::new(FileStorageProvider::with_images(images, instances.clone()));
            let restarted = FileDataPlaneProvider::new(restarted_storage);
            match restarted.inspect(InspectRequest { context }).await {
                Ok(observed) => {
                    assert_eq!(observed.binding, prepared.binding);
                    restarted
                        .abort(AbortRequest {
                            binding: observed.binding,
                        })
                        .await
                        .expect("resume interrupted removal");
                }
                Err(ProviderError::NotFound)
                    if failpoint_name == "storage-release-after-ledger-remove" => {}
                result => panic!("unexpected recovery result at {failpoint_name}: {result:?}"),
            }
            assert!(!instances.join(context.instance_id.to_string()).exists());
            assert!(!ownership_manifest(&instances, context).exists());
        }
    }

    #[tokio::test]
    async fn file_provider_passes_the_create_delete_contract() {
        let temp = tempfile::tempdir().expect("temp");
        let images = temp.path().join("images");
        let instances = temp.path().join("instances");
        std::fs::create_dir_all(&images).expect("images");
        std::fs::create_dir_all(&instances).expect("instances");
        let storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances));
        let provider = FileDataPlaneProvider::new(storage);

        exercise_create_delete(&provider, request())
            .await
            .expect("file provider lifecycle");
    }
}
