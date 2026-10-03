//! Side-effect-fenced checkpoint operations for protocol V2.

use std::path::{Component, Path};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use ws_ckpt_common::{
    validate_checkpoint_id_v2, validate_workspace_id_v2, ErrorCode, GuardedCheckpointEvidenceV2,
    GuardedCheckpointOutcomeV2, GuardedCheckpointRejectionCodeV2, GuardedRollbackEvidenceV2,
    GuardedRollbackOutcomeV2, GuardedRollbackRejectionCodeV2, Response, SnapshotMeta,
    WorkspaceGenerationTokenV2, GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2,
    GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2, LIVE_CHILD,
};

use crate::state::DaemonState;

pub(crate) async fn workspace_identity(
    state: &Arc<DaemonState>,
    registration_path: &str,
) -> Response {
    let path = Path::new(registration_path);
    if let Err(message) = validate_registration_path(registration_path) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
            message,
        );
    }

    // This is deliberately an exact map lookup. Identity discovery must not
    // canonicalize, initialize, adopt, or repair caller-supplied paths.
    let Some(candidate) = state.wsid_for_exact_registration_path(path) else {
        return workspace_not_found(registration_path);
    };
    if let Err(message) = validate_workspace_id_v2(&candidate) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidWorkspaceId,
            message,
        );
    }
    let _wsid_guard = state.lock_wsid(&candidate).await;
    let Some(workspace) = state.get_by_wsid(&candidate) else {
        return workspace_not_found(registration_path);
    };
    if !state.exact_registration_is_current(path, &candidate, &workspace) {
        return workspace_not_found(registration_path);
    }
    if !state.registration_is_live(&candidate, path).await {
        return workspace_not_found(registration_path);
    }

    let registered_path = {
        let workspace = workspace.read().await;
        if workspace.path.to_str() != Some(registration_path) {
            return workspace_not_found(registration_path);
        }
        workspace.path.to_string_lossy().into_owned()
    };
    let generation = match state.backend.live_generation(&candidate).await {
        Ok(generation) => generation,
        Err(error) => {
            return rejected(
                GuardedCheckpointRejectionCodeV2::DaemonNotReady,
                format!("failed to read live workspace generation: {error:#}"),
            )
        }
    };

    Response::WorkspaceIdentityV2Ok {
        protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
        ws_id: candidate,
        registered_path,
        generation,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn checkpoint(
    state: &Arc<DaemonState>,
    caller_uid: Option<u32>,
    ws_id: &str,
    expected_generation: WorkspaceGenerationTokenV2,
    checkpoint_id: &str,
    operation_digest: [u8; 32],
    message: Option<String>,
    metadata: Option<String>,
    pin: bool,
) -> Response {
    let caller_uid = match caller_uid {
        Some(uid) => uid,
        None => {
            return rejected(
                GuardedCheckpointRejectionCodeV2::PeerCredentialsUnavailable,
                "guarded checkpoint requires kernel peer credentials",
            )
        }
    };
    if let Err(message) = validate_workspace_id_v2(ws_id) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidWorkspaceId,
            message,
        );
    }
    if let Err(message) = validate_checkpoint_id_v2(checkpoint_id) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidCheckpointId,
            message,
        );
    }
    let parsed_metadata = match metadata {
        Some(value) => match serde_json::from_str(&value) {
            Ok(value) => Some(value),
            Err(error) => {
                return rejected(
                    GuardedCheckpointRejectionCodeV2::InvalidMetadata,
                    format!("metadata is not valid JSON: {error}"),
                )
            }
        },
        None => None,
    };

    let _wsid_guard = state.lock_wsid(ws_id).await;
    let Some(workspace) = state.get_by_wsid(ws_id) else {
        return workspace_not_found(ws_id);
    };
    let mut workspace = workspace.write().await;
    if workspace.ws_id != ws_id {
        return workspace_not_found(ws_id);
    }
    if !state.registration_is_live(ws_id, &workspace.path).await {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
            "registered workspace path no longer resolves to the live subvolume",
        );
    }

    let generation = match state.backend.live_generation(ws_id).await {
        Ok(generation) => generation,
        Err(error) => {
            return rejected(
                GuardedCheckpointRejectionCodeV2::DaemonNotReady,
                format!("failed to read live workspace generation: {error:#}"),
            )
        }
    };
    if generation != expected_generation {
        return rejected(
            GuardedCheckpointRejectionCodeV2::GenerationMismatch,
            "workspace generation no longer matches the guarded request",
        );
    }

    if let Some(evidence) = workspace.index.governed_evidence.get(checkpoint_id) {
        if evidence_matches(
            evidence,
            ws_id,
            expected_generation,
            checkpoint_id,
            operation_digest,
            caller_uid,
        ) && evidence_is_visible(&workspace.index, evidence)
        {
            return Response::GuardedCheckpointV2Ok {
                evidence: evidence.clone(),
            };
        }
        return rejected(
            GuardedCheckpointRejectionCodeV2::OperationConflict,
            "checkpoint id is already bound to a different guarded operation",
        );
    }
    if workspace.index.snapshots.contains_key(checkpoint_id) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::OperationConflict,
            "checkpoint id already exists without matching guarded evidence",
        );
    }

    let mut next_index = match index_with_evidence_slot(&workspace.index) {
        Some(index) => index,
        None => {
            return rejected(
                GuardedCheckpointRejectionCodeV2::EvidenceCapacityReached,
                "guarded evidence capacity is occupied by visible snapshots",
            )
        }
    };

    if !state.check_workspace_quiescent(ws_id).await {
        return rejected(
            GuardedCheckpointRejectionCodeV2::WriteLockConflict,
            "workspace has active write operations; retry after it becomes quiescent",
        );
    }

    let Some(registered_path) = workspace.path.to_str().map(str::to_owned) else {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
            "registered workspace path is not valid UTF-8",
        );
    };

    if let Err(error) = state.backend.create_snapshot(ws_id, checkpoint_id).await {
        return backend_effect_error(format!(
            "guarded checkpoint backend operation failed; reconcile before retrying: {error:#}"
        ));
    }

    let evidence = evidence(
        ws_id,
        registered_path,
        expected_generation,
        checkpoint_id,
        operation_digest,
        caller_uid,
        GuardedCheckpointOutcomeV2::Created {
            snapshot_id: checkpoint_id.to_string(),
        },
    );
    if let Some(old_head) = next_index.head.clone() {
        if let Some(head) = next_index.snapshots.get_mut(&old_head) {
            head.child_ids.retain(|child| child != LIVE_CHILD);
            head.child_ids.push(checkpoint_id.to_string());
        }
    }
    next_index.snapshots.insert(
        checkpoint_id.to_string(),
        SnapshotMeta {
            message,
            metadata: parsed_metadata,
            pinned: pin,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: next_index.head.clone(),
            child_ids: vec![LIVE_CHILD.to_string()],
        },
    );
    next_index.head = Some(checkpoint_id.to_string());
    next_index
        .governed_evidence
        .insert(checkpoint_id.to_string(), evidence.clone());

    if let Err(error) = crate::index_store::save_durable(&state.index_dir(ws_id), &next_index).await
    {
        return backend_effect_error(format!(
            "snapshot was created but durable evidence save failed; reconcile before retrying: {error:#}"
        ));
    }
    workspace.index = next_index;
    Response::GuardedCheckpointV2Ok { evidence }
}

