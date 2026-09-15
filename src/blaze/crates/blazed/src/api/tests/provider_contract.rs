// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::spawner::{
    BackendRestoreRequest, PinnedExecutable, ProviderAttachmentRole, RestoreResult,
};
use blaze_core::backend::{RestoreCapability, SnapshotRequest};
use blaze_provider_api::{AttachmentKind, AttachmentRole, OpenedAttachment};
use std::fs::File;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Mutex, Weak};

#[derive(Clone, Copy, Debug)]
pub(super) enum CaptureFault {
    Provider,
    PublicId,
    Reference,
    Lease,
    Generation,
    Parent,
    Digest,
}

impl CaptureFault {
    pub(super) fn apply(self, reference: &mut ProviderCheckpointRef) {
        match self {
            Self::Provider => reference.provider_instance_id = Uuid::new_v4(),
            Self::PublicId => reference.public_checkpoint_id = Uuid::new_v4(),
            Self::Reference => reference.reference_id = Uuid::nil(),
            Self::Lease => reference.source_lease_id = Uuid::new_v4(),
            Self::Generation => reference.source_generation += 1,
            Self::Parent => reference.parent_reference_id = Some(Uuid::new_v4()),
            Self::Digest => reference.content_digest = "sha256:invalid".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ReceiptFault {
    PublicId,
    Reference,
    MissingReference,
}

impl ReceiptFault {
    pub(super) fn identities(
        self,
        public_id: Uuid,
        reference: Option<Uuid>,
    ) -> (Uuid, Option<Uuid>) {
        match self {
            Self::PublicId => (Uuid::new_v4(), reference),
            Self::Reference => (public_id, Some(Uuid::new_v4())),
            Self::MissingReference => (public_id, None),
        }
    }
}

impl InventoryTestProvider {
    pub(super) fn opened_resources(
        &self,
        binding: LeaseBinding,
        root_bytes: u64,
        memory_bytes: u64,
    ) -> Vec<OpenedAttachment> {
        let mut identities = Vec::new();
        let mut attachments = Vec::new();
        for (role, bytes, consumer_path) in [
            (
                AttachmentRole::RootDrive,
                root_bytes,
                Some(PathBuf::from("rootfs.ext4")),
            ),
            (AttachmentRole::GuestMemory, memory_bytes, None),
        ] {
            // Unlinked files prove that the manager transfers descriptors rather than reopening paths.
            let file = tempfile::tempfile().expect("unlinked restore file");
            file.set_len(bytes).expect("restore extent");
            file.write_all_at(binding.context.lease_id.as_bytes(), 0)
                .expect("lease marker");
            let metadata = file.metadata().expect("opened file identity");
            identities.push((metadata.dev(), metadata.ino()));
            attachments.push(OpenedAttachment {
                role,
                descriptor: file.into(),
                kind: AttachmentKind::RegularFile,
                logical_size_bytes: bytes,
                consumer_path,
            });
        }
        self.opened_file_identities
            .lock()
            .expect("opened identities")
            .insert(binding.context.lease_id, identities);
        attachments
    }
}

#[derive(Debug)]
struct OpenedObservation {
    instance_id: Uuid,
    lease_id: Uuid,
    generation: u64,
    identities: Vec<(u64, u64)>,
}

#[derive(Default)]
struct OpenedRestoreSpawner {
    fail_restore: AtomicBool,
    seen: Mutex<Vec<OpenedObservation>>,
    files: Mutex<Vec<Weak<File>>>,
}

#[async_trait]
impl BackendSpawner for OpenedRestoreSpawner {
    async fn spawn(
        &self,
        request: BackendSpawnRequest,
    ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
        GuestMockSpawner.spawn(request).await
    }

    async fn probe(&self, path: &Path) -> blaze_core::Result<bool> {
        GuestMockSpawner.probe(path).await
    }

    async fn cleanup_orphan(&self, id: Uuid, run_dir: &OwnedRunDir) -> blaze_core::Result<()> {
        GuestMockSpawner.cleanup_orphan(id, run_dir).await
    }

    async fn restore_capability(
        &self,
        executable: Option<&PinnedExecutable>,
    ) -> blaze_core::Result<Option<RestoreCapability>> {
        let mut capability = GuestMockSpawner
            .restore_capability(executable)
            .await?
            .expect("guest restore capability");
        capability.consumes_typed_opened_attachments = true;
        Ok(Some(capability))
    }

    async fn restore(&self, mut request: BackendRestoreRequest) -> RestoreResult {
        let Some(resources) = request.provider_attachments.take() else {
            return GuestMockSpawner.restore(request).await;
        };
        resources
            .validate_shape()
            .expect("manager attachment contract");
        assert_eq!(resources.instance_id, request.instance_id);
        assert_eq!(resources.attachments.len(), 2);
        let mut identities = Vec::new();
        let mut files = Vec::new();
        for (attachment, role) in resources.attachments.into_iter().zip([
            ProviderAttachmentRole::RootDrive,
            ProviderAttachmentRole::GuestMemory,
        ]) {
            assert_eq!(attachment.role, role);
            let metadata = attachment.file.metadata().expect("received file identity");
            assert_eq!(metadata.len(), attachment.logical_size_bytes);
            let mut marker = [0; 16];
            attachment
                .file
                .read_exact_at(&mut marker, 0)
                .expect("read through descriptor");
            assert_eq!(&marker, resources.lease_id.as_bytes());
            identities.push((metadata.dev(), metadata.ino()));
            self.files
                .lock()
                .expect("descriptor witnesses")
                .push(Arc::downgrade(&attachment.file));
            files.push(attachment.file);
        }
        self.seen
            .lock()
            .expect("restore observations")
            .push(OpenedObservation {
                instance_id: resources.instance_id,
                lease_id: resources.lease_id,
                generation: resources.generation,
                identities,
            });
        if self.fail_restore.load(Ordering::Acquire) {
            return Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "test backend rejected restore after consuming attachments".to_string(),
            }));
        }
        let inner = GuestMockSpawner.restore(request).await?;
        Ok(Arc::new(OpenedOwner {
            inner,
            files: Mutex::new(files),
        }))
    }
}

