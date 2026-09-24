//! Production ws-ckpt composition for the governed checkpoint profile.

use std::path::{Path, PathBuf};

use cosh_gateway::capability::{
    BoundExecutionOperation, DurableApprovalCoordinator, DurableApprovalOutcome,
    DurableApprovalResolution, ExecutionTarget, ExecutionTargetOutcome,
    GovernedExecutionCoordinator, GovernedExecutionError,
};
use cosh_gateway::daemon::{
    ApprovalCheckpointCreateResult, ApprovalCheckpointEvidence, ApprovalCheckpointPrepareResult,
    ApprovalCheckpointReconcileResult, ApprovalCheckpointRequest, BrokeredApprovalContext,
    BrokeredApprovalPlan, BrokeredExecutionDriver, BrokeredRecoveryContext, BrokeredResolution,
    BrokeredResolutionContext, BrokeredResolutionSource, PreRuntimeCheckpointBinding,
    PreRuntimeCheckpointCreateResult, PreRuntimeCheckpointDriver, PreRuntimeCheckpointEvidence,
    PreRuntimeCheckpointReconcileRequest, PreRuntimeCheckpointReconcileResult,
    PreRuntimeCheckpointRequest, TaskSnapshotChange, TaskSnapshotDriver,
    TaskSnapshotProviderPreview, TaskSnapshotProviderRequest, TaskSnapshotProviderSwitch,
    TaskSnapshotProviderSwitchResult,
};
use cosh_gateway::runtime::TrustedWorkspaceResolver;
use cosh_gateway::storage::{ExecutionClaim, ExecutionRecord, LedgerCommand, SqliteTaskStore};
use cosh_gateway_contracts::capability::{
    ApprovalRequest, BrokeredOperation, RuntimeExecutionFence, WorkspaceCheckpointCreateV1,
};
use cosh_gateway_contracts::common::{
    BoundedOpaque, BoundedText, Digest, IdempotencyKey, TargetRef, WorkspaceRef,
};
use cosh_gateway_contracts::error::{ContractError, ErrorCategory};
use cosh_gateway_contracts::ids::{ApprovalId, CheckpointId, ExecutionId, PermitId};
use cosh_gateway_contracts::profile::{CapabilityProviderId, GatewayCapabilityProfile};
use cosh_gateway_contracts::runtime::{
    BrokeredExecutionDelivery, BrokeredExecutionOutcome, BrokeredOperationResult,
    WorkspaceCheckpointCreateV1Outcome, WorkspaceCheckpointCreateV1Result,
};
use cosh_platform::checkpoint::{CkptClient, CkptRequestEffect};
use cosh_types::checkpoint::{
    ChangeType, GuardedCheckpointEvidenceV2, GuardedCheckpointOutcomeV2, WorkspaceGenerationTokenV2,
};
use cosh_types::error::{CoshError, ErrorCode};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

mod trust;

use trust::{
    current_time_ms, open_audit_file, verify_socket_trust, AuditFile, CheckpointAdmissionError,
    CheckpointBinding, FileAuditGate,
};

const TARGET_DOMAIN: &[u8] = b"cosh.gateway.ws-ckpt-target.v2\0";
const RECEIPT_DOMAIN: &[u8] = b"cosh.gateway.ws-ckpt-receipt.v2\0";
const PRE_RUNTIME_OPERATION_DOMAIN: &[u8] = b"cosh.gateway.pre-runtime-checkpoint.v1\0";
const PRE_RUNTIME_EVIDENCE_DOMAIN: &[u8] = b"cosh.gateway.pre-runtime-evidence.v1\0";
const APPROVAL_OPERATION_DOMAIN: &[u8] = b"cosh.gateway.approval-checkpoint.v1\0";
const APPROVAL_EVIDENCE_DOMAIN: &[u8] = b"cosh.gateway.approval-evidence.v1\0";
const TASK_SWITCH_RECOVERY_DOMAIN: &[u8] = b"cosh.gateway.task-switch-recovery.v1\0";
const TASK_SNAPSHOT_PREVIEW_DOMAIN: &[u8] = b"cosh.gateway.task-snapshot-preview.v1\0";
const BINDING_VERSION: u16 = 2;
const POLICY_REVISION: u64 = 1;