pub(crate) async fn checkpoint_evidence(
    state: &Arc<DaemonState>,
    caller_uid: Option<u32>,
    ws_id: &str,
    expected_generation: WorkspaceGenerationTokenV2,
    checkpoint_id: &str,
    operation_digest: [u8; 32],
) -> Response {
    let caller_uid = match caller_uid {
        Some(uid) => uid,
        None => {
            return rejected(
                GuardedCheckpointRejectionCodeV2::PeerCredentialsUnavailable,
                "checkpoint evidence requires kernel peer credentials",
            )
        }
    };
    if let Err(message) = validate_workspace_id_v2(ws_id) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidWorkspaceId,
            message,
        );
    }
    if let Err(message) = validate_checkpoint_id_v2(checkpoint_id) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::InvalidCheckpointId,
            message,
        );
    }

    let _wsid_guard = state.lock_wsid(ws_id).await;
    let Some(workspace) = state.get_by_wsid(ws_id) else {
        return workspace_not_found(ws_id);
    };
    let workspace = workspace.read().await;
    let Some(evidence) = workspace.index.governed_evidence.get(checkpoint_id) else {
        return Response::CheckpointEvidenceV2Ok { evidence: None };
    };
    if evidence.caller_uid != caller_uid {
        return rejected(
            GuardedCheckpointRejectionCodeV2::CallerMismatch,
            "stored checkpoint evidence belongs to a different caller",
        );
    }
    if !evidence_matches(
        evidence,
        ws_id,
        expected_generation,
        checkpoint_id,
        operation_digest,
        caller_uid,
    ) {
        return rejected(
            GuardedCheckpointRejectionCodeV2::OperationConflict,
            "stored checkpoint evidence does not match the requested operation",
        );
    }

    Response::CheckpointEvidenceV2Ok {
        evidence: evidence_is_visible(&workspace.index, evidence).then(|| evidence.clone()),
    }
}