struct OpenedOwner {
    inner: DynBackendInstance,
    files: Mutex<Vec<Arc<File>>>,
}

#[async_trait]
impl BackendInstance for OpenedOwner {
    fn instance_id(&self) -> Uuid {
        self.inner.instance_id()
    }
    fn backend(&self) -> BackendKind {
        self.inner.backend()
    }
    fn version(&self) -> Option<&str> {
        self.inner.version()
    }
    fn supports_checkpoint_capture(&self) -> bool {
        self.inner.supports_checkpoint_capture()
    }
    fn guest_socket_path(&self) -> &Path {
        self.inner.guest_socket_path()
    }
    async fn try_wait(&self) -> blaze_core::Result<Option<SpawnResult>> {
        self.inner.try_wait().await
    }
    async fn pause(&self) -> blaze_core::Result<()> {
        self.inner.pause().await
    }
    async fn resume(&self) -> blaze_core::Result<()> {
        self.inner.resume().await
    }
    async fn snapshot(&self, request: SnapshotRequest) -> blaze_core::Result<()> {
        self.inner.snapshot(request).await
    }
    async fn quiesce_for_capture(&self) -> blaze_core::Result<()> {
        self.inner.quiesce_for_capture().await
    }
    async fn unquiesce_after_capture(&self) -> blaze_core::Result<()> {
        self.inner.unquiesce_after_capture().await
    }
    async fn kill(&self) -> blaze_core::Result<()> {
        self.inner.kill().await?;
        self.files.lock().expect("backend descriptors").clear();
        Ok(())
    }
}

fn lifecycle_fixture(
    temp: &tempfile::TempDir,
) -> (
    Arc<ServerState>,
    Arc<InventoryTestProvider>,
    Arc<OpenedRestoreSpawner>,
) {
    let config = test_config(temp);
    let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        config.storage.images_dir.clone(),
        config.storage.instances_dir.clone(),
    ));
    let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
    let backend = Arc::new(OpenedRestoreSpawner::default());
    let state = build_test_state_with_provider(
        config,
        test_policy(BackendKind::Mock),
        spawners(BackendKind::Mock, backend.clone()),
        BackendKind::Mock,
        storage,
        provider.clone(),
    );
    (state, provider, backend)
}

async fn create_id(state: &Arc<ServerState>) -> Uuid {
    let created = created_json(state, &test_request()).await;
    Uuid::parse_str(created["instance"]["id"].as_str().expect("instance ID"))
        .expect("instance UUID")
}

