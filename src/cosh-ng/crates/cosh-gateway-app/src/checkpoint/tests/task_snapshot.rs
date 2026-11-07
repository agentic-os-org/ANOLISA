use super::*;

fn task_snapshot_adapter(
    replies: Vec<DaemonReply>,
) -> (
    tempfile::TempDir,
    TaskSnapshotAdapter,
    TaskSnapshotProviderRequest,
    thread::JoinHandle<Vec<WsCkptRequest>>,
) {
    let (directory, socket_path, daemon) = spawn_daemon(replies);
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let workspaces = TrustedWorkspaceResolver::new(
        GatewayCapabilityProfile::task_only_v1().governed_target(),
        directory.path(),
    )
    .unwrap();
    let workspace = workspaces.workspace_ref().clone();
    let adapter = TaskSnapshotAdapter::admit(
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
    (directory, adapter, request, daemon)
}

fn assert_classified(error: &ContractError) {
    assert!(!error.safe_message.as_str().is_empty());
    assert_ne!(
        error.safe_message.as_str(),
        "The governed checkpoint operation could not be completed safely"
    );
}

#[test]
fn task_snapshot_preview_maps_generation_mismatch_rejection() {
    let (_directory, mut adapter, request, daemon) = task_snapshot_adapter(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::Response(Box::new(WsCkptResponse::GuardedRollbackV2Rejected {
            code: cosh_types::checkpoint::GuardedRollbackRejectionCodeV2::GenerationMismatch,
            message: "generation changed".to_owned(),
        })),
    ]);
    let error = adapter.preview(&request).unwrap_err();
    assert_eq!(
        error.code.as_str(),
        "checkpoint_switch_rejected_generation_mismatch"
    );
    assert!(!error.retryable);
    assert_classified(&error);
    daemon.join().unwrap();
}

#[test]
fn task_snapshot_preview_maps_diff_mismatch_rejection() {
    let (_directory, mut adapter, request, daemon) = task_snapshot_adapter(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::Response(Box::new(WsCkptResponse::GuardedRollbackV2Rejected {
            code: cosh_types::checkpoint::GuardedRollbackRejectionCodeV2::DiffMismatch,
            message: "diff changed".to_owned(),
        })),
    ]);
    let error = adapter.preview(&request).unwrap_err();
    assert_eq!(
        error.code.as_str(),
        "checkpoint_switch_rejected_diff_mismatch"
    );
    assert_ne!(
        error.code.as_str(),
        "checkpoint_switch_rejected_generation_mismatch"
    );
    assert!(!error.retryable);
    assert_classified(&error);
    daemon.join().unwrap();
}

#[test]
fn task_snapshot_preview_reports_daemon_protocol_mismatch() {
    let (_directory, mut adapter, request, daemon) = task_snapshot_adapter(vec![
        DaemonReply::Identity,
        DaemonReply::Identity,
        DaemonReply::CloseWithoutReply,
    ]);
    let error = adapter.preview(&request).unwrap_err();
    assert_eq!(error.code.as_str(), "checkpoint_daemon_protocol_mismatch");
    assert!(!error.retryable);
    assert!(error.safe_message.as_str().contains("ws-ckpt"));
    assert_classified(&error);
    daemon.join().unwrap();
}

#[test]
fn task_snapshot_preview_reports_daemon_unavailable() {
    let (_directory, mut adapter, request, daemon) =
        task_snapshot_adapter(vec![DaemonReply::Identity]);
    // Once the listener is dropped the socket file remains but refuses
    // connections, which the client classifies as daemon-unavailable.
    daemon.join().unwrap();
    let error = adapter.preview(&request).unwrap_err();
    assert_eq!(error.code.as_str(), "checkpoint_daemon_unavailable");
    assert!(error.retryable);
    assert_classified(&error);
}
