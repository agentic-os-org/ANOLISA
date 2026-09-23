use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::thread;

use cosh_gateway::capability::{ExecutionTarget, ExecutionTargetOutcome};
use cosh_gateway_contracts::capability::RuntimeExecutionFence;
use cosh_gateway_contracts::common::{BoundedName, BoundedOpaque, Digest, TargetRef, WorkspaceRef};
use cosh_gateway_contracts::ids::{ApprovalId, CheckpointId, RunId, RuntimeBindingId, TaskId};
use cosh_gateway_contracts::runtime::{
    BrokeredOperationResult, WorkspaceCheckpointCreateV1Outcome,
};
use cosh_types::checkpoint::{
    ChangeType, DiffEntry, GuardedCheckpointEvidenceV2, GuardedCheckpointOutcomeV2,
    GuardedCheckpointRejectionCodeV2, GuardedRollbackEvidenceV2, GuardedRollbackOutcomeV2,
    WorkspaceGenerationTokenV2, WsCkptRequest, WsCkptResponse,
    GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
};

use super::*;

enum DaemonReply {
    Response(Box<WsCkptResponse>),
    Identity,
    CreatedFromRequest,
    SkippedFromRequest(String),
    EvidenceFromRequest,
    CreatedWithWrongPath,
    GuardedPreviewFromRequest(Vec<DiffEntry>),
    GuardedRollbackFromRequest,
    ReplaceWorkspace(PathBuf, bool),
}

#[test]
fn task_snapshot_preview_recovery_and_switch_use_exact_ids() {
    let task_id = TaskId::new();
    let snapshot_id = CheckpointId::new();
    let recovery_id = CheckpointId::new();
    let (directory, socket_path, daemon) = spawn_daemon(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::GuardedPreviewFromRequest(vec![DiffEntry {
            path: "managed/1.txt".to_owned(),
            change_type: ChangeType::Deleted,
            detail: None,
        }]),
        DaemonReply::Identity,
        DaemonReply::CreatedFromRequest,
        DaemonReply::Identity,
        DaemonReply::GuardedPreviewFromRequest(vec![DiffEntry {
            path: "managed/1.txt".to_owned(),
            change_type: ChangeType::Deleted,
            detail: None,
        }]),
        DaemonReply::GuardedRollbackFromRequest,
    ]);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspaces = TrustedWorkspaceResolver::new(
        GatewayCapabilityProfile::task_only_v1().governed_target(),
        directory.path(),
    )
    .unwrap();
    let workspace = workspaces.workspace_ref().clone();
    let mut adapter = TaskSnapshotAdapter::admit(
        PathBuf::from(socket_path),
        Path::new("/workspace"),
        workspaces,
        nix::unistd::Uid::effective().as_raw(),
    )
    .unwrap();
    let request = TaskSnapshotProviderRequest {
        task_id,
        snapshot_id: snapshot_id.clone(),
        workspace,
    };

    let preview = adapter.preview(&request).unwrap();
    assert_eq!(preview.changes.len(), 1);
    assert_eq!(preview.changes[0].path.as_str(), "managed/1.txt");
    adapter
        .create_recovery(&request, &recovery_id, &preview.preview_digest)
        .unwrap();
    let operation_id = recovery_id.clone();
    let operation_digest = digest(9);
    let switched = adapter
        .switch(
            &request,
            &preview.preview_digest,
            &operation_id,
            &operation_digest,
        )
        .unwrap();
    let TaskSnapshotProviderSwitchResult::Switched(switched) = switched else {
        panic!("expected successful guarded switch")
    };
    assert_eq!(switched.from.as_str(), hex_bytes(&[7; 32]));
    assert_eq!(switched.to, snapshot_id);

    let requests = daemon.join().unwrap();
    assert!(matches!(
        &requests[2],
        WsCkptRequest::GuardedRollbackPreviewV2 { target_snapshot_id, .. }
            if target_snapshot_id == snapshot_id.as_str()
    ));
    assert!(matches!(
        &requests[4],
        WsCkptRequest::GuardedCheckpointV2 { checkpoint_id, .. }
            if checkpoint_id == recovery_id.as_str()
    ));
    assert!(matches!(
        &requests[7],
        WsCkptRequest::GuardedRollbackV2 {
            target_snapshot_id,
            operation_id: actual_operation_id,
            operation_digest: actual_operation_digest,
            ..
        } if target_snapshot_id == snapshot_id.as_str()
            && actual_operation_id == operation_id.as_str()
            && actual_operation_digest == &digest_bytes(&operation_digest).unwrap()
    ));
}