pub(crate) struct CheckpointDriver {
    endpoint: CheckpointEndpoint,
    profile: GatewayCapabilityProfile,
    target: TargetRef,
    audit_file: AuditFile,
}

struct CheckpointEndpoint {
    client: CkptClient,
    socket_path: PathBuf,
    socket_identity: (u64, u64, u32),
    registration_path: String,
    owner_uid: u32,
}

pub(crate) struct PreRuntimeCheckpointAdapter {
    endpoint: CheckpointEndpoint,
    workspace: WorkspaceRef,
}

pub(crate) struct TaskSnapshotAdapter {
    endpoint: CheckpointEndpoint,
    workspaces: TrustedWorkspaceResolver,
}

impl CheckpointEndpoint {
    fn admit(
        socket_path: PathBuf,
        registration_path: &Path,
        owner_uid: u32,
    ) -> Result<Self, CheckpointAdmissionError> {
        if !socket_path.is_absolute() {
            return Err(CheckpointAdmissionError::Socket);
        }
        let socket_identity = verify_socket_trust(&socket_path, owner_uid)?;
        let registration_path = registration_path
            .to_str()
            .filter(|_| registration_path.is_absolute())
            .ok_or(CheckpointAdmissionError::Workspace)?
            .to_owned();
        let client = CkptClient::with_timeout(
            socket_path
                .to_str()
                .ok_or(CheckpointAdmissionError::Socket)?,
            30_000,
        )
        .require_trusted_peer(owner_uid);
        let identity = client
            .workspace_identity_v2(&registration_path)
            .map_err(|_| CheckpointAdmissionError::Identity)?;
        if identity.registered_path != registration_path {
            return Err(CheckpointAdmissionError::Identity);
        }
        Ok(Self {
            client,
            socket_path,
            socket_identity,
            registration_path,
            owner_uid,
        })
    }

    fn resolve_binding(&self) -> Result<CheckpointBinding, ContractError> {
        self.verify_socket_unchanged()?;
        let identity = self
            .client
            .workspace_identity_v2(&self.registration_path)
            .map_err(|_| checkpoint_error("checkpoint_identity_unavailable", false))?;
        Ok(CheckpointBinding {
            version: BINDING_VERSION,
            ws_id: identity.ws_id,
            registered_path: identity.registered_path,
            generation: identity.generation.into_bytes(),
            owner_uid: self.owner_uid,
        })
    }

    fn verify_socket_unchanged(&self) -> Result<(), ContractError> {
        let current = verify_socket_trust(&self.socket_path, self.owner_uid)
            .map_err(|_| checkpoint_error("checkpoint_socket_changed", false))?;
        if current != self.socket_identity {
            return Err(checkpoint_error("checkpoint_socket_changed", false));
        }
        Ok(())
    }
}

include!("checkpoint/pre_runtime.rs");

include!("checkpoint/task_snapshot.rs");

include!("checkpoint/pre_runtime_driver.rs");

include!("checkpoint/brokered.rs");

