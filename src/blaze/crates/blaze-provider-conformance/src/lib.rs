// SPDX-License-Identifier: Apache-2.0
//! Reusable response validation for build-time data-plane providers.

#![forbid(unsafe_code)]

mod example_provider;

pub use example_provider::ExampleFileProvider;

use std::collections::HashSet;

use blaze_provider_api::{
    AttachmentRole, CapacityRequest, CapacitySnapshot, CheckpointSubmission, CommitRequest,
    DataPlaneProvider, DrainRequest, DrainResult, FinalizeRequest, InspectRequest, InventoryPage,
    InventorySnapshot, LeaseBinding, LeaseState, MAX_INVENTORY_CURSOR_BYTES,
    PROVIDER_CONTRACT_VERSION, PrepareRequest, PrepareSource, PreparedLease, PreparedResources,
    ProviderCapabilities, ProviderCheckpointRef, ProviderDescriptor, ProviderError,
    ProviderSuspensionRef, PublicTransitionRef, ReconcileAction, ReleaseRequest, RequestContext,
    RetireCheckpointResult, RetireSuspensionResult, StopRequest, SuspensionSubmission,
};
use thiserror::Error;

/// A provider response violated a source-level contract invariant.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConformanceError {
    /// Provider descriptor cannot identify a compatible implementation.
    #[error("invalid provider descriptor")]
    InvalidDescriptor,
    /// Returned binding does not match the initiating request.
    #[error("provider lease binding does not match the request")]
    BindingMismatch,
    /// Returned state or generation is not the required next transition.
    #[error("provider lease transition is invalid")]
    InvalidTransition,
    /// Prepared resources cannot satisfy the selected source.
    #[error("prepared provider resources are invalid")]
    InvalidResources,
    /// Inventory snapshot or lease identity cannot be trusted.
    #[error("provider inventory is invalid")]
    InvalidInventory,
    /// Checkpoint identity, lineage, or provider content is invalid.
    #[error("provider checkpoint response is invalid")]
    InvalidCheckpoint,
    /// Suspension identity or provider content is invalid.
    #[error("provider suspension response is invalid")]
    InvalidSuspension,
    /// Capacity scope, revision, accounting, or drain identity is invalid.
    #[error("provider capacity response is invalid")]
    InvalidCapacity,
}