fn assert_descriptor_transfer(
    provider: &InventoryTestProvider,
    backend: &OpenedRestoreSpawner,
    id: Uuid,
) -> Uuid {
    let seen = backend.seen.lock().expect("restore observations");
    let last = seen.last().expect("backend must receive descriptors");
    assert_eq!(last.instance_id, id);
    assert_eq!(last.generation, 1);
    assert_eq!(
        provider
            .opened_file_identities
            .lock()
            .expect("prepared file identities")
            .get(&last.lease_id),
        Some(&last.identities)
    );
    last.lease_id
}

fn assert_no_opened_handles(backend: &OpenedRestoreSpawner) {
    assert!(
        backend
            .files
            .lock()
            .expect("descriptor witnesses")
            .iter()
            .all(|file| file.upgrade().is_none()),
        "stopped or failed backend must drop every descriptor"
    );
}

#[tokio::test]
async fn opened_checkpoint_restore_transfers_exact_files_and_finalizes_replacement() {
    let temp = tempfile::tempdir().expect("test directory");
    let (state, provider, backend) = lifecycle_fixture(&temp);
    let id = create_id(&state).await;
    let checkpoint = state.manager.checkpoint(id).await.expect("capture");
    let old = state
        .manager
        .get(id)
        .expect("instance")
        .data_plane_lease
        .expect("old lease")
        .lease_id;
    provider.advertise_opened_checkpoint_restore_resources();
    let restored = state
        .manager
        .restore(
            id,
            RestoreSandbox {
                checkpoint_id: checkpoint.id,
                binary_path: PathBuf::new(),
            },
        )
        .await
        .expect("opened checkpoint restore");
    let lease = assert_descriptor_transfer(&provider, &backend, id);
    assert_ne!(lease, old);
    assert_eq!(
        restored
            .instance
            .data_plane_lease
            .expect("replacement")
            .lease_id,
        lease
    );
    assert_eq!(
        provider.binding(lease).expect("finalized binding").state,
        LeaseState::Finalized
    );
    assert!(provider.binding(old).is_none());
    assert!(restored.instance.replacement_data_plane_lease.is_none());
    assert_eq!(restored.instance.state, SandboxState::Running);
    assert!(
        backend
            .files
            .lock()
            .expect("live descriptors")
            .iter()
            .all(|file| file.upgrade().is_some())
    );
    assert!(state.manager.destroy(id).await.expect("destroy"));
    assert!(provider.binding(lease).is_none());
    assert_no_opened_handles(&backend);
}

#[tokio::test]
async fn opened_checkpoint_restore_failure_aborts_replacement_and_retains_old_owner_record() {
    let temp = tempfile::tempdir().expect("test directory");
    let (state, provider, backend) = lifecycle_fixture(&temp);
    let id = create_id(&state).await;
    let checkpoint = state.manager.checkpoint(id).await.expect("capture");
    let old = state
        .manager
        .get(id)
        .expect("instance")
        .data_plane_lease
        .expect("old lease")
        .lease_id;
    provider.advertise_opened_checkpoint_restore_resources();
    backend.fail_restore.store(true, Ordering::Release);
    state
        .manager
        .restore(
            id,
            RestoreSandbox {
                checkpoint_id: checkpoint.id,
                binary_path: PathBuf::new(),
            },
        )
        .await
        .expect_err("backend restore failure");
    let replacement = assert_descriptor_transfer(&provider, &backend, id);
    assert!(provider.binding(replacement).is_none());
    let retained = state.manager.get(id).expect("recovery record");
    assert_eq!(retained.state, SandboxState::RecoveryRequired);
    assert!(retained.replacement_data_plane_lease.is_none());
    assert_eq!(
        retained
            .data_plane_lease
            .expect("stopped old lease")
            .lease_id,
        old
    );
    assert_eq!(
        provider.binding(old).expect("retained old lease").state,
        LeaseState::Stopped
    );
    assert!(state.manager.backend_owner(id).is_none());
    assert_no_opened_handles(&backend);
    assert!(state.manager.destroy(id).await.expect("explicit cleanup"));
    assert!(provider.binding(old).is_none());
}