fn spawn_daemon(
    replies: Vec<DaemonReply>,
) -> (
    tempfile::TempDir,
    String,
    thread::JoinHandle<Vec<WsCkptRequest>>,
) {
    let directory = tempfile::tempdir().unwrap();
    let socket_path = directory.path().join("ws-ckpt.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let handle = thread::spawn(move || {
        let mut requests = Vec::with_capacity(replies.len());
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).unwrap();
            let mut payload = vec![0_u8; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut payload).unwrap();
            let request: WsCkptRequest = bincode::deserialize(&payload).unwrap();
            let response = daemon_response(reply, &request);
            requests.push(request);
            let payload = bincode::serialize(&response).unwrap();
            stream
                .write_all(&(payload.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(&payload).unwrap();
        }
        requests
    });
    (
        directory,
        socket_path.to_string_lossy().into_owned(),
        handle,
    )
}

fn daemon_response(reply: DaemonReply, request: &WsCkptRequest) -> WsCkptResponse {
    match reply {
        DaemonReply::ReplaceWorkspace(workspace, recreate) => {
            let old = workspace.with_extension("rollback-tmp");
            std::fs::rename(&workspace, &old).unwrap();
            if recreate {
                std::fs::create_dir(&workspace).unwrap();
            }
            std::fs::remove_dir_all(old).unwrap();
            daemon_response(DaemonReply::GuardedRollbackFromRequest, request)
        }
        DaemonReply::Response(response) => *response,
        DaemonReply::Identity => {
            let WsCkptRequest::WorkspaceIdentityV2 { registration_path } = request else {
                panic!("expected workspace identity request")
            };
            WsCkptResponse::WorkspaceIdentityV2Ok {
                protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
                ws_id: "ws-abc123".to_owned(),
                registered_path: registration_path.clone(),
                generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
            }
        }
        DaemonReply::CreatedFromRequest | DaemonReply::SkippedFromRequest(_) => {
            let WsCkptRequest::GuardedCheckpointV2 {
                ws_id,
                expected_generation,
                checkpoint_id,
                operation_digest,
                ..
            } = request
            else {
                panic!("expected guarded checkpoint request")
            };
            let mut evidence = guarded_evidence(
                ws_id,
                *expected_generation,
                checkpoint_id,
                *operation_digest,
            );
            if let DaemonReply::SkippedFromRequest(reason) = reply {
                evidence.outcome = GuardedCheckpointOutcomeV2::Skipped { reason };
            }
            WsCkptResponse::GuardedCheckpointV2Ok { evidence }
        }
        DaemonReply::EvidenceFromRequest => {
            let WsCkptRequest::CheckpointEvidenceV2 {
                ws_id,
                expected_generation,
                checkpoint_id,
                operation_digest,
            } = request
            else {
                panic!("expected checkpoint evidence request")
            };
            WsCkptResponse::CheckpointEvidenceV2Ok {
                evidence: Some(guarded_evidence(
                    ws_id,
                    *expected_generation,
                    checkpoint_id,
                    *operation_digest,
                )),
            }
        }
        DaemonReply::CreatedWithWrongPath => {
            let WsCkptRequest::GuardedCheckpointV2 {
                ws_id,
                expected_generation,
                checkpoint_id,
                operation_digest,
                ..
            } = request
            else {
                panic!("expected guarded checkpoint request")
            };
            let mut evidence = guarded_evidence(
                ws_id,
                *expected_generation,
                checkpoint_id,
                *operation_digest,
            );
            evidence.registered_path = "/wrong-workspace".to_owned();
            WsCkptResponse::GuardedCheckpointV2Ok { evidence }
        }
        DaemonReply::GuardedPreviewFromRequest(changes) => {
            let WsCkptRequest::GuardedRollbackPreviewV2 {
                registered_path,
                ws_id,
                expected_generation,
                target_snapshot_id,
            } = request
            else {
                panic!("expected guarded rollback preview request")
            };
            WsCkptResponse::GuardedRollbackPreviewV2Ok {
                protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
                registered_path: registered_path.clone(),
                ws_id: ws_id.clone(),
                generation: *expected_generation,
                target_snapshot_id: target_snapshot_id.clone(),
                diff_digest: [6; 32],
                changes,
                caller_uid: nix::unistd::Uid::effective().as_raw(),
            }
        }
        DaemonReply::GuardedRollbackFromRequest => {
            let WsCkptRequest::GuardedRollbackV2 {
                registered_path,
                ws_id,
                expected_generation,
                target_snapshot_id,
                expected_diff_digest,
                operation_id,
                operation_digest,
            } = request
            else {
                panic!("expected guarded rollback request")
            };
            WsCkptResponse::GuardedRollbackV2Ok {
                evidence: GuardedRollbackEvidenceV2 {
                    ws_id: ws_id.clone(),
                    registered_path: registered_path.clone(),
                    expected_generation: *expected_generation,
                    target_snapshot_id: target_snapshot_id.clone(),
                    expected_diff_digest: *expected_diff_digest,
                    operation_id: operation_id.clone(),
                    operation_digest: *operation_digest,
                    caller_uid: nix::unistd::Uid::effective().as_raw(),
                    outcome: GuardedRollbackOutcomeV2::Succeeded {
                        resulting_generation: WorkspaceGenerationTokenV2::from_bytes([8; 32]),
                    },
                },
            }
        }
    }
}