pub(crate) async fn rollback_preview(
    state: &Arc<DaemonState>,
    caller_uid: Option<u32>,
    registered_path: &str,
    ws_id: &str,
    expected_generation: WorkspaceGenerationTokenV2,
    target_snapshot_id: &str,
) -> Response {
    let caller_uid = match rollback_caller_uid(caller_uid, "guarded rollback preview") {
        Ok(uid) => uid,
        Err(response) => return *response,
    };
    if let Err(response) =
        validate_rollback_request(registered_path, ws_id, target_snapshot_id, None)
    {
        return *response;
    }

    let _wsid_guard = state.lock_wsid(ws_id).await;
    let Some(workspace_arc) = state.get_by_wsid(ws_id) else {
        return rollback_workspace_not_found(ws_id);
    };
    let workspace = workspace_arc.read().await;
    if let Err(response) = validate_rollback_workspace(
        state,
        &workspace_arc,
        &workspace,
        registered_path,
        ws_id,
        target_snapshot_id,
    )
    .await
    {
        return *response;
    }
    let generation = match state.backend.live_generation(ws_id).await {
        Ok(generation) => generation,
        Err(error) => {
            return rollback_rejected(
                GuardedRollbackRejectionCodeV2::DaemonNotReady,
                format!("failed to read live workspace generation: {error:#}"),
            )
        }
    };
    if generation != expected_generation {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::GenerationMismatch,
            "workspace generation no longer matches the guarded preview",
        );
    }
    let changes = match state.backend.diff(ws_id, target_snapshot_id, None).await {
        Ok(changes) => changes,
        Err(error) => {
            return rollback_rejected(
                GuardedRollbackRejectionCodeV2::DaemonNotReady,
                format!("failed to compute guarded rollback preview: {error:#}"),
            )
        }
    };
    let diff_digest = rollback_diff_digest(&changes);

    Response::GuardedRollbackPreviewV2Ok {
        protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
        registered_path: registered_path.to_string(),
        ws_id: ws_id.to_string(),
        generation,
        target_snapshot_id: target_snapshot_id.to_string(),
        diff_digest,
        changes,
        caller_uid,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn rollback(
    state: &Arc<DaemonState>,
    caller_uid: Option<u32>,
    registered_path: &str,
    ws_id: &str,
    expected_generation: WorkspaceGenerationTokenV2,
    target_snapshot_id: &str,
    expected_diff_digest: [u8; 32],
    operation_id: &str,
    operation_digest: [u8; 32],
) -> Response {
    let caller_uid = match rollback_caller_uid(caller_uid, "guarded rollback") {
        Ok(uid) => uid,
        Err(response) => return *response,
    };
    if let Err(response) = validate_rollback_request(
        registered_path,
        ws_id,
        target_snapshot_id,
        Some(operation_id),
    ) {
        return *response;
    }

    let _wsid_guard = state.lock_wsid(ws_id).await;
    let Some(workspace_arc) = state.get_by_wsid(ws_id) else {
        return rollback_workspace_not_found(ws_id);
    };
    let mut workspace = workspace_arc.write().await;

    if let Some(evidence) = workspace.index.guarded_rollbacks.get(operation_id) {
        if evidence.caller_uid != caller_uid {
            return rollback_rejected(
                GuardedRollbackRejectionCodeV2::CallerMismatch,
                "stored rollback evidence belongs to a different caller",
            );
        }
        if !rollback_evidence_matches(
            evidence,
            registered_path,
            ws_id,
            expected_generation,
            target_snapshot_id,
            expected_diff_digest,
            operation_id,
            operation_digest,
            caller_uid,
        ) {
            return rollback_rejected(
                GuardedRollbackRejectionCodeV2::OperationConflict,
                "operation id is already bound to a different guarded rollback",
            );
        }
        return rollback_evidence_response(evidence.clone());
    }

    if workspace.index.guarded_rollbacks.len() >= GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2 {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::EvidenceCapacityReached,
            "guarded rollback evidence capacity is exhausted",
        );
    }
    if let Err(response) = validate_rollback_workspace(
        state,
        &workspace_arc,
        &workspace,
        registered_path,
        ws_id,
        target_snapshot_id,
    )
    .await
    {
        return *response;
    }
    let generation = match state.backend.live_generation(ws_id).await {
        Ok(generation) => generation,
        Err(error) => {
            return rollback_rejected(
                GuardedRollbackRejectionCodeV2::DaemonNotReady,
                format!("failed to read live workspace generation: {error:#}"),
            )
        }
    };
    if generation != expected_generation {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::GenerationMismatch,
            "workspace generation no longer matches the guarded rollback",
        );
    }
    if !state.check_workspace_quiescent(ws_id).await {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::WriteLockConflict,
            "workspace has active write operations; retry after it becomes quiescent",
        );
    }
    if let Some(response) = crate::util::guard_cwd_occupants(registered_path).await {
        return rollback_cwd_rejection(response);
    }

    let evidence = GuardedRollbackEvidenceV2 {
        ws_id: ws_id.to_string(),
        registered_path: registered_path.to_string(),
        expected_generation,
        target_snapshot_id: target_snapshot_id.to_string(),
        expected_diff_digest,
        operation_id: operation_id.to_string(),
        operation_digest,
        caller_uid,
        outcome: GuardedRollbackOutcomeV2::Started,
    };
    let mut started_index = workspace.index.clone();
    started_index
        .guarded_rollbacks
        .insert(operation_id.to_string(), evidence.clone());
    if let Err(error) =
        crate::index_store::save_durable(&state.index_dir(ws_id), &started_index).await
    {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::DaemonNotReady,
            format!("failed to durably reserve guarded rollback operation: {error:#}"),
        );
    }
    workspace.index = started_index;

    // Revalidate all mutable provider state after the durable intent write. The
    // digest comparison is the final completed await before backend rollback,
    // keeping preview validation and the destructive call in one write lock.
    if let Err(response) = validate_rollback_workspace(
        state,
        &workspace_arc,
        &workspace,
        registered_path,
        ws_id,
        target_snapshot_id,
    )
    .await
    {
        return abort_reserved_rollback(state, &mut workspace, operation_id, *response).await;
    }
    let generation = match state.backend.live_generation(ws_id).await {
        Ok(generation) => generation,
        Err(error) => {
            let response = rollback_rejected(
                GuardedRollbackRejectionCodeV2::DaemonNotReady,
                format!("failed to revalidate live workspace generation: {error:#}"),
            );
            return abort_reserved_rollback(state, &mut workspace, operation_id, response).await;
        }
    };
    if generation != expected_generation {
        let response = rollback_rejected(
            GuardedRollbackRejectionCodeV2::GenerationMismatch,
            "workspace generation changed while reserving the rollback operation",
        );
        return abort_reserved_rollback(state, &mut workspace, operation_id, response).await;
    }
    let changes = match state.backend.diff(ws_id, target_snapshot_id, None).await {
        Ok(changes) => changes,
        Err(error) => {
            let response = rollback_rejected(
                GuardedRollbackRejectionCodeV2::DaemonNotReady,
                format!("failed to recompute guarded rollback diff: {error:#}"),
            );
            return abort_reserved_rollback(state, &mut workspace, operation_id, response).await;
        }
    };
    if rollback_diff_digest(&changes) != expected_diff_digest {
        let response = rollback_rejected(
            GuardedRollbackRejectionCodeV2::DiffMismatch,
            "live workspace changes no longer match the guarded rollback preview",
        );
        return abort_reserved_rollback(state, &mut workspace, operation_id, response).await;
    }

    if let Err(error) = state.backend.rollback(ws_id, target_snapshot_id).await {
        return mark_rollback_unknown(
            state,
            &mut workspace,
            operation_id,
            format!("backend rollback returned an uncertain failure: {error:#}"),
        )
        .await;
    }
    let resulting_generation = match state.backend.live_generation(ws_id).await {
        Ok(generation) => generation,
        Err(error) => {
            return mark_rollback_unknown(
                state,
                &mut workspace,
                operation_id,
                format!("rollback completed but resulting generation could not be read: {error:#}"),
            )
            .await;
        }
    };

    update_live_head(&mut workspace.index, target_snapshot_id);
    let succeeded = GuardedRollbackOutcomeV2::Succeeded {
        resulting_generation,
    };
    let completed_evidence = match workspace.index.guarded_rollbacks.get_mut(operation_id) {
        Some(evidence) => {
            evidence.outcome = succeeded;
            evidence.clone()
        }
        None => {
            let mut uncertain = evidence;
            uncertain.outcome = GuardedRollbackOutcomeV2::Unknown {
                reason: "rollback completed but its durable reservation disappeared".to_string(),
            };
            return Response::GuardedRollbackV2Uncertain {
                evidence: uncertain,
            };
        }
    };
    if let Err(error) =
        crate::index_store::save_durable(&state.index_dir(ws_id), &workspace.index).await
    {
        return mark_rollback_unknown(
            state,
            &mut workspace,
            operation_id,
            format!(
                "rollback completed but durable success evidence could not be saved: {error:#}"
            ),
        )
        .await;
    }

    rollback_evidence_response(completed_evidence)
}

