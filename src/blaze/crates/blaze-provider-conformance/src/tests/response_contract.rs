// SPDX-License-Identifier: Apache-2.0

use super::*;
use blaze_provider_api::CapacityClass;

fn checkpoint(previous: LeaseBinding) -> CheckpointSubmission {
    let next = LeaseBinding {
        generation: previous.generation + 1,
        ..previous
    };
    CheckpointSubmission {
        binding: next,
        checkpoint: ProviderCheckpointRef {
            provider_instance_id: previous.provider_instance_id,
            public_checkpoint_id: Uuid::new_v4(),
            reference_id: Uuid::new_v4(),
            content_digest: format!("sha256:{}", "ab".repeat(32)),
            parent_reference_id: None,
            source_lease_id: previous.context.lease_id,
            source_generation: next.generation,
        },
    }
}

#[test]
fn checkpoint_submission_rejects_each_mismatched_identity_and_digest() {
    let previous = binding(LeaseState::Finalized, 4);
    let valid = checkpoint(previous);
    let id = valid.checkpoint.public_checkpoint_id;
    assert_eq!(
        validate_checkpoint_submission(previous, id, None, &valid),
        Ok(())
    );
    let mut cases = Vec::new();
    for field in [
        "provider",
        "public_id",
        "reference",
        "lease",
        "generation",
        "parent",
    ] {
        let mut candidate = checkpoint(previous);
        candidate.checkpoint = valid.checkpoint.clone();
        match field {
            "provider" => candidate.checkpoint.provider_instance_id = Uuid::new_v4(),
            "public_id" => candidate.checkpoint.public_checkpoint_id = Uuid::new_v4(),
            "reference" => candidate.checkpoint.reference_id = Uuid::nil(),
            "lease" => candidate.checkpoint.source_lease_id = Uuid::new_v4(),
            "generation" => candidate.checkpoint.source_generation += 1,
            "parent" => candidate.checkpoint.parent_reference_id = Some(Uuid::new_v4()),
            _ => unreachable!(),
        }
        cases.push((field.to_string(), candidate));
    }
    for digest in [
        String::new(),
        "ab".repeat(32),
        "sha256:abc".into(),
        format!("sha256:{}", "AB".repeat(32)),
        format!("sha256:{}", "gg".repeat(32)),
    ] {
        let mut candidate = checkpoint(previous);
        candidate.checkpoint = valid.checkpoint.clone();
        candidate.checkpoint.content_digest = digest.clone();
        cases.push((digest, candidate));
    }
    for (label, candidate) in cases {
        assert_eq!(
            validate_checkpoint_submission(previous, id, None, &candidate),
            Err(ConformanceError::InvalidCheckpoint),
            "{label}"
        );
    }
    let parent = checkpoint(previous).checkpoint;
    assert_eq!(
        validate_checkpoint_submission(previous, id, Some(&parent), &valid),
        Err(ConformanceError::InvalidCheckpoint)
    );
    assert_eq!(
        validate_checkpoint_submission(previous, Uuid::nil(), None, &valid),
        Err(ConformanceError::InvalidCheckpoint)
    );
}

#[test]
fn retirement_receipts_must_echo_the_exact_checkpoint_and_suspension() {
    let captured = checkpoint(binding(LeaseState::Finalized, 4)).checkpoint;
    let suspension = ProviderSuspensionRef {
        provider_instance_id: captured.provider_instance_id,
        suspension_id: Uuid::new_v4(),
        reference_id: Uuid::new_v4(),
        content_digest: captured.content_digest.clone(),
        source_lease_id: captured.source_lease_id,
        source_generation: captured.source_generation,
        root_filesystem_bytes: 4096,
        guest_memory_bytes: 8192,
    };
    for retired in [false, true] {
        let receipt = RetireCheckpointResult {
            public_checkpoint_id: captured.public_checkpoint_id,
            reference_id: Some(captured.reference_id),
            retired,
        };
        assert_eq!(validate_checkpoint_retirement(&captured, receipt), Ok(()));
        for id in [Uuid::nil(), Uuid::new_v4()] {
            assert_eq!(
                validate_checkpoint_retirement(
                    &captured,
                    RetireCheckpointResult {
                        public_checkpoint_id: id,
                        ..receipt
                    }
                ),
                Err(ConformanceError::InvalidCheckpoint)
            );
        }
        for reference_id in [None, Some(Uuid::nil()), Some(Uuid::new_v4())] {
            assert_eq!(
                validate_checkpoint_retirement(
                    &captured,
                    RetireCheckpointResult {
                        reference_id,
                        ..receipt
                    }
                ),
                Err(ConformanceError::InvalidCheckpoint)
            );
        }
        let receipt = RetireSuspensionResult {
            suspension_id: suspension.suspension_id,
            reference_id: Some(suspension.reference_id),
            retired,
        };
        assert_eq!(validate_suspension_retirement(&suspension, receipt), Ok(()));
        for id in [Uuid::nil(), Uuid::new_v4()] {
            assert_eq!(
                validate_suspension_retirement(
                    &suspension,
                    RetireSuspensionResult {
                        suspension_id: id,
                        ..receipt
                    }
                ),
                Err(ConformanceError::InvalidSuspension)
            );
        }
        for reference_id in [None, Some(Uuid::nil()), Some(Uuid::new_v4())] {
            assert_eq!(
                validate_suspension_retirement(
                    &suspension,
                    RetireSuspensionResult {
                        reference_id,
                        ..receipt
                    }
                ),
                Err(ConformanceError::InvalidSuspension)
            );
        }
    }
}