#[test]
fn task_switch_recovery_rejects_wrong_registered_path_evidence() {
    let (directory, socket_path, daemon) = spawn_daemon(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::CreatedWithWrongPath,
    ]);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspaces = TrustedWorkspaceResolver::new(
        GatewayCapabilityProfile::task_only_v1().governed_target(),
        directory.path(),
    )
    .unwrap();
    let workspace = workspaces.workspace_ref().clone();
    let mut adapter = TaskSnapshotAdapter::admit(
        PathBuf::from(socket_path),
        Path::new("/workspace"),
        workspaces,
        nix::unistd::Uid::effective().as_raw(),
    )
    .unwrap();
    let request = TaskSnapshotProviderRequest {
        task_id: TaskId::new(),
        snapshot_id: CheckpointId::new(),
        workspace,
    };

    let error = adapter
        .create_recovery(&request, &CheckpointId::new(), &digest(3))
        .unwrap_err();

    assert_eq!(error.code.as_str(), "checkpoint_recovery_not_created");
    assert_eq!(daemon.join().unwrap().len(), 3);
}

fn guarded_evidence(
    ws_id: &str,
    generation: WorkspaceGenerationTokenV2,
    checkpoint_id: &str,
    operation_digest: [u8; 32],
) -> GuardedCheckpointEvidenceV2 {
    GuardedCheckpointEvidenceV2 {
        ws_id: ws_id.to_owned(),
        registered_path: "/workspace".to_owned(),
        generation,
        checkpoint_id: checkpoint_id.to_owned(),
        operation_digest,
        caller_uid: nix::unistd::Uid::effective().as_raw(),
        outcome: GuardedCheckpointOutcomeV2::Created {
            snapshot_id: checkpoint_id.to_owned(),
        },
    }
}

fn digest(byte: u8) -> Digest {
    Digest::parse(format!("{byte:02x}").repeat(32)).unwrap()
}

fn operation() -> CheckpointOperation {
    CheckpointOperation {
        target: TargetRef {
            kind: BoundedName::new("local").unwrap(),
            authority: BoundedName::new("cosh").unwrap(),
            identifier: BoundedOpaque::new("primary").unwrap(),
        },
        target_identity_digest: digest(8),
        runtime_fence: RuntimeExecutionFence {
            binding_id: RuntimeBindingId::new(),
            runtime_generation: 11,
            lease_generation: 12,
            lease_revision: 13,
        },
        operation_digest: digest(9),
        input_digest: digest(10),
        checkpoint_id: CheckpointId::new(),
        binding: CheckpointBinding {
            version: BINDING_VERSION,
            ws_id: "ws-abc123".to_owned(),
            registered_path: "/workspace".to_owned(),
            generation: [7; 32],
            owner_uid: nix::unistd::Uid::effective().as_raw(),
        },
    }
}

fn evidence(operation: &CheckpointOperation) -> GuardedCheckpointEvidenceV2 {
    GuardedCheckpointEvidenceV2 {
        ws_id: operation.binding.ws_id.clone(),
        registered_path: operation.binding.registered_path.clone(),
        generation: WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation),
        checkpoint_id: operation.checkpoint_id.as_str().to_owned(),
        operation_digest: digest_bytes(&operation.operation_digest).unwrap(),
        caller_uid: operation.binding.owner_uid,
        outcome: GuardedCheckpointOutcomeV2::Created {
            snapshot_id: operation.checkpoint_id.as_str().to_owned(),
        },
    }
}

fn execute(
    operation: &CheckpointOperation,
    replies: Vec<DaemonReply>,
) -> (ExecutionTargetOutcome, Vec<WsCkptRequest>) {
    let (_directory, socket_path, daemon) = spawn_daemon(replies);
    let client = CkptClient::with_timeout(&socket_path, 1_000)
        .require_trusted_peer(operation.binding.owner_uid);
    let mut target = CheckpointTarget { client: &client };
    let outcome = target.execute(operation);
    (outcome, daemon.join().unwrap())
}