fn evidence_outcome(
    operation: &CheckpointOperation,
    evidence: GuardedCheckpointEvidenceV2,
) -> ExecutionTargetOutcome {
    if evidence.caller_uid != operation.binding.owner_uid
        || evidence.registered_path != operation.binding.registered_path
    {
        return ExecutionTargetOutcome::Unknown {
            safe_detail: bounded(
                "Checkpoint evidence did not match the admitted actor or workspace",
            ),
        };
    }
    let evidence_receipt = match serde_json::to_vec(&evidence) {
        Ok(encoded) => encoded,
        Err(_) => {
            return ExecutionTargetOutcome::Unknown {
                safe_detail: bounded("Checkpoint evidence could not be bound into its receipt"),
            }
        }
    };
    let typed = match evidence.outcome {
        GuardedCheckpointOutcomeV2::Created { snapshot_id } => {
            let Ok(snapshot_id) = BoundedOpaque::new(snapshot_id) else {
                return ExecutionTargetOutcome::Unknown {
                    safe_detail: bounded(
                        "Checkpoint evidence exceeded its bounded result contract",
                    ),
                };
            };
            WorkspaceCheckpointCreateV1Outcome::Created { snapshot_id }
        }
        GuardedCheckpointOutcomeV2::Skipped { reason } => {
            let Ok(reason) = BoundedText::new(reason) else {
                return ExecutionTargetOutcome::Unknown {
                    safe_detail: bounded(
                        "Checkpoint skip evidence exceeded its bounded result contract",
                    ),
                };
            };
            WorkspaceCheckpointCreateV1Outcome::Skipped { reason }
        }
    };
    let receipt_digest = digest_parts(&[
        RECEIPT_DOMAIN,
        operation.target_identity_digest.as_str().as_bytes(),
        operation.operation_digest.as_str().as_bytes(),
        operation.checkpoint_id.as_str().as_bytes(),
        &evidence.generation.into_bytes(),
        &evidence_receipt,
    ])
    .unwrap_or_else(|_| operation.operation_digest.clone());
    ExecutionTargetOutcome::Conclusive {
        succeeded: true,
        receipt_digest,
        safe_detail: bounded("Workspace checkpoint completed with durable daemon evidence"),
        typed_result: Some(BrokeredOperationResult::WorkspaceCheckpointCreateV1(
            WorkspaceCheckpointCreateV1Result {
                checkpoint_id: operation.checkpoint_id.clone(),
                outcome: typed,
            },
        )),
    }
}

fn known_failure(operation: &CheckpointOperation, message: &str) -> ExecutionTargetOutcome {
    let receipt_digest = digest_parts(&[
        RECEIPT_DOMAIN,
        operation.target_identity_digest.as_str().as_bytes(),
        operation.operation_digest.as_str().as_bytes(),
        operation.checkpoint_id.as_str().as_bytes(),
        message.as_bytes(),
    ])
    .unwrap_or_else(|_| operation.operation_digest.clone());
    ExecutionTargetOutcome::Conclusive {
        succeeded: false,
        receipt_digest,
        safe_detail: bounded(message),
        typed_result: None,
    }
}

fn ledger_command<T: Serialize>(
    actor_id: &cosh_gateway_contracts::ids::ActorId,
    idempotency_key: IdempotencyKey,
    domain: &str,
    value: &T,
    committed_at_ms: u64,
) -> Result<LedgerCommand, ContractError> {
    Ok(LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key,
        command_digest: digest_parts(&[
            b"cosh.gateway.checkpoint-command.v1\0",
            domain.as_bytes(),
            &serde_json::to_vec(value)
                .map_err(|_| checkpoint_error("checkpoint_internal", false))?,
        ])?,
        committed_at_ms,
    })
}

fn internal_key(prefix: &str, value: &str) -> Result<IdempotencyKey, ContractError> {
    IdempotencyKey::new(format!("checkpoint-{prefix}-{value}"))
        .map_err(|_| checkpoint_error("checkpoint_internal", false))
}

fn digest_parts(parts: &[&[u8]]) -> Result<Digest, ContractError> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    Digest::parse(format!("{:x}", hasher.finalize()))
        .map_err(|_| checkpoint_error("checkpoint_internal", false))
}

fn digest_bytes(digest: &Digest) -> Result<[u8; 32], ContractError> {
    let bytes = digest.as_str().as_bytes();
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        decoded[index] = (hex(pair[0])? << 4) | hex(pair[1])?;
    }
    Ok(decoded)
}

fn generation_bytes(generation: &BoundedOpaque) -> Result<[u8; 32], ContractError> {
    let bytes = generation.as_str().as_bytes();
    if bytes.len() != 64 {
        return Err(pre_runtime_checkpoint_error(
            "checkpoint_generation_invalid",
            ErrorCategory::InvalidRequest,
            false,
            "Durable checkpoint generation evidence was invalid",
        ));
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        decoded[index] = (hex(pair[0])? << 4) | hex(pair[1])?;
    }
    Ok(decoded)
}