/// Failure reported by the reusable create-and-delete contract exercise.
#[derive(Debug, Error)]
pub enum ExerciseError {
    /// The provider rejected or could not complete one operation.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// A response violated a provider-independent contract invariant.
    #[error(transparent)]
    Conformance(#[from] ConformanceError),
}

/// Validate a provider descriptor before any mutating call.
pub fn validate_descriptor(descriptor: ProviderDescriptor) -> Result<(), ConformanceError> {
    if descriptor.contract_version != PROVIDER_CONTRACT_VERSION
        || descriptor.provider_instance_id.is_nil()
    {
        return Err(ConformanceError::InvalidDescriptor);
    }
    Ok(())
}

/// Validate a preparation result against the exact initiating request.
pub fn validate_prepared(
    capabilities: ProviderCapabilities,
    context: RequestContext,
    template_source: bool,
    root_filesystem_bytes: u64,
    guest_memory_bytes: u64,
    prepared: &PreparedLease,
) -> Result<(), ConformanceError> {
    validate_prepared_binding(context, prepared.binding)?;
    match &prepared.resources {
        PreparedResources::PathBacked {
            storage,
            restore_payload_dir,
        } => {
            if storage.id != context.instance_id.to_string() {
                return Err(ConformanceError::InvalidResources);
            }
            if template_source != restore_payload_dir.is_some() {
                return Err(ConformanceError::InvalidResources);
            }
        }
        PreparedResources::OpenedRestore {
            restore_payload_dir,
            attachments,
        } => {
            if !template_source
                || !capabilities.opened_template_restore_resources
                || restore_payload_dir.as_os_str().is_empty()
            {
                return Err(ConformanceError::InvalidResources);
            }
            validate_opened_attachments(attachments, root_filesystem_bytes, guest_memory_bytes)?;
        }
        PreparedResources::CheckpointRestore { .. }
        | PreparedResources::SuspensionRestore { .. } => {
            return Err(ConformanceError::InvalidResources);
        }
    }
    Ok(())
}

/// Validate that a prepared binding is safe to use for compensation.
///
/// Callers should not pass an untrusted binding to `abort`: a mismatched
/// provider, lease, or generation could identify resources owned by another
/// operation.
pub fn validate_prepared_binding(
    context: RequestContext,
    binding: LeaseBinding,
) -> Result<(), ConformanceError> {
    if context.instance_id.is_nil()
        || context.request_id.is_nil()
        || context.operation_id.is_nil()
        || context.lease_id.is_nil()
        || context.generation == 0
        || binding.provider_instance_id.is_nil()
        || binding.context != context
        || binding.generation != context.generation
        || binding.state != LeaseState::Prepared
    {
        return Err(ConformanceError::BindingMismatch);
    }
    Ok(())
}

/// Validate that one result is the exact next state of the same lease.
pub fn validate_transition(
    previous: LeaseBinding,
    next: LeaseBinding,
    expected: LeaseState,
) -> Result<(), ConformanceError> {
    let Some(expected_generation) = previous.generation.checked_add(1) else {
        return Err(ConformanceError::InvalidTransition);
    };
    if next.provider_instance_id != previous.provider_instance_id
        || next.context != previous.context
        || next.generation != expected_generation
        || next.state != expected
    {
        return Err(ConformanceError::InvalidTransition);
    }
    Ok(())
}

/// Validate one frozen inventory identity before requesting any pages.
pub fn validate_inventory_snapshot(
    descriptor: ProviderDescriptor,
    snapshot: InventorySnapshot,
) -> Result<(), ConformanceError> {
    validate_descriptor(descriptor)?;
    if snapshot.provider_instance_id != descriptor.provider_instance_id
        || snapshot.snapshot_id.is_nil()
    {
        return Err(ConformanceError::InvalidInventory);
    }
    Ok(())
}

/// Validate one bounded inventory page before consuming any lease identities.
pub fn validate_inventory_page(
    page: &InventoryPage,
    requested_page_size: u32,
) -> Result<(), ConformanceError> {
    if requested_page_size == 0 || page.leases.len() > requested_page_size as usize {
        return Err(ConformanceError::InvalidInventory);
    }
    if let Some(cursor) = page.next_cursor.as_deref()
        && (cursor.is_empty()
            || cursor.len() > MAX_INVENTORY_CURSOR_BYTES
            || page.leases.is_empty())
    {
        return Err(ConformanceError::InvalidInventory);
    }
    Ok(())
}

/// Validate one lease returned by a provider inventory.
pub fn validate_inventory_lease(
    descriptor: ProviderDescriptor,
    binding: LeaseBinding,
) -> Result<(), ConformanceError> {
    if binding.provider_instance_id != descriptor.provider_instance_id
        || binding.context.instance_id.is_nil()
        || binding.context.request_id.is_nil()
        || binding.context.operation_id.is_nil()
        || binding.context.lease_id.is_nil()
        || binding.context.generation == 0
        || binding.generation < binding.context.generation
        || binding.state == LeaseState::Released
    {
        return Err(ConformanceError::InvalidInventory);
    }
    Ok(())
}

/// Validate the exact transition required by one reconciliation action.
pub fn validate_reconcile_result(
    previous: LeaseBinding,
    next: LeaseBinding,
    action: ReconcileAction,
) -> Result<(), ConformanceError> {
    if previous.generation < previous.context.generation {
        return Err(ConformanceError::InvalidTransition);
    }
    let expected = match action {
        ReconcileAction::Adopt { backend_process }
            if matches!(
                previous.state,
                LeaseState::Committed | LeaseState::Finalized
            ) && backend_process.pid != 0
                && backend_process.start_time_ticks != 0 =>
        {
            LeaseState::Finalized
        }
        ReconcileAction::Quarantine
            if matches!(
                previous.state,
                LeaseState::Prepared
                    | LeaseState::Committed
                    | LeaseState::Finalized
                    | LeaseState::Stopped
                    | LeaseState::Quarantined
            ) =>
        {
            LeaseState::Quarantined
        }
        _ => return Err(ConformanceError::InvalidTransition),
    };
    validate_transition(previous, next, expected)
}

/// Validate one immutable provider capture and its active-lease generation.
pub fn validate_checkpoint_submission(
    previous: LeaseBinding,
    checkpoint_id: uuid::Uuid,
    parent: Option<&ProviderCheckpointRef>,
    submission: &CheckpointSubmission,
) -> Result<(), ConformanceError> {
    validate_transition(previous, submission.binding, LeaseState::Finalized)?;
    let checkpoint = &submission.checkpoint;
    let digest = checkpoint.content_digest.strip_prefix("sha256:");
    if checkpoint_id.is_nil()
        || checkpoint.provider_instance_id != previous.provider_instance_id
        || checkpoint.public_checkpoint_id != checkpoint_id
        || checkpoint.reference_id.is_nil()
        || checkpoint.source_lease_id != previous.context.lease_id
        || checkpoint.source_generation != submission.binding.generation
        || checkpoint.parent_reference_id != parent.map(|parent| parent.reference_id)
        || !digest.is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(ConformanceError::InvalidCheckpoint);
    }
    Ok(())
}