fn assert_guarded_request(request: &WsCkptRequest, operation: &CheckpointOperation) {
    let WsCkptRequest::GuardedCheckpointV2 {
        ws_id,
        expected_generation,
        checkpoint_id,
        operation_digest,
        message,
        metadata,
        pin,
    } = request
    else {
        panic!("expected one guarded checkpoint request")
    };
    assert_eq!(ws_id, &operation.binding.ws_id);
    assert_eq!(
        expected_generation,
        &WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation)
    );
    assert_eq!(checkpoint_id, operation.checkpoint_id.as_str());
    assert_eq!(
        operation_digest,
        &digest_bytes(&operation.operation_digest).unwrap()
    );
    assert_eq!(message.as_deref(), Some("COSH governed Task checkpoint"));
    assert_eq!(metadata, &None);
    assert!(!pin);
}

fn assert_evidence_request(request: &WsCkptRequest, operation: &CheckpointOperation) {
    let WsCkptRequest::CheckpointEvidenceV2 {
        ws_id,
        expected_generation,
        checkpoint_id,
        operation_digest,
    } = request
    else {
        panic!("expected one read-only evidence request")
    };
    assert_eq!(ws_id, &operation.binding.ws_id);
    assert_eq!(
        expected_generation,
        &WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation)
    );
    assert_eq!(checkpoint_id, operation.checkpoint_id.as_str());
    assert_eq!(
        operation_digest,
        &digest_bytes(&operation.operation_digest).unwrap()
    );
}

#[test]
fn created_checkpoint_commits_the_exact_typed_receipt() {
    let operation = operation();
    let evidence = evidence(&operation);
    let expected_receipt = digest_parts(&[
        RECEIPT_DOMAIN,
        operation.target_identity_digest.as_str().as_bytes(),
        operation.operation_digest.as_str().as_bytes(),
        operation.checkpoint_id.as_str().as_bytes(),
        &evidence.generation.into_bytes(),
        &serde_json::to_vec(&evidence).unwrap(),
    ])
    .unwrap();
    let (outcome, requests) = execute(
        &operation,
        vec![DaemonReply::Response(Box::new(
            WsCkptResponse::GuardedCheckpointV2Ok {
                evidence: evidence.clone(),
            },
        ))],
    );

    let ExecutionTargetOutcome::Conclusive {
        succeeded,
        receipt_digest,
        typed_result,
        ..
    } = outcome
    else {
        panic!("created checkpoint must be conclusive")
    };
    assert!(succeeded);
    assert_eq!(receipt_digest, expected_receipt);
    assert!(matches!(
        typed_result,
        Some(BrokeredOperationResult::WorkspaceCheckpointCreateV1(result))
            if result.checkpoint_id == operation.checkpoint_id
                && matches!(
                    result.outcome,
                    WorkspaceCheckpointCreateV1Outcome::Created { ref snapshot_id }
                        if snapshot_id.as_str() == operation.checkpoint_id.as_str()
                )
    ));
    assert_eq!(requests.len(), 1);
    assert_guarded_request(&requests[0], &operation);
}

#[test]
fn possibly_applied_reconciles_with_evidence_without_replay() {
    let operation = operation();
    let evidence = evidence(&operation);
    let mut mismatched_evidence = evidence.clone();
    mismatched_evidence.operation_digest = [6; 32];
    let (outcome, requests) = execute(
        &operation,
        vec![
            DaemonReply::Response(Box::new(WsCkptResponse::GuardedCheckpointV2Ok {
                evidence: mismatched_evidence,
            })),
            DaemonReply::Response(Box::new(WsCkptResponse::CheckpointEvidenceV2Ok {
                evidence: Some(evidence),
            })),
        ],
    );

    assert!(matches!(
        outcome,
        ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            typed_result: Some(BrokeredOperationResult::WorkspaceCheckpointCreateV1(_)),
            ..
        }
    ));
    assert_eq!(requests.len(), 2);
    assert_guarded_request(&requests[0], &operation);
    assert_evidence_request(&requests[1], &operation);
}

#[test]
fn missing_reconcile_evidence_is_unknown_and_never_replayed() {
    let operation = operation();
    let mut mismatched_evidence = evidence(&operation);
    mismatched_evidence.operation_digest = [6; 32];
    let (outcome, requests) = execute(
        &operation,
        vec![
            DaemonReply::Response(Box::new(WsCkptResponse::GuardedCheckpointV2Ok {
                evidence: mismatched_evidence,
            })),
            DaemonReply::Response(Box::new(WsCkptResponse::CheckpointEvidenceV2Ok {
                evidence: None,
            })),
        ],
    );

    assert!(matches!(outcome, ExecutionTargetOutcome::Unknown { .. }));
    assert_eq!(requests.len(), 2);
    assert_guarded_request(&requests[0], &operation);
    assert_evidence_request(&requests[1], &operation);
}

