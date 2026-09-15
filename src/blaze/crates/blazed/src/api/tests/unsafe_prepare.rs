// SPDX-License-Identifier: Apache-2.0
//! Reject unsafe ownership without mistaking an unrelated lookup for cleanup proof.

use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum PreparedResponseFault {
    Provider,
    Sandbox,
    Request,
    Operation,
    Lease,
    InitialGeneration,
    Generation,
    State,
    Resources,
}

const UNSAFE_BINDINGS: [PreparedResponseFault; 8] = [
    PreparedResponseFault::Provider,
    PreparedResponseFault::Sandbox,
    PreparedResponseFault::Request,
    PreparedResponseFault::Operation,
    PreparedResponseFault::Lease,
    PreparedResponseFault::InitialGeneration,
    PreparedResponseFault::Generation,
    PreparedResponseFault::State,
];

impl InventoryTestProvider {
    pub(super) fn inject_prepared_response_fault(
        &self,
        mut prepared: PreparedLease,
    ) -> PreparedLease {
        let Some(fault) = *self.prepared_response_fault.lock().expect("response fault") else {
            return prepared;
        };
        let expected = prepared.binding.context;
        if matches!(fault, PreparedResponseFault::Resources) {
            prepared.resources = PreparedResources::PathBacked {
                storage: StorageSlot {
                    id: expected.instance_id.to_string(),
                    rootfs_path: PathBuf::new(),
                    mem_path: PathBuf::new(),
                    mem_diff_path: PathBuf::new(),
                    rootfs_diff_path: PathBuf::new(),
                    instance_dir: PathBuf::new(),
                },
                restore_payload_dir: None,
            };
            return prepared;
        }
        match fault {
            PreparedResponseFault::Provider => {
                prepared.binding.provider_instance_id = Uuid::new_v4()
            }
            PreparedResponseFault::Sandbox => prepared.binding.context.instance_id = Uuid::new_v4(),
            PreparedResponseFault::Request => prepared.binding.context.request_id = Uuid::new_v4(),
            PreparedResponseFault::Operation => {
                prepared.binding.context.operation_id = Uuid::new_v4()
            }
            PreparedResponseFault::Lease => prepared.binding.context.lease_id = Uuid::new_v4(),
            PreparedResponseFault::InitialGeneration => prepared.binding.context.generation += 1,
            PreparedResponseFault::Generation => prepared.binding.generation += 1,
            PreparedResponseFault::State => prepared.binding.state = LeaseState::Finalized,
            PreparedResponseFault::Resources => unreachable!("resource faults return above"),
        }
        self.bindings
            .lock()
            .expect("bindings")
            .remove(&expected.lease_id);
        self.record(prepared.binding);
        // Model a provider that allocated a different lease but cannot find
        // the original request. Its optional inventory interface is disabled.
        *self.absent_prepare_context.lock().expect("absent context") = Some(expected);
        prepared
    }
}