/// Validate resources prepared from one provider checkpoint.
pub fn validate_checkpoint_restore(
    capabilities: ProviderCapabilities,
    context: RequestContext,
    checkpoint: &ProviderCheckpointRef,
    root_filesystem_bytes: u64,
    guest_memory_bytes: u64,
    prepared: &PreparedLease,
) -> Result<(), ConformanceError> {
    validate_prepared_binding(context, prepared.binding)?;
    if prepared.binding.provider_instance_id != checkpoint.provider_instance_id {
        return Err(ConformanceError::InvalidCheckpoint);
    }
    match &prepared.resources {
        PreparedResources::CheckpointRestore {
            storage: Some(storage),
            attachments,
        } if attachments.is_empty() && storage.id == context.instance_id.to_string() => {}
        PreparedResources::CheckpointRestore {
            storage: None,
            attachments,
        } if capabilities.opened_checkpoint_restore_resources => {
            validate_opened_attachments(attachments, root_filesystem_bytes, guest_memory_bytes)?
        }
        _ => return Err(ConformanceError::InvalidResources),
    }
    Ok(())
}

fn validate_opened_attachments(
    attachments: &[blaze_provider_api::OpenedAttachment],
    root_filesystem_bytes: u64,
    guest_memory_bytes: u64,
) -> Result<(), ConformanceError> {
    if attachments.len() != 2 {
        return Err(ConformanceError::InvalidResources);
    }
    let mut roles = HashSet::new();
    for attachment in attachments {
        if !roles.insert(attachment.role)
            || attachment.logical_size_bytes == 0
            || !attachment.logical_size_bytes.is_multiple_of(4096)
        {
            return Err(ConformanceError::InvalidResources);
        }
    }
    let root = attachments
        .iter()
        .find(|attachment| attachment.role == AttachmentRole::RootDrive)
        .ok_or(ConformanceError::InvalidResources)?;
    let memory = attachments
        .iter()
        .find(|attachment| attachment.role == AttachmentRole::GuestMemory)
        .ok_or(ConformanceError::InvalidResources)?;
    if root.logical_size_bytes != root_filesystem_bytes
        || memory.logical_size_bytes != guest_memory_bytes
    {
        return Err(ConformanceError::InvalidResources);
    }
    Ok(())
}

/// Validate idempotent retirement of one exact provider reference.
pub fn validate_checkpoint_retirement(
    checkpoint: &ProviderCheckpointRef,
    result: RetireCheckpointResult,
) -> Result<(), ConformanceError> {
    if checkpoint.reference_id.is_nil()
        || result.public_checkpoint_id != checkpoint.public_checkpoint_id
        || result.reference_id != Some(checkpoint.reference_id)
    {
        return Err(ConformanceError::InvalidCheckpoint);
    }
    Ok(())
}