#[test]
fn explicit_v2_rejection_is_a_conclusive_failure() {
    let operation = operation();
    let (outcome, requests) = execute(
        &operation,
        vec![DaemonReply::Response(Box::new(
            WsCkptResponse::GuardedCheckpointV2Rejected {
                code: GuardedCheckpointRejectionCodeV2::GenerationMismatch,
                message: "daemon-private generation detail".to_owned(),
            },
        ))],
    );

    let ExecutionTargetOutcome::Conclusive {
        succeeded,
        safe_detail,
        typed_result,
        ..
    } = outcome
    else {
        panic!("an explicit V2 pre-effect rejection must be conclusive")
    };
    assert!(!succeeded);
    assert!(typed_result.is_none());
    let safe_detail = safe_detail.unwrap();
    assert!(!safe_detail.as_str().contains("daemon-private"));
    assert_eq!(requests.len(), 1);
    assert_guarded_request(&requests[0], &operation);
}

fn pre_runtime_workspace() -> WorkspaceRef {
    WorkspaceRef {
        scope_digest: digest(3),
        display_name: None,
    }
}

fn pre_runtime_request(workspace: WorkspaceRef) -> PreRuntimeCheckpointRequest {
    PreRuntimeCheckpointRequest {
        baseline_id: CheckpointId::new(),
        task_id: TaskId::new(),
        run_id: RunId::new(),
        workspace,
    }
}

fn approval_checkpoint_request(workspace: WorkspaceRef) -> ApprovalCheckpointRequest {
    ApprovalCheckpointRequest {
        checkpoint_id: CheckpointId::new(),
        approval_id: ApprovalId::new(),
        task_id: TaskId::new(),
        run_id: RunId::new(),
        workspace,
        runtime_fence: RuntimeExecutionFence {
            binding_id: RuntimeBindingId::new(),
            runtime_generation: 21,
            lease_generation: 22,
            lease_revision: 23,
        },
    }
}

#[test]
fn pre_runtime_create_dispatches_guarded_checkpoint_once() {
    let (directory, socket_path, daemon) = spawn_daemon(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::CreatedFromRequest,
    ]);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = pre_runtime_workspace();
    let mut adapter = PreRuntimeCheckpointAdapter::admit(
        PathBuf::from(socket_path),
        Path::new("/workspace"),
        workspace.clone(),
        nix::unistd::Uid::effective().as_raw(),
    )
    .unwrap();
    let request = pre_runtime_request(workspace);
    let binding = adapter.prepare_baseline(&request).unwrap();

    let result = adapter.create_baseline(&request, &binding).unwrap();

    assert!(matches!(
        result,
        PreRuntimeCheckpointCreateResult::Created { ref evidence }
            if evidence.baseline_id == request.baseline_id
                && evidence.provider_generation.as_str() == "07".repeat(32)
    ));
    let requests = daemon.join().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::WorkspaceIdentityV2 { .. }))
            .count(),
        2,
        "create must consume the prepared workspace generation"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::GuardedCheckpointV2 { .. }))
            .count(),
        1
    );
    drop(directory);
}

#[test]
fn pre_runtime_skipped_checkpoint_explains_how_to_resubmit_safely() {
    for (provider_reason, empty_workspace) in [
        ("Empty workspace, no snapshot created.", true),
        ("daemon-private reason for skipping", false),
    ] {
        let (directory, socket_path, daemon) = spawn_daemon(vec![
            DaemonReply::Identity,
            DaemonReply::Identity,
            DaemonReply::SkippedFromRequest(provider_reason.to_owned()),
        ]);
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let workspace = pre_runtime_workspace();
        let mut adapter = PreRuntimeCheckpointAdapter::admit(
            PathBuf::from(socket_path),
            Path::new("/workspace"),
            workspace.clone(),
            nix::unistd::Uid::effective().as_raw(),
        )
        .unwrap();
        let request = pre_runtime_request(workspace);
        let binding = adapter.prepare_baseline(&request).unwrap();

        let PreRuntimeCheckpointCreateResult::KnownNoEffect { reason } =
            adapter.create_baseline(&request, &binding).unwrap()
        else {
            panic!("a skipped checkpoint must not authorize a required baseline")
        };
        let message = reason.as_str();
        assert_eq!(message.contains("empty workspace"), empty_workspace);
        assert_eq!(
            message.contains("Upgrade ws-ckpt or add a file"),
            empty_workspace
        );
        assert!(message.contains("submit a new Task"));
        assert!(message.contains("checkpoint=off"));
        assert!(!message.contains("daemon-private"));
        let requests = daemon.join().unwrap();
        assert_eq!(
            requests.len(),
            3,
            "skip must not trigger an automatic retry"
        );
    }
}