pub(crate) async fn rollback_evidence(
    state: &Arc<DaemonState>,
    caller_uid: Option<u32>,
    ws_id: &str,
    operation_id: &str,
    operation_digest: [u8; 32],
) -> Response {
    let caller_uid = match rollback_caller_uid(caller_uid, "guarded rollback evidence") {
        Ok(uid) => uid,
        Err(response) => return *response,
    };
    if let Err(message) = validate_workspace_id_v2(ws_id) {
        return rollback_rejected(GuardedRollbackRejectionCodeV2::InvalidWorkspaceId, message);
    }
    if let Err(message) = validate_checkpoint_id_v2(operation_id) {
        return rollback_rejected(GuardedRollbackRejectionCodeV2::InvalidOperationId, message);
    }
    let _wsid_guard = state.lock_wsid(ws_id).await;
    let Some(workspace) = state.get_by_wsid(ws_id) else {
        return rollback_workspace_not_found(ws_id);
    };
    let workspace = workspace.read().await;
    let Some(evidence) = workspace.index.guarded_rollbacks.get(operation_id) else {
        return Response::GuardedRollbackEvidenceV2Ok { evidence: None };
    };
    if evidence.caller_uid != caller_uid {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::CallerMismatch,
            "stored rollback evidence belongs to a different caller",
        );
    }
    if evidence.ws_id != ws_id
        || evidence.operation_id != operation_id
        || evidence.operation_digest != operation_digest
    {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::OperationConflict,
            "stored rollback evidence does not match the requested operation",
        );
    }
    Response::GuardedRollbackEvidenceV2Ok {
        evidence: Some(evidence.clone()),
    }
}