#[tokio::test]
async fn opened_suspension_resume_transfers_files_and_retires_content_on_delete() {
    let temp = tempfile::tempdir().expect("test directory");
    let (state, provider, backend) = lifecycle_fixture(&temp);
    let id = create_id(&state).await;
    let old = state
        .manager
        .get(id)
        .expect("instance")
        .data_plane_lease
        .expect("old lease")
        .lease_id;
    state
        .manager
        .hibernate(
            id,
            HibernateSandbox {
                binary_path: PathBuf::new(),
            },
        )
        .await
        .expect("hibernate");
    assert!(provider.binding(old).is_none());
    provider.advertise_opened_suspension_restore_resources();
    let resumed = state
        .manager
        .resume(
            id,
            ResumeSandbox {
                binary_path: PathBuf::new(),
            },
        )
        .await
        .expect("opened resume");
    let lease = assert_descriptor_transfer(&provider, &backend, id);
    assert_ne!(lease, old);
    assert_eq!(resumed.id, id);
    assert_eq!(resumed.state, SandboxState::Running);
    assert_eq!(
        resumed.data_plane_lease.expect("replacement").lease_id,
        lease
    );
    assert_eq!(
        provider.binding(lease).expect("binding").state,
        LeaseState::Finalized
    );
    assert!(resumed.replacement_data_plane_lease.is_none());
    assert_eq!(provider.suspension_count(), 1);
    assert!(state.manager.destroy(id).await.expect("destroy"));
    assert_eq!(provider.suspension_count(), 0);
    assert!(provider.binding(lease).is_none());
    assert_no_opened_handles(&backend);
}

#[tokio::test]
async fn opened_suspension_resume_failure_preserves_content_for_a_fresh_retry() {
    let temp = tempfile::tempdir().expect("test directory");
    let (state, provider, backend) = lifecycle_fixture(&temp);
    let id = create_id(&state).await;
    let hibernated = state
        .manager
        .hibernate(
            id,
            HibernateSandbox {
                binary_path: PathBuf::new(),
            },
        )
        .await
        .expect("hibernate");
    provider.advertise_opened_suspension_restore_resources();
    backend.fail_restore.store(true, Ordering::Release);
    state
        .manager
        .resume(
            id,
            ResumeSandbox {
                binary_path: PathBuf::new(),
            },
        )
        .await
        .expect_err("backend restore failure");
    let failed = assert_descriptor_transfer(&provider, &backend, id);
    assert!(provider.binding(failed).is_none());
    let retained = state.manager.get(id).expect("retained suspension");
    assert_eq!(retained.state, SandboxState::Hibernated);
    assert_eq!(retained.provider_suspension, hibernated.provider_suspension);
    assert!(retained.data_plane_lease.is_none());
    assert!(retained.replacement_data_plane_lease.is_none());
    assert!(state.manager.backend_owner(id).is_none());
    assert_eq!(provider.suspension_count(), 1);
    assert_no_opened_handles(&backend);
    backend.fail_restore.store(false, Ordering::Release);
    let resumed = state
        .manager
        .resume(
            id,
            ResumeSandbox {
                binary_path: PathBuf::new(),
            },
        )
        .await
        .expect("retry opened resume");
    let replacement = assert_descriptor_transfer(&provider, &backend, id);
    assert_ne!(replacement, failed);
    assert_eq!(resumed.state, SandboxState::Running);
    assert_eq!(
        provider.binding(replacement).expect("retry binding").state,
        LeaseState::Finalized
    );
    assert!(state.manager.destroy(id).await.expect("destroy"));
    assert_no_opened_handles(&backend);
}

#[tokio::test]
async fn invalid_checkpoint_success_is_not_published_and_is_compensated() {
    for fault in [
        CaptureFault::Provider,
        CaptureFault::PublicId,
        CaptureFault::Reference,
        CaptureFault::Lease,
        CaptureFault::Generation,
        CaptureFault::Parent,
        CaptureFault::Digest,
    ] {
        let temp = tempfile::tempdir().expect("test directory");
        let (state, provider, _) = lifecycle_fixture(&temp);
        let id = create_id(&state).await;
        *provider.capture_fault.lock().expect("capture fault") = Some(fault);
        let error = state
            .manager
            .checkpoint(id)
            .await
            .expect_err("invalid success receipt");
        assert!(
            matches!(
                error,
                BlazeDaemonError::DataPlane(ProviderError::InvalidResponse)
            ),
            "{fault:?}: {error}"
        );
        assert!(
            state
                .manager
                .list_checkpoints(id)
                .await
                .expect("catalog")
                .is_empty()
        );
        assert_eq!(
            provider.checkpoint_count(),
            0,
            "unpublished capture must be retired by the recorded request identity"
        );
        let retained = state.manager.get(id).expect("compensated instance");
        assert_eq!(retained.state, SandboxState::Running);
        assert!(retained.operation.is_none());
        assert!(retained.pending_provider_retirements.is_empty());
        let lease = retained.data_plane_lease.expect("retained lease");
        assert_eq!(
            provider
                .binding(lease.lease_id)
                .expect("observed binding")
                .generation,
            lease.generation
        );
        *provider.capture_fault.lock().expect("capture fault") = None;
        state
            .manager
            .checkpoint(id)
            .await
            .expect("capture after compensation");
        assert!(state.manager.destroy(id).await.expect("destroy"));
    }
}