async fn exercise_unsafe_replacement(resume: bool, fault: PreparedResponseFault) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = test_config(&temporary);
    let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        config.storage.images_dir.clone(),
        config.storage.instances_dir.clone(),
    ));
    let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
    provider.inventory_enabled.store(false, Ordering::Release);
    assert!(provider.inventory().is_none());
    let state = build_test_state_with_provider(
        config.clone(),
        test_policy(BackendKind::Mock),
        spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
        BackendKind::Mock,
        storage.clone(),
        provider.clone(),
    );
    let created = created_json(&state, &test_request()).await;
    let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("id")).expect("UUID");
    let checkpoint_id = if resume {
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
        None
    } else {
        Some(state.manager.checkpoint(id).await.expect("checkpoint").id)
    };
    let before = state.manager.get(id).expect("original owner");
    *provider
        .prepared_response_fault
        .lock()
        .expect("response fault") = Some(fault);
    let error = if resume {
        state
            .manager
            .resume(
                id,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("unsafe resume response")
    } else {
        state
            .manager
            .restore(
                id,
                RestoreSandbox {
                    checkpoint_id: checkpoint_id.expect("checkpoint identity"),
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("unsafe checkpoint response")
    };
    assert!(
        matches!(error, BlazeDaemonError::RecoveryRequired(_)),
        "{fault:?}: {error}"
    );
    let retained = state.manager.get(id).expect("retained lifecycle");
    assert_eq!(retained.state, SandboxState::RecoveryRequired, "{fault:?}");
    assert_eq!(retained.data_plane_lease, before.data_plane_lease);
    assert_eq!(retained.provider_suspension, before.provider_suspension);
    assert!(retained.replacement_data_plane_lease.is_none());
    let pending = retained
        .operation
        .as_ref()
        .and_then(|operation| operation.provider_operation)
        .expect("durable request retained");
    assert_eq!(provider.abort_calls.load(Ordering::Acquire), 0);
    assert_eq!(provider.unsafe_inspect_calls.load(Ordering::Acquire), 0);
    assert!(matches!(
        provider
            .inspect(InspectRequest {
                context: RequestContext::from(pending.context),
            })
            .await,
        Err(ProviderError::NotFound)
    ));
    let bindings = provider.bindings.lock().expect("bindings").clone();
    assert!(
        !bindings.is_empty(),
        "the unsafe response represents retained resources"
    );
    if !resume {
        assert!(
            state.manager.backend_owner(id).is_some(),
            "old backend remains owned"
        );
    }
    let operation = retained.operation.clone();
    assert!(matches!(
        state.manager.destroy(id).await,
        Err(BlazeDaemonError::RecoveryRequired(_))
    ));
    assert_eq!(
        state.manager.get(id).expect("blocked cleanup").operation,
        operation
    );
    drop(state);

    let restarted = build_test_state_with_provider(
        config.clone(),
        test_policy(BackendKind::Mock),
        spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
        BackendKind::Mock,
        storage,
        provider.clone(),
    );
    let loaded = restarted
        .manager
        .get(id)
        .expect("reload unresolved request");
    assert_eq!(loaded.state, SandboxState::RecoveryRequired);
    assert_eq!(loaded.operation, operation);
    let report = restarted
        .manager
        .reconcile_startup()
        .await
        .expect("startup reconciliation");
    assert_eq!(report.completed, 0);
    assert!(!report.failures.is_empty());
    assert!(matches!(
        restarted.manager.destroy(id).await,
        Err(BlazeDaemonError::RecoveryRequired(_))
    ));
    let after = restarted.manager.get(id).expect("request remains tracked");
    assert_eq!(after.state, SandboxState::RecoveryRequired);
    assert_eq!(after.operation, operation);
    assert_eq!(after.data_plane_lease, before.data_plane_lease);
    assert_eq!(after.provider_suspension, before.provider_suspension);
    assert_eq!(*provider.bindings.lock().expect("bindings"), bindings);
    assert_eq!(provider.abort_calls.load(Ordering::Acquire), 0);
    assert_eq!(provider.unsafe_inspect_calls.load(Ordering::Acquire), 1);
    if resume {
        assert!(
            config
                .daemon
                .state_dir
                .join(id.to_string())
                .join("hibernate/manifest.json")
                .is_file()
        );
        assert_eq!(provider.suspension_count(), 1);
    } else {
        assert_eq!(provider.checkpoint_count(), 1);
    }
}

#[tokio::test]
async fn unsafe_checkpoint_restore_binding_retains_recovery_without_inventory() {
    for fault in UNSAFE_BINDINGS {
        exercise_unsafe_replacement(false, fault).await;
    }
}

#[tokio::test]
async fn unsafe_resume_binding_retains_recovery_without_inventory() {
    for fault in UNSAFE_BINDINGS {
        exercise_unsafe_replacement(true, fault).await;
    }
}

#[tokio::test]
async fn safe_replacement_binding_still_compensates_invalid_resources() {
    for resume in [false, true] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = test_config(&temporary);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        provider.inventory_enabled.store(false, Ordering::Release);
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("id")).expect("UUID");
        let checkpoint_id = if resume {
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
            None
        } else {
            Some(state.manager.checkpoint(id).await.expect("checkpoint").id)
        };
        let before = state.manager.get(id).expect("before");
        let bindings = provider.bindings.lock().expect("bindings").clone();
        *provider
            .prepared_response_fault
            .lock()
            .expect("response fault") = Some(PreparedResponseFault::Resources);
        let error = if resume {
            state
                .manager
                .resume(
                    id,
                    ResumeSandbox {
                        binary_path: PathBuf::new(),
                    },
                )
                .await
                .expect_err("invalid resume resources")
        } else {
            state
                .manager
                .restore(
                    id,
                    RestoreSandbox {
                        checkpoint_id: checkpoint_id.expect("checkpoint"),
                        binary_path: PathBuf::new(),
                    },
                )
                .await
                .expect_err("invalid checkpoint resources")
        };
        assert!(matches!(
            error,
            BlazeDaemonError::DataPlane(ProviderError::InvalidResponse)
        ));
        let after = state.manager.get(id).expect("compensated");
        assert_eq!(after.state, before.state);
        assert!(after.operation.is_none());
        assert_eq!(after.data_plane_lease, before.data_plane_lease);
        assert_eq!(after.provider_suspension, before.provider_suspension);
        assert_eq!(*provider.bindings.lock().expect("bindings"), bindings);
        assert_eq!(provider.abort_calls.load(Ordering::Acquire), 1);
        assert!(
            state
                .manager
                .destroy(id)
                .await
                .expect("destroy after compensation")
        );
    }
}