/// Validate one immutable suspension capture and its active-lease generation.
pub fn validate_suspension_submission(
    previous: LeaseBinding,
    suspension_id: uuid::Uuid,
    root_filesystem_bytes: u64,
    guest_memory_bytes: u64,
    submission: &SuspensionSubmission,
) -> Result<(), ConformanceError> {
    validate_transition(previous, submission.binding, LeaseState::Finalized)?;
    let suspension = &submission.suspension;
    validate_suspension_reference(previous.provider_instance_id, suspension)?;
    if suspension_id.is_nil()
        || suspension.suspension_id != suspension_id
        || suspension.source_lease_id != previous.context.lease_id
        || suspension.source_generation != submission.binding.generation
        || suspension.root_filesystem_bytes != root_filesystem_bytes
        || suspension.guest_memory_bytes != guest_memory_bytes
    {
        return Err(ConformanceError::InvalidSuspension);
    }
    Ok(())
}

/// Validate the bounded identity and integrity shape of a suspension reference.
pub fn validate_suspension_reference(
    provider_instance_id: uuid::Uuid,
    suspension: &ProviderSuspensionRef,
) -> Result<(), ConformanceError> {
    let digest = suspension.content_digest.strip_prefix("sha256:");
    if provider_instance_id.is_nil()
        || suspension.provider_instance_id != provider_instance_id
        || suspension.suspension_id.is_nil()
        || suspension.reference_id.is_nil()
        || suspension.source_lease_id.is_nil()
        || suspension.source_generation == 0
        || suspension.root_filesystem_bytes == 0
        || suspension.guest_memory_bytes == 0
        || !digest.is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(ConformanceError::InvalidSuspension);
    }
    Ok(())
}

/// Validate resources prepared from one immutable suspension reference.
pub fn validate_suspension_restore(
    capabilities: ProviderCapabilities,
    context: RequestContext,
    suspension: &ProviderSuspensionRef,
    root_filesystem_bytes: u64,
    guest_memory_bytes: u64,
    prepared: &PreparedLease,
) -> Result<(), ConformanceError> {
    validate_prepared_binding(context, prepared.binding)?;
    if prepared.binding.provider_instance_id != suspension.provider_instance_id
        || root_filesystem_bytes != suspension.root_filesystem_bytes
        || guest_memory_bytes != suspension.guest_memory_bytes
    {
        return Err(ConformanceError::InvalidSuspension);
    }
    match &prepared.resources {
        PreparedResources::SuspensionRestore {
            storage: Some(storage),
            attachments,
        } if attachments.is_empty() && storage.id == context.instance_id.to_string() => {}
        PreparedResources::SuspensionRestore {
            storage: None,
            attachments,
        } if capabilities.opened_suspension_restore_resources => {
            validate_opened_attachments(attachments, root_filesystem_bytes, guest_memory_bytes)?
        }
        _ => return Err(ConformanceError::InvalidResources),
    }
    Ok(())
}

/// Validate idempotent retirement of one exact provider suspension reference.
pub fn validate_suspension_retirement(
    suspension: &ProviderSuspensionRef,
    result: RetireSuspensionResult,
) -> Result<(), ConformanceError> {
    if suspension.reference_id.is_nil()
        || result.suspension_id != suspension.suspension_id
        || result.reference_id != Some(suspension.reference_id)
    {
        return Err(ConformanceError::InvalidSuspension);
    }
    Ok(())
}

/// Validate one complete and mutually exclusive capacity observation.
pub fn validate_capacity_snapshot(
    descriptor: ProviderDescriptor,
    request: CapacityRequest,
    snapshot: CapacitySnapshot,
) -> Result<(), ConformanceError> {
    if descriptor.provider_instance_id.is_nil()
        || snapshot.provider_instance_id != descriptor.provider_instance_id
        || snapshot.scope != request.scope
        || snapshot.scope.class_digest == [0; 32]
        || (snapshot.class.root_filesystem_capacity_bytes == 0
            && snapshot.class.guest_memory_capacity_bytes == 0)
        || snapshot.class.digest() != snapshot.scope.class_digest
        || snapshot.revision == 0
        || snapshot.checked_total().is_none()
    {
        return Err(ConformanceError::InvalidCapacity);
    }
    Ok(())
}

/// Validate a drain acknowledgement and its post-request capacity snapshot.
pub fn validate_drain_result(
    descriptor: ProviderDescriptor,
    request: DrainRequest,
    result: DrainResult,
) -> Result<(), ConformanceError> {
    if request.operation_id.is_nil()
        || result.operation_id != request.operation_id
        || result.deferred_in_use > result.snapshot.draining
        || result.snapshot.accepting_allocations
        || result.snapshot.ready != 0
        || result.snapshot.building != 0
        || result.snapshot.in_use != 0
    {
        return Err(ConformanceError::InvalidCapacity);
    }
    validate_capacity_snapshot(
        descriptor,
        CapacityRequest {
            scope: request.scope,
        },
        result.snapshot,
    )
}