fn rollback_caller_uid(caller_uid: Option<u32>, operation: &str) -> Result<u32, Box<Response>> {
    caller_uid.ok_or_else(|| {
        Box::new(rollback_rejected(
            GuardedRollbackRejectionCodeV2::PeerCredentialsUnavailable,
            format!("{operation} requires kernel peer credentials"),
        ))
    })
}

fn validate_rollback_request(
    registered_path: &str,
    ws_id: &str,
    target_snapshot_id: &str,
    operation_id: Option<&str>,
) -> Result<(), Box<Response>> {
    if let Err(message) = validate_registration_path(registered_path) {
        return Err(Box::new(rollback_rejected(
            GuardedRollbackRejectionCodeV2::InvalidRegistrationPath,
            message,
        )));
    }
    if let Err(message) = validate_workspace_id_v2(ws_id) {
        return Err(Box::new(rollback_rejected(
            GuardedRollbackRejectionCodeV2::InvalidWorkspaceId,
            message,
        )));
    }
    if let Err(message) = validate_checkpoint_id_v2(target_snapshot_id) {
        return Err(Box::new(rollback_rejected(
            GuardedRollbackRejectionCodeV2::InvalidSnapshotId,
            message,
        )));
    }
    if let Some(operation_id) = operation_id {
        if let Err(message) = validate_checkpoint_id_v2(operation_id) {
            return Err(Box::new(rollback_rejected(
                GuardedRollbackRejectionCodeV2::InvalidOperationId,
                message,
            )));
        }
    }
    Ok(())
}