#[tokio::test]
async fn invalid_checkpoint_retirement_receipt_retains_pending_cleanup() {
    for fault in [
        ReceiptFault::PublicId,
        ReceiptFault::Reference,
        ReceiptFault::MissingReference,
    ] {
        let temp = tempfile::tempdir().expect("test directory");
        let (state, provider, _) = lifecycle_fixture(&temp);
        let id = create_id(&state).await;
        let root = state.manager.checkpoint(id).await.expect("root capture");
        let child = state.manager.checkpoint(id).await.expect("child capture");
        let reference = provider
            .checkpoints
            .lock()
            .expect("captured references")
            .get(
                &blaze_core::checkpoint::validate_checkpoint_id(&child.id)
                    .expect("checkpoint UUID"),
            )
            .expect("child reference")
            .clone();
        state
            .manager
            .restore(
                id,
                RestoreSandbox {
                    checkpoint_id: root.id,
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("restore root");
        *provider
            .checkpoint_retirement_fault
            .lock()
            .expect("retirement fault") = Some(fault);
        state
            .manager
            .prune_checkpoints(id)
            .await
            .expect_err("invalid retirement receipt");
        let retained = state.manager.get(id).expect("pending cleanup");
        assert!(
            retained
                .pending_provider_retirements
                .contains(&reference.to_record()),
            "{fault:?}"
        );
        assert_eq!(provider.checkpoint_count(), 2);
        assert!(
            state
                .manager
                .list_checkpoints(id)
                .await
                .expect("catalog")
                .iter()
                .all(|entry| entry.id != child.id)
        );
        let persisted = SandboxInstance::load(&configured_state_dir(&state), id)
            .expect("durable pending cleanup");
        assert!(
            persisted
                .pending_provider_retirements
                .contains(&reference.to_record())
        );
        *provider
            .checkpoint_retirement_fault
            .lock()
            .expect("retirement fault") = None;
        assert!(state.manager.destroy(id).await.expect("retry cleanup"));
        assert_eq!(provider.checkpoint_count(), 0);
    }
}

#[tokio::test]
async fn invalid_suspension_retirement_receipt_retains_pending_cleanup() {
    for fault in [
        ReceiptFault::PublicId,
        ReceiptFault::Reference,
        ReceiptFault::MissingReference,
    ] {
        let temp = tempfile::tempdir().expect("test directory");
        let (state, provider, _) = lifecycle_fixture(&temp);
        let id = create_id(&state).await;
        let hibernated = state
            .manager
            .hibernate(
                id,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("hibernate");
        let reference = hibernated
            .provider_suspension
            .expect("suspension reference");
        *provider
            .suspension_retirement_fault
            .lock()
            .expect("retirement fault") = Some(fault);
        state
            .manager
            .destroy(id)
            .await
            .expect_err("invalid retirement receipt");
        let retained = state.manager.get(id).expect("pending cleanup");
        assert!(
            retained
                .pending_provider_suspension_retirements
                .contains(&reference),
            "{fault:?}"
        );
        assert!(retained.provider_suspension.is_none());
        assert_eq!(provider.suspension_count(), 1);
        let persisted = SandboxInstance::load(&configured_state_dir(&state), id)
            .expect("durable pending cleanup");
        assert!(
            persisted
                .pending_provider_suspension_retirements
                .contains(&reference)
        );
        *provider
            .suspension_retirement_fault
            .lock()
            .expect("retirement fault") = None;
        assert!(state.manager.destroy(id).await.expect("retry cleanup"));
        assert_eq!(provider.suspension_count(), 0);
    }
}

fn capacity_fixture(
    temp: &tempfile::TempDir,
) -> (Arc<ServerState>, Arc<CapacityTestProvider>, String) {
    let config = test_config(temp);
    let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        config.storage.images_dir.clone(),
        config.storage.instances_dir.clone(),
    ));
    let provider = Arc::new(CapacityTestProvider::new(storage.clone()));
    let class = encode_capacity_class_digest(
        provider
            .capacity
            .lock()
            .expect("capacity")
            .scope
            .class_digest,
    );
    let state = build_test_state_with_provider(
        config,
        test_policy(BackendKind::Mock),
        spawners(BackendKind::Mock, Arc::new(MockSpawner)),
        BackendKind::Mock,
        storage,
        provider.clone(),
    );
    (state, provider, format!("/v1/pools/mock/{class}"))
}

#[tokio::test]
async fn provider_errors_have_exact_http_status_and_public_response_bodies() {
    let temp = tempfile::tempdir().expect("test directory");
    let (state, provider, path) = capacity_fixture(&temp);
    for (error, expected_status) in [
        (ProviderError::Unsupported, 501),
        (ProviderError::NotFound, 404),
        (ProviderError::Conflict, 409),
        (ProviderError::Unavailable, 503),
        (ProviderError::OutcomeUnknown, 500),
        (ProviderError::InvalidResponse, 500),
        (ProviderError::Incompatible, 500),
    ] {
        *provider.response_error.lock().expect("response error") = Some(error);
        for (method, target) in [
            (Method::GET, path.clone()),
            (Method::POST, format!("{path}/drain")),
        ] {
            let (status, body) = handled_json(&state, method, &target, Vec::new()).await;
            assert_eq!(status.as_u16(), expected_status, "{error:?}");
            assert_eq!(
                body,
                json!({"status": expected_status, "error": error.to_string()})
            );
            assert!(
                !body
                    .to_string()
                    .contains(temp.path().to_str().expect("test path"))
            );
        }
    }
}

#[tokio::test]
async fn invalid_drain_requests_are_rejected_before_provider_mutation() {
    let temp = tempfile::tempdir().expect("test directory");
    let (state, provider, path) = capacity_fixture(&temp);
    let before = *provider.capacity.lock().expect("capacity");
    for body in [
        b"{".to_vec(),
        br#"{"unexpected":true}"#.to_vec(),
        br#"{"operation_id":"not-a-uuid"}"#.to_vec(),
        serde_json::to_vec(&json!({"operation_id": Uuid::nil()})).expect("nil operation body"),
    ] {
        let (status, response) =
            handled_json(&state, Method::POST, &format!("{path}/drain"), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["status"], 400);
        assert_eq!(provider.drain_calls.load(Ordering::Acquire), 0);
        assert_eq!(
            *provider.capacity.lock().expect("unchanged capacity"),
            before
        );
        assert!(provider.drains.lock().expect("drain records").is_empty());
    }
}

#[tokio::test]
async fn invalid_drain_success_responses_never_become_http_success() {
    let temp = tempfile::tempdir().expect("test directory");
    let (state, provider, path) = capacity_fixture(&temp);
    let original = *provider.capacity.lock().expect("capacity");
    let operation_id = Uuid::new_v4();
    let valid = DrainResult {
        operation_id,
        removed_ready: 3,
        deferred_in_use: 2,
        snapshot: CapacitySnapshot {
            ready: 0,
            building: 0,
            in_use: 0,
            draining: 3,
            accepting_allocations: false,
            revision: 2,
            ..original
        },
    };
    for field in [
        "operation",
        "deferred",
        "accepting",
        "ready",
        "building",
        "in_use",
        "provider",
        "scope",
        "revision",
    ] {
        let mut response = valid;
        match field {
            "operation" => response.operation_id = Uuid::new_v4(),
            "deferred" => response.deferred_in_use = 4,
            "accepting" => response.snapshot.accepting_allocations = true,
            "ready" => response.snapshot.ready = 1,
            "building" => response.snapshot.building = 1,
            "in_use" => response.snapshot.in_use = 1,
            "provider" => response.snapshot.provider_instance_id = Uuid::new_v4(),
            "scope" => response.snapshot.scope.backend = BackendKind::Firecracker,
            "revision" => response.snapshot.revision = 0,
            _ => unreachable!(),
        }
        *provider.drain_response.lock().expect("drain response") = Some(response);
        let (status, body) = handled_json(
            &state,
            Method::POST,
            &format!("{path}/drain"),
            serde_json::to_vec(&json!({"operation_id": operation_id})).expect("drain request"),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{field}");
        assert_eq!(
            body,
            json!({"status": 500, "error": ProviderError::InvalidResponse.to_string()})
        );
    }
}