#[test]
fn pre_runtime_possibly_applied_reconciles_only_with_evidence() {
    let mismatched = GuardedCheckpointEvidenceV2 {
        ws_id: "ws-abc123".to_owned(),
        registered_path: "/workspace".to_owned(),
        generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
        checkpoint_id: "different-baseline".to_owned(),
        operation_digest: [6; 32],
        caller_uid: nix::unistd::Uid::effective().as_raw(),
        outcome: GuardedCheckpointOutcomeV2::Created {
            snapshot_id: "different-baseline".to_owned(),
        },
    };
    let (directory, socket_path, daemon) = spawn_daemon(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::Response(Box::new(WsCkptResponse::GuardedCheckpointV2Ok {
            evidence: mismatched,
        })),
        DaemonReply::EvidenceFromRequest,
    ]);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = pre_runtime_workspace();
    let mut adapter = PreRuntimeCheckpointAdapter::admit(
        PathBuf::from(socket_path),
        Path::new("/workspace"),
        workspace.clone(),
        nix::unistd::Uid::effective().as_raw(),
    )
    .unwrap();
    let request = pre_runtime_request(workspace);
    let binding = adapter.prepare_baseline(&request).unwrap();

    assert!(matches!(
        adapter.create_baseline(&request, &binding).unwrap(),
        PreRuntimeCheckpointCreateResult::PossiblyApplied { .. }
    ));
    assert!(matches!(
        adapter.reconcile_baseline(&request, &binding).unwrap(),
        PreRuntimeCheckpointReconcileResult::Created { .. }
    ));

    let requests = daemon.join().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::WorkspaceIdentityV2 { .. }))
            .count(),
        2,
        "reconciliation must not resolve a new workspace generation"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::GuardedCheckpointV2 { .. }))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::CheckpointEvidenceV2 { .. }))
            .count(),
        1
    );
    drop(directory);
}

#[test]
fn pre_runtime_missing_exact_evidence_remains_unknown() {
    let (directory, socket_path, daemon) = spawn_daemon(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::Response(Box::new(WsCkptResponse::CheckpointEvidenceV2Ok {
            evidence: None,
        })),
    ]);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = pre_runtime_workspace();
    let mut adapter = PreRuntimeCheckpointAdapter::admit(
        PathBuf::from(socket_path),
        Path::new("/workspace"),
        workspace.clone(),
        nix::unistd::Uid::effective().as_raw(),
    )
    .unwrap();
    let request = pre_runtime_request(workspace);
    let binding = adapter.prepare_baseline(&request).unwrap();

    assert!(matches!(
        adapter.reconcile_baseline(&request, &binding).unwrap(),
        PreRuntimeCheckpointReconcileResult::Unknown { .. }
    ));

    let requests = daemon.join().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::CheckpointEvidenceV2 { .. }))
            .count(),
        1
    );
    drop(directory);
}

#[test]
fn approval_checkpoint_guarded_create_binds_ids_fence_and_safe_metadata() {
    let (directory, socket_path, daemon) = spawn_daemon(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::CreatedFromRequest,
    ]);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = pre_runtime_workspace();
    let mut adapter = PreRuntimeCheckpointAdapter::admit(
        PathBuf::from(socket_path),
        Path::new("/workspace"),
        workspace.clone(),
        nix::unistd::Uid::effective().as_raw(),
    )
    .unwrap();
    let request = approval_checkpoint_request(workspace);
    let ApprovalCheckpointPrepareResult::Prepared(binding) =
        adapter.prepare_approval_checkpoint(&request).unwrap()
    else {
        panic!("approval checkpoint must prepare a durable binding")
    };

    assert!(matches!(
        adapter
            .create_approval_checkpoint(&request, &binding)
            .unwrap(),
        ApprovalCheckpointCreateResult::Created { ref evidence }
            if evidence.checkpoint_id == request.checkpoint_id
                && evidence.provider_generation == binding.provider_generation
    ));

    let requests = daemon.join().unwrap();
    let guarded = requests
        .iter()
        .find(|request| matches!(request, WsCkptRequest::GuardedCheckpointV2 { .. }))
        .expect("approval create must issue one guarded checkpoint request");
    let WsCkptRequest::GuardedCheckpointV2 {
        ws_id,
        expected_generation,
        checkpoint_id,
        operation_digest,
        message,
        metadata,
        pin,
    } = guarded
    else {
        unreachable!()
    };
    assert_eq!(ws_id, binding.provider_workspace_id.as_str());
    assert_eq!(
        expected_generation,
        &WorkspaceGenerationTokenV2::from_bytes([7; 32])
    );
    assert_eq!(checkpoint_id, request.checkpoint_id.as_str());
    let fence = serde_json::to_vec(&request.runtime_fence).unwrap();
    let expected_digest = digest_parts(&[
        APPROVAL_OPERATION_DOMAIN,
        request.checkpoint_id.as_str().as_bytes(),
        request.approval_id.as_str().as_bytes(),
        request.task_id.as_str().as_bytes(),
        request.run_id.as_str().as_bytes(),
        request.workspace.scope_digest.as_str().as_bytes(),
        &fence,
        ws_id.as_bytes(),
        b"/workspace",
        &[7; 32],
        &nix::unistd::Uid::effective().as_raw().to_le_bytes(),
    ])
    .unwrap();
    assert_eq!(operation_digest, &digest_bytes(&expected_digest).unwrap());
    assert_eq!(
        message.as_deref(),
        Some("COSH Task pre-approval checkpoint")
    );
    let metadata: serde_json::Value = serde_json::from_str(
        metadata
            .as_deref()
            .expect("metadata must identify the approval"),
    )
    .unwrap();
    assert_eq!(
        metadata,
        serde_json::json!({
            "task_id": request.task_id.as_str(),
            "run_id": request.run_id.as_str(),
            "approval_id": request.approval_id.as_str(),
        })
    );
    assert_eq!(metadata.as_object().unwrap().len(), 3);
    assert!(!pin);

    let mut changed_fence = request.clone();
    changed_fence.runtime_fence.lease_revision += 1;
    assert!(adapter
        .create_approval_checkpoint(&changed_fence, &binding)
        .is_err());
    drop(directory);
}