async fn validate_rollback_workspace(
    state: &DaemonState,
    workspace_arc: &Arc<tokio::sync::RwLock<crate::state::WorkspaceState>>,
    workspace: &crate::state::WorkspaceState,
    registered_path: &str,
    ws_id: &str,
    target_snapshot_id: &str,
) -> Result<(), Box<Response>> {
    if workspace.ws_id != ws_id
        || workspace.path.to_str() != Some(registered_path)
        || !state.exact_registration_is_current(Path::new(registered_path), ws_id, workspace_arc)
        || !state.registration_is_live(ws_id, &workspace.path).await
    {
        return Err(Box::new(rollback_rejected(
            GuardedRollbackRejectionCodeV2::InvalidRegistrationPath,
            "registered workspace path no longer identifies the requested live workspace",
        )));
    }
    match workspace.index.snapshots.get(target_snapshot_id) {
        Some(snapshot) if !snapshot.missing => Ok(()),
        _ => Err(Box::new(rollback_rejected(
            GuardedRollbackRejectionCodeV2::SnapshotNotFound,
            format!("exact snapshot not found: {target_snapshot_id}"),
        ))),
    }
}

fn rollback_diff_digest(changes: &[ws_ckpt_common::DiffEntry]) -> [u8; 32] {
    let mut ordered = changes.to_vec();
    ordered.sort_by(|left, right| diff_entry_key(left).cmp(&diff_entry_key(right)));
    let mut digest = Sha256::new();
    digest.update(b"ws-ckpt-guarded-rollback-diff-v2\0");
    digest.update((ordered.len() as u64).to_le_bytes());
    for change in ordered {
        digest_field(&mut digest, change.path.as_bytes());
        digest.update([change_type_tag(&change.change_type)]);
        match change.detail {
            Some(detail) => {
                digest.update([1]);
                digest_field(&mut digest, detail.as_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.finalize().into()
}

fn diff_entry_key(entry: &ws_ckpt_common::DiffEntry) -> (&str, u8, Option<&str>) {
    (
        entry.path.as_str(),
        change_type_tag(&entry.change_type),
        entry.detail.as_deref(),
    )
}

fn change_type_tag(change_type: &ws_ckpt_common::ChangeType) -> u8 {
    match change_type {
        ws_ckpt_common::ChangeType::Added => 0,
        ws_ckpt_common::ChangeType::Modified => 1,
        ws_ckpt_common::ChangeType::Deleted => 2,
        ws_ckpt_common::ChangeType::Renamed => 3,
    }
}

fn digest_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[allow(clippy::too_many_arguments)]
fn rollback_evidence_matches(
    evidence: &GuardedRollbackEvidenceV2,
    registered_path: &str,
    ws_id: &str,
    expected_generation: WorkspaceGenerationTokenV2,
    target_snapshot_id: &str,
    expected_diff_digest: [u8; 32],
    operation_id: &str,
    operation_digest: [u8; 32],
    caller_uid: u32,
) -> bool {
    evidence.ws_id == ws_id
        && evidence.registered_path == registered_path
        && evidence.expected_generation == expected_generation
        && evidence.target_snapshot_id == target_snapshot_id
        && evidence.expected_diff_digest == expected_diff_digest
        && evidence.operation_id == operation_id
        && evidence.operation_digest == operation_digest
        && evidence.caller_uid == caller_uid
}

fn rollback_evidence_response(evidence: GuardedRollbackEvidenceV2) -> Response {
    match evidence.outcome {
        GuardedRollbackOutcomeV2::Succeeded { .. } => Response::GuardedRollbackV2Ok { evidence },
        GuardedRollbackOutcomeV2::Started | GuardedRollbackOutcomeV2::Unknown { .. } => {
            Response::GuardedRollbackV2Uncertain { evidence }
        }
    }
}

async fn abort_reserved_rollback(
    state: &DaemonState,
    workspace: &mut crate::state::WorkspaceState,
    operation_id: &str,
    rejection: Response,
) -> Response {
    let mut aborted = workspace.index.clone();
    aborted.guarded_rollbacks.remove(operation_id);
    match crate::index_store::save_durable(&state.index_dir(&workspace.ws_id), &aborted).await {
        Ok(()) => {
            workspace.index = aborted;
            rejection
        }
        Err(error) => {
            mark_rollback_unknown(
                state,
                workspace,
                operation_id,
                format!(
                    "rollback was not invoked, but its durable reservation could not be cleared: {error:#}"
                ),
            )
            .await
        }
    }
}

async fn mark_rollback_unknown(
    state: &DaemonState,
    workspace: &mut crate::state::WorkspaceState,
    operation_id: &str,
    reason: String,
) -> Response {
    let Some(evidence) = workspace.index.guarded_rollbacks.get_mut(operation_id) else {
        return rollback_rejected(
            GuardedRollbackRejectionCodeV2::OperationConflict,
            "guarded rollback reservation disappeared during execution",
        );
    };
    evidence.outcome = GuardedRollbackOutcomeV2::Unknown {
        reason: reason.clone(),
    };
    let uncertain_evidence = evidence.clone();
    if let Err(error) =
        crate::index_store::save_durable(&state.index_dir(&workspace.ws_id), &workspace.index).await
    {
        tracing::error!(
            "failed to persist unknown guarded rollback outcome for {}: {error:#}",
            operation_id
        );
    }
    rollback_evidence_response(uncertain_evidence)
}

fn update_live_head(index: &mut ws_ckpt_common::SnapshotIndex, target_snapshot_id: &str) {
    if let Some(old_head) = index.head.clone() {
        if let Some(head) = index.snapshots.get_mut(&old_head) {
            head.child_ids.retain(|child| child != LIVE_CHILD);
        }
    }
    if let Some(target) = index.snapshots.get_mut(target_snapshot_id) {
        if !target.child_ids.iter().any(|child| child == LIVE_CHILD) {
            target.child_ids.push(LIVE_CHILD.to_string());
        }
    }
    index.head = Some(target_snapshot_id.to_string());
}

fn rollback_cwd_rejection(response: Response) -> Response {
    match response {
        Response::Error {
            code: ErrorCode::CwdOccupied,
            message,
        } => rollback_rejected(GuardedRollbackRejectionCodeV2::CwdOccupied, message),
        Response::Error {
            code: ErrorCode::CwdScanFailed,
            message,
        } => rollback_rejected(GuardedRollbackRejectionCodeV2::CwdScanFailed, message),
        Response::Error { message, .. } => {
            rollback_rejected(GuardedRollbackRejectionCodeV2::DaemonNotReady, message)
        }
        _ => rollback_rejected(
            GuardedRollbackRejectionCodeV2::DaemonNotReady,
            "unexpected cwd guard response",
        ),
    }
}

fn rollback_workspace_not_found(workspace: &str) -> Response {
    rollback_rejected(
        GuardedRollbackRejectionCodeV2::WorkspaceNotFound,
        format!("workspace is not registered: {workspace}"),
    )
}

fn rollback_rejected(code: GuardedRollbackRejectionCodeV2, message: impl Into<String>) -> Response {
    Response::GuardedRollbackV2Rejected {
        code,
        message: message.into(),
    }
}

fn validate_registration_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty() || !path.is_absolute() || value.as_bytes().contains(&0) {
        return Err("registration path must be a non-empty absolute path".to_string());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("registration path must not contain '.' or '..' components".to_string());
    }
    Ok(())
}

fn index_with_evidence_slot(
    index: &ws_ckpt_common::SnapshotIndex,
) -> Option<ws_ckpt_common::SnapshotIndex> {
    let mut next = index.clone();
    while next.governed_evidence.len() >= GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2 {
        let evict = next
            .governed_evidence
            .iter()
            .find(|(_, evidence)| {
                matches!(
                    &evidence.outcome,
                    GuardedCheckpointOutcomeV2::Skipped { .. }
                ) || created_evidence_is_marked_missing(&next, evidence)
            })
            .map(|(checkpoint_id, _)| checkpoint_id.clone())?;
        next.governed_evidence.remove(&evict);
    }
    Some(next)
}

fn created_evidence_is_marked_missing(
    index: &ws_ckpt_common::SnapshotIndex,
    evidence: &GuardedCheckpointEvidenceV2,
) -> bool {
    match &evidence.outcome {
        GuardedCheckpointOutcomeV2::Created { snapshot_id }
            if snapshot_id == &evidence.checkpoint_id =>
        {
            index
                .snapshots
                .get(snapshot_id)
                .is_some_and(|snapshot| snapshot.missing)
        }
        _ => false,
    }
}

fn evidence(
    ws_id: &str,
    registered_path: String,
    generation: WorkspaceGenerationTokenV2,
    checkpoint_id: &str,
    operation_digest: [u8; 32],
    caller_uid: u32,
    outcome: GuardedCheckpointOutcomeV2,
) -> GuardedCheckpointEvidenceV2 {
    GuardedCheckpointEvidenceV2 {
        ws_id: ws_id.to_string(),
        registered_path,
        generation,
        checkpoint_id: checkpoint_id.to_string(),
        operation_digest,
        caller_uid,
        outcome,
    }
}

fn evidence_matches(
    evidence: &GuardedCheckpointEvidenceV2,
    ws_id: &str,
    generation: WorkspaceGenerationTokenV2,
    checkpoint_id: &str,
    operation_digest: [u8; 32],
    caller_uid: u32,
) -> bool {
    evidence.ws_id == ws_id
        && evidence.generation == generation
        && evidence.checkpoint_id == checkpoint_id
        && evidence.operation_digest == operation_digest
        && evidence.caller_uid == caller_uid
}

fn evidence_is_visible(
    index: &ws_ckpt_common::SnapshotIndex,
    evidence: &GuardedCheckpointEvidenceV2,
) -> bool {
    match &evidence.outcome {
        GuardedCheckpointOutcomeV2::Created { snapshot_id }
            if snapshot_id == &evidence.checkpoint_id =>
        {
            index
                .snapshots
                .get(&evidence.checkpoint_id)
                .is_some_and(|snapshot| !snapshot.missing)
        }
        GuardedCheckpointOutcomeV2::Created { .. } => false,
        GuardedCheckpointOutcomeV2::Skipped { .. } => true,
    }
}

fn workspace_not_found(workspace: &str) -> Response {
    rejected(
        GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
        format!("workspace is not registered: {workspace}"),
    )
}

fn rejected(code: GuardedCheckpointRejectionCodeV2, message: impl Into<String>) -> Response {
    Response::GuardedCheckpointV2Rejected {
        code,
        message: message.into(),
    }
}

fn backend_effect_error(message: String) -> Response {
    Response::Error {
        code: ErrorCode::InternalError,
        message,
    }
}

#[cfg(test)]
mod tests;