fn capacity_fixture() -> (ProviderDescriptor, CapacityRequest, CapacitySnapshot) {
    let descriptor = ProviderDescriptor {
        contract_version: PROVIDER_CONTRACT_VERSION,
        provider_instance_id: Uuid::new_v4(),
    };
    let class = CapacityClass {
        root_filesystem_capacity_bytes: 4096,
        guest_memory_capacity_bytes: 8192,
    };
    let scope = blaze_provider_api::CapacityScope {
        backend: blaze_core::backend::BackendKind::Mock,
        class_digest: class.digest(),
    };
    (
        descriptor,
        CapacityRequest { scope },
        CapacitySnapshot {
            provider_instance_id: descriptor.provider_instance_id,
            scope,
            class,
            revision: 1,
            ready: 0,
            building: 0,
            in_use: 0,
            draining: 2,
            quarantined: 0,
            accepting_allocations: false,
        },
    )
}

#[test]
fn capacity_rejects_wrong_identity_scope_revision_and_empty_class() {
    let (descriptor, request, snapshot) = capacity_fixture();
    assert_eq!(
        validate_capacity_snapshot(descriptor, request, snapshot),
        Ok(())
    );
    let mut cases = Vec::new();
    cases.push(CapacitySnapshot {
        provider_instance_id: Uuid::new_v4(),
        ..snapshot
    });
    cases.push(CapacitySnapshot {
        revision: 0,
        ..snapshot
    });
    let mut wrong_backend = snapshot;
    wrong_backend.scope.backend = blaze_core::backend::BackendKind::Firecracker;
    cases.push(wrong_backend);
    let mut wrong_digest = snapshot;
    wrong_digest.scope.class_digest = [9; 32];
    cases.push(wrong_digest);
    for candidate in cases {
        assert_eq!(
            validate_capacity_snapshot(descriptor, request, candidate),
            Err(ConformanceError::InvalidCapacity),
            "{candidate:?}"
        );
    }
    assert_eq!(
        validate_capacity_snapshot(
            ProviderDescriptor {
                provider_instance_id: Uuid::nil(),
                ..descriptor
            },
            request,
            snapshot
        ),
        Err(ConformanceError::InvalidCapacity)
    );
    let mut empty = snapshot;
    empty.class = CapacityClass {
        root_filesystem_capacity_bytes: 0,
        guest_memory_capacity_bytes: 0,
    };
    empty.scope.class_digest = empty.class.digest();
    assert_eq!(
        validate_capacity_snapshot(descriptor, CapacityRequest { scope: empty.scope }, empty),
        Err(ConformanceError::InvalidCapacity)
    );
    empty = snapshot;
    empty.scope.class_digest = [0; 32];
    assert_eq!(
        validate_capacity_snapshot(descriptor, CapacityRequest { scope: empty.scope }, empty),
        Err(ConformanceError::InvalidCapacity)
    );
}

#[test]
fn drain_rejects_invalid_success_receipts_and_non_drained_snapshots() {
    let (descriptor, capacity, snapshot) = capacity_fixture();
    let request = DrainRequest {
        scope: capacity.scope,
        operation_id: Uuid::new_v4(),
    };
    let valid = DrainResult {
        operation_id: request.operation_id,
        removed_ready: 1,
        deferred_in_use: 2,
        snapshot,
    };
    assert_eq!(validate_drain_result(descriptor, request, valid), Ok(()));
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
        let mut candidate = valid;
        match field {
            "operation" => candidate.operation_id = Uuid::new_v4(),
            "deferred" => candidate.deferred_in_use = snapshot.draining + 1,
            "accepting" => candidate.snapshot.accepting_allocations = true,
            "ready" => candidate.snapshot.ready = 1,
            "building" => candidate.snapshot.building = 1,
            "in_use" => candidate.snapshot.in_use = 1,
            "provider" => candidate.snapshot.provider_instance_id = Uuid::new_v4(),
            "scope" => {
                candidate.snapshot.scope.backend = blaze_core::backend::BackendKind::Firecracker
            }
            "revision" => candidate.snapshot.revision = 0,
            _ => unreachable!(),
        }
        assert_eq!(
            validate_drain_result(descriptor, request, candidate),
            Err(ConformanceError::InvalidCapacity),
            "{field}"
        );
    }
    let nil_request = DrainRequest {
        operation_id: Uuid::nil(),
        ..request
    };
    assert_eq!(
        validate_drain_result(
            descriptor,
            nil_request,
            DrainResult {
                operation_id: Uuid::nil(),
                ..valid
            }
        ),
        Err(ConformanceError::InvalidCapacity)
    );
}