#[tokio::test]
async fn unsafe_create_binding_cannot_be_forgotten_by_cleanup_or_restart() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let config = test_config(&temporary);
    let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
        config.storage.images_dir.clone(),
        config.storage.instances_dir.clone(),
    ));
    let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
    provider.inventory_enabled.store(false, Ordering::Release);
    *provider
        .prepared_response_fault
        .lock()
        .expect("response fault") = Some(PreparedResponseFault::Lease);
    let state = build_test_state_with_provider(
        config.clone(),
        test_policy(BackendKind::Mock),
        spawners(BackendKind::Mock, Arc::new(MockSpawner)),
        BackendKind::Mock,
        storage.clone(),
        provider.clone(),
    );
    let error = dispatch(&Method::POST, "/v1/sandboxes", "", test_request(), &state)
        .await
        .expect_err("unsafe creation response");
    assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
    let instances = state.manager.list().expect("retained instance");
    assert_eq!(instances.len(), 1);
    let id = instances[0].id;
    assert_eq!(instances[0].state, SandboxState::RecoveryRequired);
    let operation = instances[0].operation.clone();
    assert!(matches!(
        state.manager.destroy(id).await,
        Err(BlazeDaemonError::RecoveryRequired(_))
    ));
    assert_eq!(
        state.manager.get(id).expect("blocked cleanup").operation,
        operation
    );
    drop(state);
    let restarted = build_test_state_with_provider(
        config,
        test_policy(BackendKind::Mock),
        spawners(BackendKind::Mock, Arc::new(MockSpawner)),
        BackendKind::Mock,
        storage,
        provider.clone(),
    );
    let report = restarted
        .manager
        .reconcile_startup()
        .await
        .expect("reconcile");
    assert_eq!(report.completed, 0);
    assert!(!report.failures.is_empty());
    let retained = restarted.manager.get(id).expect("retained request");
    assert_eq!(retained.state, SandboxState::RecoveryRequired);
    assert_eq!(retained.operation, operation);
    assert_eq!(provider.abort_calls.load(Ordering::Acquire), 0);
    assert_eq!(provider.unsafe_inspect_calls.load(Ordering::Acquire), 0);
    assert_eq!(provider.bindings.lock().expect("bindings").len(), 1);
}
