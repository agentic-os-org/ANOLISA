//! Shared validation for the two repositories' atomic reconciliation writes.
use crate::{BindingStateSnapshot, BindingStateWrite, StoreError, WriteReceipt, WriteResult};
use asc_policy_types::{binding::BindingStatus, target::Presence};
use sha2::{Digest, Sha256};

/// Binds a write ID to its original condition and patch without saving request payloads.
/// # Errors
/// Returns `Invalid` when the condition or patch cannot be encoded.
pub fn write_digest(
    expected: &BindingStateSnapshot,
    write: &BindingStateWrite,
) -> Result<[u8; 32], StoreError> {
    let bytes = serde_json::to_vec(&(
        &expected.binding.spec.binding_id,
        expected.binding.spec.binding_revision,
        expected.status_version,
        &expected.binding.status,
        &expected.deployments,
        &write.next,
    ))
    .map_err(|_| StoreError::Invalid)?;
    Ok(Sha256::digest(bytes).into())
}

/// Validates a narrow write against the latest aggregate inside its repository transaction.
/// Callers must retain single-executor ownership for target observations.
/// # Errors
/// Rejects invalid state, oversized responsibilities and status version overflow.
pub fn apply_binding_write(
    current: &BindingStateSnapshot,
    expected: &BindingStateSnapshot,
    write: &BindingStateWrite,
) -> Result<(Option<BindingStateSnapshot>, WriteResult), StoreError> {
    if current.status_version < 1 || expected.status_version < 1 {
        return Err(StoreError::Invalid);
    }
    let conflict = || (Some(current.clone()), WriteResult::Conflict);
    if current.binding.spec.binding_id != expected.binding.spec.binding_id
        || current.binding.spec.binding_revision != expected.binding.spec.binding_revision
    {
        return Ok(conflict());
    }
    let matched = current.status_version == expected.status_version
        && current.binding.status == expected.binding.status;
    let Some(patch) = &write.next else {
        if !matched || current.deployments != expected.deployments {
            return Ok(conflict());
        }
        if current.binding.status.phase != BindingStatus::Deleting {
            return Err(StoreError::Invalid);
        }
        return Ok((
            None,
            WriteResult::Applied(WriteReceipt {
                status_version: None,
                status_applied: true,
            }),
        ));
    };
    if !matched && !patch.preserve_observations_on_conflict {
        return Ok(conflict());
    }
    let mut next = current.clone();
    if let Some(deployments) = &patch.deployments {
        // PAP never writes deployments. A changed base indicates another target writer.
        if current.deployments != expected.deployments {
            return Ok(conflict());
        }
        if deployments.len() > 32
            || serde_json::to_vec(deployments)
                .map_err(|_| StoreError::Invalid)?
                .len()
                > 1024 * 1024
        {
            return Err(StoreError::Invalid);
        }
        for (index, deployment) in deployments.iter().enumerate() {
            if deployment.presence == Presence::Absent
                || deployments[..index]
                    .iter()
                    .any(|d| d.target.same_identity(&deployment.target))
            {
                return Err(StoreError::Invalid);
            }
            if patch.preserve_observations_on_conflict
                && !current
                    .deployments
                    .iter()
                    .any(|d| d.target == deployment.target && d.revision == deployment.revision)
            {
                return Err(StoreError::Invalid);
            }
        }
        next.deployments.clone_from(deployments);
    }
    if let Some(status) = &patch.status {
        if status.phase == BindingStatus::Deleted {
            return Err(StoreError::Invalid);
        }
        if matched && *status != current.binding.status {
            next.status_version = current
                .status_version
                .checked_add(1)
                .ok_or(StoreError::Invalid)?;
            next.binding.status = status.clone();
        }
    }
    let receipt = WriteReceipt {
        status_version: Some(next.status_version),
        status_applied: matched,
    };
    Ok((Some(next), WriteResult::Applied(receipt)))
}