#[test]
fn approval_checkpoint_possibly_applied_reconciles_evidence_without_second_create() {
    let mismatched = GuardedCheckpointEvidenceV2 {
        ws_id: "ws-abc123".to_owned(),
        registered_path: "/workspace".to_owned(),
        generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
        checkpoint_id: "different-approval-checkpoint".to_owned(),
        operation_digest: [6; 32],
        caller_uid: nix::unistd::Uid::effective().as_raw(),
        outcome: GuardedCheckpointOutcomeV2::Created {
            snapshot_id: "different-approval-checkpoint".to_owned(),
        },
    };
    let (directory, socket_path, daemon) = spawn_daemon(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::Response(Box::new(WsCkptResponse::GuardedCheckpointV2Ok {
            evidence: mismatched,
        })),
        DaemonReply::EvidenceFromRequest,
    ]);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspace = pre_runtime_workspace();
    let mut adapter = PreRuntimeCheckpointAdapter::admit(
        PathBuf::from(socket_path),
        Path::new("/workspace"),
        workspace.clone(),
        nix::unistd::Uid::effective().as_raw(),
    )
    .unwrap();
    let request = approval_checkpoint_request(workspace);
    let ApprovalCheckpointPrepareResult::Prepared(binding) =
        adapter.prepare_approval_checkpoint(&request).unwrap()
    else {
        panic!("approval checkpoint must prepare a durable binding")
    };

    assert!(matches!(
        adapter
            .create_approval_checkpoint(&request, &binding)
            .unwrap(),
        ApprovalCheckpointCreateResult::PossiblyApplied { .. }
    ));
    assert!(matches!(
        adapter
            .reconcile_approval_checkpoint(&request, &binding)
            .unwrap(),
        ApprovalCheckpointReconcileResult::Created { ref evidence }
            if evidence.checkpoint_id == request.checkpoint_id
                && evidence.provider_generation == binding.provider_generation
    ));

    let requests = daemon.join().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::GuardedCheckpointV2 { .. }))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| matches!(request, WsCkptRequest::CheckpointEvidenceV2 { .. }))
            .count(),
        1
    );
    let guarded = requests
        .iter()
        .find(|request| matches!(request, WsCkptRequest::GuardedCheckpointV2 { .. }))
        .unwrap();
    let evidence = requests
        .iter()
        .find(|request| matches!(request, WsCkptRequest::CheckpointEvidenceV2 { .. }))
        .unwrap();
    let (
        WsCkptRequest::GuardedCheckpointV2 {
            ws_id: guarded_ws,
            expected_generation: guarded_generation,
            checkpoint_id: guarded_checkpoint,
            operation_digest: guarded_digest,
            ..
        },
        WsCkptRequest::CheckpointEvidenceV2 {
            ws_id: evidence_ws,
            expected_generation: evidence_generation,
            checkpoint_id: evidence_checkpoint,
            operation_digest: evidence_digest,
        },
    ) = (guarded, evidence)
    else {
        unreachable!()
    };
    assert_eq!(evidence_ws, guarded_ws);
    assert_eq!(evidence_generation, guarded_generation);
    assert_eq!(evidence_checkpoint, guarded_checkpoint);
    assert_eq!(evidence_digest, guarded_digest);
    drop(directory);
}