/// Map a conformance violation to the public provider error category.
pub fn invalid_response(_: ConformanceError) -> ProviderError {
    ProviderError::InvalidResponse
}

/// Exercise the successful provider lifecycle without starting a backend.
///
/// This helper is intended for isolated provider tests. It verifies probe,
/// prepare, inspection, commit, public-state finalization, stop, and release
/// as one exact lease sequence. A caller remains responsible for testing real
/// backend consumption and every extension-defined compensation behavior.
pub async fn exercise_create_delete(
    provider: &(dyn DataPlaneProvider + Send + Sync),
    request: PrepareRequest,
) -> Result<(), ExerciseError> {
    let descriptor = provider.descriptor();
    validate_descriptor(descriptor)?;
    let capabilities = provider.capabilities();
    let template_source = matches!(&request.source, PrepareSource::Template(_));
    if (template_source && !capabilities.templates) || (!template_source && !capabilities.images) {
        return Err(ProviderError::Unsupported.into());
    }

    provider.probe().await?;
    let context = request.context;
    let root_filesystem_bytes = request.root_filesystem_bytes;
    let guest_memory_bytes = request.guest_memory_bytes;
    let prepared = provider.prepare(request).await?;
    validate_prepared(
        capabilities,
        context,
        template_source,
        root_filesystem_bytes,
        guest_memory_bytes,
        &prepared,
    )?;
    if prepared.binding.provider_instance_id != descriptor.provider_instance_id {
        return Err(ConformanceError::BindingMismatch.into());
    }

    let observed = provider.inspect(InspectRequest { context }).await?;
    if observed.binding != prepared.binding {
        return Err(ConformanceError::InvalidTransition.into());
    }

    let committed = provider
        .commit(CommitRequest {
            binding: prepared.binding,
        })
        .await?;
    validate_transition(prepared.binding, committed.binding, LeaseState::Committed)?;

    let finalized = provider
        .finalize(FinalizeRequest {
            binding: committed.binding,
            public_transition: PublicTransitionRef {
                instance_id: context.instance_id,
                operation_id: context.operation_id,
            },
        })
        .await?;
    validate_transition(committed.binding, finalized.binding, LeaseState::Finalized)?;

    let stopped = provider
        .stop(StopRequest {
            binding: finalized.binding,
        })
        .await?;
    validate_transition(finalized.binding, stopped.binding, LeaseState::Stopped)?;

    let released = provider
        .release(ReleaseRequest {
            binding: stopped.binding,
        })
        .await?;
    validate_transition(stopped.binding, released.binding, LeaseState::Released)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    mod response_contract;
    use std::fs::OpenOptions;
    use std::path::PathBuf;

    use super::*;
    use blaze_core::storage::StorageSlot;
    use blaze_provider_api::{AttachmentKind, InventoryLease, OpenedAttachment};
    use uuid::Uuid;

    fn binding(state: LeaseState, generation: u64) -> LeaseBinding {
        LeaseBinding {
            provider_instance_id: Uuid::new_v4(),
            context: RequestContext {
                instance_id: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                operation_id: Uuid::new_v4(),
                lease_id: Uuid::new_v4(),
                generation: 1,
            },
            generation,
            state,
        }
    }

    fn opened_attachments() -> Vec<OpenedAttachment> {
        let open = || -> std::os::fd::OwnedFd {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
                .expect("open attachment")
                .into()
        };
        vec![
            OpenedAttachment {
                role: AttachmentRole::RootDrive,
                descriptor: open(),
                kind: AttachmentKind::CharacterDevice,
                logical_size_bytes: 4096,
                consumer_path: Some(PathBuf::from("rootfs.ext4")),
            },
            OpenedAttachment {
                role: AttachmentRole::GuestMemory,
                descriptor: open(),
                kind: AttachmentKind::CharacterDevice,
                logical_size_bytes: 8192,
                consumer_path: None,
            },
        ]
    }

    #[test]
    fn transition_requires_same_binding_and_next_generation() {
        let previous = binding(LeaseState::Prepared, 1);
        let next = LeaseBinding {
            generation: 2,
            state: LeaseState::Committed,
            ..previous
        };
        assert_eq!(
            validate_transition(previous, next, LeaseState::Committed),
            Ok(())
        );

        let stale = LeaseBinding {
            generation: 1,
            ..next
        };
        assert_eq!(
            validate_transition(previous, stale, LeaseState::Committed),
            Err(ConformanceError::InvalidTransition)
        );

        let exhausted = binding(LeaseState::Prepared, u64::MAX);
        let wrapped = LeaseBinding {
            state: LeaseState::Committed,
            ..exhausted
        };
        assert_eq!(
            validate_transition(exhausted, wrapped, LeaseState::Committed),
            Err(ConformanceError::InvalidTransition)
        );
    }

    #[test]
    fn inventory_and_reconciliation_reject_identity_drift() {
        let previous = binding(LeaseState::Finalized, 4);
        let descriptor = ProviderDescriptor {
            contract_version: PROVIDER_CONTRACT_VERSION,
            provider_instance_id: previous.provider_instance_id,
        };
        validate_inventory_snapshot(
            descriptor,
            InventorySnapshot {
                provider_instance_id: descriptor.provider_instance_id,
                snapshot_id: Uuid::new_v4(),
            },
        )
        .expect("snapshot");
        validate_inventory_lease(descriptor, previous).expect("inventory lease");

        let quarantined = LeaseBinding {
            generation: 5,
            state: LeaseState::Quarantined,
            ..previous
        };
        validate_reconcile_result(previous, quarantined, ReconcileAction::Quarantine)
            .expect("quarantine transition");

        let wrong_provider = LeaseBinding {
            provider_instance_id: Uuid::new_v4(),
            ..previous
        };
        assert_eq!(
            validate_inventory_lease(descriptor, wrong_provider),
            Err(ConformanceError::InvalidInventory)
        );
    }

    #[test]
    fn inventory_rejects_released_leases_and_unbounded_continuations() {
        let released = binding(LeaseState::Released, 4);
        let descriptor = ProviderDescriptor {
            contract_version: PROVIDER_CONTRACT_VERSION,
            provider_instance_id: released.provider_instance_id,
        };
        assert_eq!(
            validate_inventory_lease(descriptor, released),
            Err(ConformanceError::InvalidInventory)
        );

        for page in [
            InventoryPage {
                leases: Vec::new(),
                next_cursor: Some("next".to_string()),
            },
            InventoryPage {
                leases: vec![InventoryLease {
                    binding: binding(LeaseState::Finalized, 4),
                }],
                next_cursor: Some(String::new()),
            },
            InventoryPage {
                leases: vec![InventoryLease {
                    binding: binding(LeaseState::Finalized, 4),
                }],
                next_cursor: Some("x".repeat(MAX_INVENTORY_CURSOR_BYTES + 1)),
            },
        ] {
            assert_eq!(
                validate_inventory_page(&page, 256),
                Err(ConformanceError::InvalidInventory)
            );
        }
    }

    #[test]
    fn reconciliation_rejects_impossible_source_states_and_generations() {
        let released = binding(LeaseState::Released, 4);
        let quarantined = LeaseBinding {
            generation: 5,
            state: LeaseState::Quarantined,
            ..released
        };
        assert_eq!(
            validate_reconcile_result(released, quarantined, ReconcileAction::Quarantine),
            Err(ConformanceError::InvalidTransition)
        );

        let prepared = binding(LeaseState::Prepared, 4);
        let finalized = LeaseBinding {
            generation: 5,
            state: LeaseState::Finalized,
            ..prepared
        };
        assert_eq!(
            validate_reconcile_result(
                prepared,
                finalized,
                ReconcileAction::Adopt {
                    backend_process: blaze_core::data_plane::BackendProcessIdentity {
                        pid: 12,
                        start_time_ticks: 34,
                    },
                },
            ),
            Err(ConformanceError::InvalidTransition)
        );

        let mut invalid_generation = binding(LeaseState::Finalized, 1);
        invalid_generation.context.generation = 2;
        let next = LeaseBinding {
            generation: 2,
            state: LeaseState::Quarantined,
            ..invalid_generation
        };
        assert_eq!(
            validate_reconcile_result(invalid_generation, next, ReconcileAction::Quarantine,),
            Err(ConformanceError::InvalidTransition)
        );
    }

    #[test]
    fn suspension_capture_and_restore_require_exact_identity_and_extents() {
        let previous = binding(LeaseState::Finalized, 4);
        let suspension_id = Uuid::new_v4();
        let suspension = ProviderSuspensionRef {
            provider_instance_id: previous.provider_instance_id,
            suspension_id,
            reference_id: Uuid::new_v4(),
            content_digest: format!("sha256:{}", "b".repeat(64)),
            source_lease_id: previous.context.lease_id,
            source_generation: 5,
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
        };
        let submission = SuspensionSubmission {
            binding: LeaseBinding {
                generation: 5,
                state: LeaseState::Finalized,
                ..previous
            },
            suspension: suspension.clone(),
        };
        validate_suspension_submission(previous, suspension_id, 4096, 8192, &submission)
            .expect("suspension capture");

        let context = RequestContext {
            instance_id: previous.context.instance_id,
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
        };
        let prepared = PreparedLease {
            binding: LeaseBinding {
                provider_instance_id: previous.provider_instance_id,
                context,
                generation: 1,
                state: LeaseState::Prepared,
            },
            resources: PreparedResources::SuspensionRestore {
                storage: Some(StorageSlot {
                    id: context.instance_id.to_string(),
                    rootfs_path: PathBuf::new(),
                    mem_path: PathBuf::new(),
                    mem_diff_path: PathBuf::new(),
                    rootfs_diff_path: PathBuf::new(),
                    instance_dir: PathBuf::new(),
                }),
                attachments: Vec::new(),
            },
        };
        validate_suspension_restore(
            ProviderCapabilities::default(),
            context,
            &suspension,
            4096,
            8192,
            &prepared,
        )
        .expect("suspension restore");
        assert_eq!(
            validate_suspension_restore(
                ProviderCapabilities::default(),
                context,
                &suspension,
                4097,
                8192,
                &prepared,
            ),
            Err(ConformanceError::InvalidSuspension)
        );

        let mut wrong = suspension.clone();
        wrong.guest_memory_bytes += 1;
        assert_eq!(
            validate_suspension_submission(
                previous,
                suspension_id,
                4096,
                8192,
                &SuspensionSubmission {
                    binding: submission.binding,
                    suspension: wrong,
                },
            ),
            Err(ConformanceError::InvalidSuspension)
        );
    }

    #[test]
    fn opened_restore_resources_require_the_matching_declared_capability() {
        let template_binding = binding(LeaseState::Prepared, 1);
        let template = PreparedLease {
            binding: template_binding,
            resources: PreparedResources::OpenedRestore {
                restore_payload_dir: PathBuf::from("payload"),
                attachments: opened_attachments(),
            },
        };
        assert_eq!(
            validate_prepared(
                ProviderCapabilities::default(),
                template_binding.context,
                true,
                4096,
                8192,
                &template,
            ),
            Err(ConformanceError::InvalidResources)
        );
        validate_prepared(
            ProviderCapabilities {
                opened_template_restore_resources: true,
                ..ProviderCapabilities::default()
            },
            template_binding.context,
            true,
            4096,
            8192,
            &template,
        )
        .expect("declared template attachments");

        let checkpoint_binding = binding(LeaseState::Prepared, 1);
        let checkpoint = ProviderCheckpointRef {
            provider_instance_id: checkpoint_binding.provider_instance_id,
            public_checkpoint_id: Uuid::new_v4(),
            reference_id: Uuid::new_v4(),
            content_digest: format!("sha256:{}", "c".repeat(64)),
            parent_reference_id: None,
            source_lease_id: Uuid::new_v4(),
            source_generation: 2,
        };
        let checkpoint_restore = PreparedLease {
            binding: checkpoint_binding,
            resources: PreparedResources::CheckpointRestore {
                storage: None,
                attachments: opened_attachments(),
            },
        };
        assert_eq!(
            validate_checkpoint_restore(
                ProviderCapabilities {
                    opened_template_restore_resources: true,
                    ..ProviderCapabilities::default()
                },
                checkpoint_binding.context,
                &checkpoint,
                4096,
                8192,
                &checkpoint_restore,
            ),
            Err(ConformanceError::InvalidResources)
        );
        validate_checkpoint_restore(
            ProviderCapabilities {
                opened_checkpoint_restore_resources: true,
                ..ProviderCapabilities::default()
            },
            checkpoint_binding.context,
            &checkpoint,
            4096,
            8192,
            &checkpoint_restore,
        )
        .expect("declared checkpoint attachments");

        let suspension_binding = binding(LeaseState::Prepared, 1);
        let suspension = ProviderSuspensionRef {
            provider_instance_id: suspension_binding.provider_instance_id,
            suspension_id: Uuid::new_v4(),
            reference_id: Uuid::new_v4(),
            content_digest: format!("sha256:{}", "d".repeat(64)),
            source_lease_id: Uuid::new_v4(),
            source_generation: 2,
            root_filesystem_bytes: 4096,
            guest_memory_bytes: 8192,
        };
        let suspension_restore = PreparedLease {
            binding: suspension_binding,
            resources: PreparedResources::SuspensionRestore {
                storage: None,
                attachments: opened_attachments(),
            },
        };
        assert_eq!(
            validate_suspension_restore(
                ProviderCapabilities {
                    opened_checkpoint_restore_resources: true,
                    ..ProviderCapabilities::default()
                },
                suspension_binding.context,
                &suspension,
                4096,
                8192,
                &suspension_restore,
            ),
            Err(ConformanceError::InvalidResources)
        );
        validate_suspension_restore(
            ProviderCapabilities {
                opened_suspension_restore_resources: true,
                ..ProviderCapabilities::default()
            },
            suspension_binding.context,
            &suspension,
            4096,
            8192,
            &suspension_restore,
        )
        .expect("declared suspension attachments");
    }

    #[test]
    fn capacity_and_drain_require_exact_scope_identity_and_accounting() {
        let descriptor = ProviderDescriptor {
            contract_version: PROVIDER_CONTRACT_VERSION,
            provider_instance_id: Uuid::new_v4(),
        };
        let class = blaze_provider_api::CapacityClass {
            root_filesystem_capacity_bytes: 4 * 1024 * 1024 * 1024,
            guest_memory_capacity_bytes: 512 * 1024 * 1024,
        };
        let scope = blaze_provider_api::CapacityScope {
            backend: blaze_core::backend::BackendKind::Firecracker,
            class_digest: class.digest(),
        };
        let request = CapacityRequest { scope };
        let snapshot = CapacitySnapshot {
            provider_instance_id: descriptor.provider_instance_id,
            scope,
            class,
            revision: 7,
            ready: 3,
            building: 1,
            in_use: 2,
            draining: 1,
            quarantined: 0,
            accepting_allocations: true,
        };
        validate_capacity_snapshot(descriptor, request, snapshot).expect("capacity snapshot");
        assert_eq!(snapshot.checked_total(), Some(7));

        let drain = DrainRequest {
            scope,
            operation_id: Uuid::new_v4(),
        };
        validate_drain_result(
            descriptor,
            drain,
            DrainResult {
                operation_id: drain.operation_id,
                removed_ready: 3,
                deferred_in_use: 2,
                snapshot: CapacitySnapshot {
                    revision: 8,
                    ready: 0,
                    building: 0,
                    in_use: 0,
                    draining: 2,
                    accepting_allocations: false,
                    ..snapshot
                },
            },
        )
        .expect("drain result");

        let invalid = CapacitySnapshot {
            ready: u64::MAX,
            building: 1,
            ..snapshot
        };
        assert_eq!(
            validate_capacity_snapshot(descriptor, request, invalid),
            Err(ConformanceError::InvalidCapacity)
        );

        let mismatched_class = CapacitySnapshot {
            class: blaze_provider_api::CapacityClass {
                root_filesystem_capacity_bytes: class.root_filesystem_capacity_bytes + 1,
                ..class
            },
            ..snapshot
        };
        assert_eq!(
            validate_capacity_snapshot(descriptor, request, mismatched_class),
            Err(ConformanceError::InvalidCapacity)
        );
    }
}