fn hex(value: u8) -> Result<u8, ContractError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(checkpoint_error("checkpoint_digest_invalid", false)),
    }
}

fn checkpoint_error(code: &str, retryable: bool) -> ContractError {
    ContractError::new(
        code,
        ErrorCategory::PolicyDenied,
        retryable,
        "The governed checkpoint operation could not be completed safely",
    )
    .unwrap_or_else(|_| unreachable!("static checkpoint errors are bounded"))
}

// Codes come only from this static table or the locally produced
// `guarded_rollback_rejection` detail name, never from remote text; the
// message reuses the already-redacted `CoshError` text.
fn task_snapshot_failure(fallback_code: &str, failure: &CoshError) -> ContractError {
    let (code, category) = match failure.code {
        ErrorCode::CheckpointDaemonUnavailable => (
            "checkpoint_daemon_unavailable".to_owned(),
            ErrorCategory::RuntimeUnavailable,
        ),
        ErrorCode::Timeout => (
            "checkpoint_daemon_timeout".to_owned(),
            ErrorCategory::Transport,
        ),
        ErrorCode::CheckpointProtocolError => (
            "checkpoint_daemon_protocol_mismatch".to_owned(),
            ErrorCategory::Transport,
        ),
        ErrorCode::PermissionDenied => (
            "checkpoint_caller_denied".to_owned(),
            ErrorCategory::PolicyDenied,
        ),
        ErrorCode::InvalidInput => (
            "checkpoint_invalid_input".to_owned(),
            ErrorCategory::InvalidRequest,
        ),
        ErrorCode::CheckpointNotFound => (
            "checkpoint_target_not_found".to_owned(),
            ErrorCategory::NotFound,
        ),
        ErrorCode::CheckpointRestoreFailed => {
            (rollback_rejection_code(failure), ErrorCategory::Conflict)
        }
        _ => return checkpoint_error(fallback_code, false),
    };
    let mut message = failure.message.clone();
    if failure.code == ErrorCode::CheckpointProtocolError {
        message.push_str(
            "; the ws-ckpt daemon may be older than the guarded snapshot protocol; upgrade ws-ckpt and retry",
        );
    } else if let Some(hint) = &failure.hint {
        message.push_str("; ");
        message.push_str(hint);
    }
    // A protocol mismatch is not worth retrying against the same daemon.
    let retryable = failure.recoverable && failure.code != ErrorCode::CheckpointProtocolError;
    ContractError::new(code, category, retryable, message)
        .unwrap_or_else(|_| checkpoint_error(fallback_code, false))
}

fn rollback_rejection_code(failure: &CoshError) -> String {
    let Some(name) = failure
        .details
        .as_ref()
        .and_then(|details| details.get("guarded_rollback_rejection"))
        .and_then(serde_json::Value::as_str)
    else {
        return "checkpoint_switch_rejected".to_owned();
    };
    let code = format!("checkpoint_switch_rejected_{name}");
    if cosh_gateway_contracts::error::ErrorCode::parse(&code).is_ok() {
        code
    } else {
        "checkpoint_switch_rejected".to_owned()
    }
}

fn pre_runtime_checkpoint_error(
    code: &str,
    category: ErrorCategory,
    retryable: bool,
    message: &str,
) -> ContractError {
    ContractError::new(code, category, retryable, message)
        .unwrap_or_else(|_| unreachable!("static pre-Runtime checkpoint errors are bounded"))
}

fn bounded_text(message: &str) -> Result<BoundedText, ContractError> {
    BoundedText::new(message).map_err(|_| {
        pre_runtime_checkpoint_error(
            "checkpoint_internal",
            ErrorCategory::Internal,
            false,
            "Checkpoint result exceeded its bounded contract",
        )
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn bounded(message: &str) -> Option<BoundedText> {
    BoundedText::new(message).ok()
}

#[cfg(test)]
#[path = "checkpoint/tests.rs"]
mod tests;