#[test]
fn task_snapshot_switch_refreshes_shared_binding_or_blocks_new_runtimes() {
    for recreate in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let target = GatewayCapabilityProfile::task_only_v1().governed_target();
        let workspaces = TrustedWorkspaceResolver::new(target.clone(), &workspace).unwrap();
        let original = workspaces.resolve(&target).unwrap();
        let (directory, socket_path, daemon) = spawn_daemon(vec![
            DaemonReply::Identity,
            DaemonReply::Identity,
            DaemonReply::GuardedPreviewFromRequest(vec![]),
            DaemonReply::Identity,
            DaemonReply::GuardedPreviewFromRequest(vec![]),
            DaemonReply::ReplaceWorkspace(workspace.clone(), recreate),
        ]);
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut adapter = TaskSnapshotAdapter::admit(
            PathBuf::from(socket_path),
            &workspace,
            workspaces.clone(),
            nix::unistd::Uid::effective().as_raw(),
        )
        .unwrap();
        let request = TaskSnapshotProviderRequest {
            task_id: TaskId::new(),
            snapshot_id: CheckpointId::new(),
            workspace: workspaces.workspace_ref().clone(),
        };
        let preview = adapter.preview(&request).unwrap();
        let result = adapter
            .switch(
                &request,
                &preview.preview_digest,
                &CheckpointId::new(),
                &digest(9),
            )
            .unwrap();
        if recreate {
            assert!(matches!(
                result,
                TaskSnapshotProviderSwitchResult::Switched(_)
            ));
            let refreshed = workspaces.resolve(&target).unwrap();
            assert_ne!(refreshed.identity(), original.identity());
            assert_eq!(
                refreshed.identity(),
                cosh_gateway::runtime::PinnedDirectory::pin(&workspace)
                    .unwrap()
                    .identity()
            );
            assert_eq!(refreshed.reference(), original.reference());
        } else {
            assert!(
                matches!(result, TaskSnapshotProviderSwitchResult::PossiblyApplied { error }
                if error.code.as_str() == "checkpoint_workspace_refresh_failed")
            );
            assert!(workspaces.resolve(&target).is_err());
        }
        assert_eq!(daemon.join().unwrap().len(), 6);
    }
}

#[test]
fn task_snapshot_rejection_preserves_binding_but_uncertain_switch_invalidates_it() {
    for rejected in [true, false] {
        let response = if rejected {
            WsCkptResponse::GuardedRollbackV2Rejected {
                code: cosh_types::checkpoint::GuardedRollbackRejectionCodeV2::GenerationMismatch,
                message: "generation changed".to_owned(),
            }
        } else {
            // A reply without matching rollback evidence cannot prove the outcome.
            WsCkptResponse::InitOk {
                ws_id: "unexpected".to_owned(),
            }
        };
        let (directory, socket_path, daemon) = spawn_daemon(vec![
            DaemonReply::Identity,
            DaemonReply::Identity,
            DaemonReply::GuardedPreviewFromRequest(vec![]),
            DaemonReply::Identity,
            DaemonReply::GuardedPreviewFromRequest(vec![]),
            DaemonReply::Response(Box::new(response)),
        ]);
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = GatewayCapabilityProfile::task_only_v1().governed_target();
        let workspaces = TrustedWorkspaceResolver::new(target.clone(), directory.path()).unwrap();
        let original = workspaces.resolve(&target).unwrap();
        let mut adapter = TaskSnapshotAdapter::admit(
            PathBuf::from(socket_path),
            Path::new("/workspace"),
            workspaces.clone(),
            nix::unistd::Uid::effective().as_raw(),
        )
        .unwrap();
        let request = TaskSnapshotProviderRequest {
            task_id: TaskId::new(),
            snapshot_id: CheckpointId::new(),
            workspace: workspaces.workspace_ref().clone(),
        };
        let preview = adapter.preview(&request).unwrap();
        let result = adapter
            .switch(
                &request,
                &preview.preview_digest,
                &CheckpointId::new(),
                &digest(9),
            )
            .unwrap();
        if rejected {
            assert!(matches!(
                result,
                TaskSnapshotProviderSwitchResult::Rejected { .. }
            ));
            assert_eq!(
                workspaces.resolve(&target).unwrap().identity(),
                original.identity()
            );
        } else {
            assert!(matches!(
                result,
                TaskSnapshotProviderSwitchResult::PossiblyApplied { .. }
            ));
            assert!(workspaces.resolve(&target).is_err());
        }
        assert_eq!(daemon.join().unwrap().len(), 6);
    }
}
