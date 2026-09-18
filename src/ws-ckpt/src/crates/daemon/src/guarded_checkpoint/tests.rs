use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use ws_ckpt_common::backend::{BackendType, EnvironmentStatus, GcResult, StorageBackend};
use ws_ckpt_common::{
    decode_payload, encode_frame, ChangeType, DaemonConfig, DiffEntry, GuardedCheckpointOutcomeV2,
    GuardedCheckpointRejectionCodeV2, GuardedRollbackOutcomeV2, GuardedRollbackRejectionCodeV2,
    Request, Response, SnapshotIndex, SnapshotMeta, WorkspaceGenerationTokenV2, WorkspaceInfo,
    GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2,
};

use super::{
    checkpoint, checkpoint_evidence, index_with_evidence_slot, rollback, rollback_diff_digest,
    rollback_evidence, rollback_preview, workspace_identity,
};
use crate::dispatcher;
use crate::state::DaemonState;

const WS_ID: &str = "ws-abcdef";
const CHECKPOINT_ID: &str = "checkpoint-1";
const TARGET_ID: &str = "target-snapshot-full-id";
const OPERATION_ID: &str = "switch-operation-1";
const DIGEST: [u8; 32] = [9; 32];
const OPERATION_DIGEST: [u8; 32] = [11; 32];

struct TestBackend {
    data_root: PathBuf,
    snapshots_root: PathBuf,
    generation: WorkspaceGenerationTokenV2,
    bootstrap_calls: AtomicUsize,
    generation_calls: AtomicUsize,
    create_calls: AtomicUsize,
    diff_calls: AtomicUsize,
    rollback_calls: AtomicUsize,
    rollback_error: Mutex<Option<String>>,
    current_diff: Mutex<Vec<DiffEntry>>,
    diff_after_first: Mutex<Option<Vec<DiffEntry>>>,
}

impl TestBackend {
    fn new(root: &Path) -> Self {
        Self {
            data_root: root.join("data"),
            snapshots_root: root.join("snapshots"),
            generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
            bootstrap_calls: AtomicUsize::new(0),
            generation_calls: AtomicUsize::new(0),
            create_calls: AtomicUsize::new(0),
            diff_calls: AtomicUsize::new(0),
            rollback_calls: AtomicUsize::new(0),
            rollback_error: Mutex::new(None),
            current_diff: Mutex::new(vec![DiffEntry {
                path: "1.txt".to_string(),
                change_type: ChangeType::Deleted,
                detail: None,
            }]),
            diff_after_first: Mutex::new(None),
        }
    }
}

#[async_trait]
impl StorageBackend for TestBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::BtrfsBase
    }

    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn snapshots_root(&self) -> &Path {
        &self.snapshots_root
    }

    async fn init_workspace(&self, _: &str, _: &str) -> anyhow::Result<WorkspaceInfo> {
        anyhow::bail!("init_workspace must not be called by guarded requests")
    }

    async fn create_snapshot(&self, _: &str, _: &str) -> anyhow::Result<()> {
        self.create_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn rollback(&self, _: &str, _: &str) -> anyhow::Result<PathBuf> {
        self.rollback_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = self.rollback_error.lock().unwrap().clone() {
            anyhow::bail!(message);
        }
        Ok(self.data_root.clone())
    }

    async fn delete_snapshot(&self, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("unused test backend operation")
    }

    async fn recover_workspace(&self, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("unused test backend operation")
    }

    async fn diff(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<Vec<ws_ckpt_common::DiffEntry>> {
        let call = self.diff_calls.fetch_add(1, Ordering::SeqCst);
        if call > 0 {
            if let Some(changes) = self.diff_after_first.lock().unwrap().clone() {
                return Ok(changes);
            }
        }
        Ok(self.current_diff.lock().unwrap().clone())
    }

    async fn cleanup_snapshots(
        &self,
        _: &str,
        _: &[String],
    ) -> anyhow::Result<Vec<(String, ws_ckpt_common::backend::SnapshotDeleteOutcome)>> {
        anyhow::bail!("unused test backend operation")
    }

    async fn fork(&self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("unused test backend operation")
    }

    async fn gc_generations(&self, _: &str) -> anyhow::Result<GcResult> {
        anyhow::bail!("unused test backend operation")
    }

    async fn check_environment(&self) -> anyhow::Result<EnvironmentStatus> {
        anyhow::bail!("unused test backend operation")
    }

    async fn get_usage(&self) -> anyhow::Result<(u64, u64)> {
        anyhow::bail!("unused test backend operation")
    }

    async fn bootstrap(&self, _: &DaemonConfig) -> anyhow::Result<()> {
        self.bootstrap_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn live_generation(&self, _: &str) -> anyhow::Result<WorkspaceGenerationTokenV2> {
        self.generation_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.generation)
    }
}

struct Fixture {
    _temp: TempDir,
    state: Arc<DaemonState>,
    backend: Arc<TestBackend>,
    workspace_path: PathBuf,
}

impl Fixture {
    fn new(nonempty: bool) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_path = temp.path().join("workspace");
        let backend = Arc::new(TestBackend::new(temp.path()));
        let live_path = backend.data_root.join(WS_ID);
        std::fs::create_dir_all(&live_path).expect("live workspace directory");
        if nonempty {
            std::fs::write(live_path.join("file.txt"), "content").expect("workspace file");
        }
        symlink(&live_path, &workspace_path).expect("registered workspace symlink");
        let config = DaemonConfig {
            socket_path: temp.path().join("daemon.sock"),
            ..DaemonConfig::default()
        };
        let state = Arc::new(DaemonState::new(
            config,
            backend.clone(),
            temp.path().join("state"),
        ));
        state.register_workspace(
            WS_ID.to_string(),
            workspace_path.clone(),
            SnapshotIndex::new(workspace_path.clone()),
        );
        Self {
            _temp: temp,
            state,
            backend,
            workspace_path,
        }
    }

    async fn add_target_snapshot(&self) {
        let workspace = self.state.get_by_wsid(WS_ID).expect("registered workspace");
        let mut workspace = workspace.write().await;
        workspace.index.snapshots.insert(
            TARGET_ID.to_string(),
            SnapshotMeta {
                message: Some("target".to_string()),
                metadata: None,
                pinned: true,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        workspace.index.head = Some(TARGET_ID.to_string());
    }
}

fn assert_rejected(response: Response, expected: GuardedCheckpointRejectionCodeV2) {
    match response {
        Response::GuardedCheckpointV2Rejected { code, .. } => assert_eq!(code, expected),
        other => panic!("expected guarded rejection {expected:?}, got {other:?}"),
    }
}

fn assert_rollback_rejected(response: Response, expected: GuardedRollbackRejectionCodeV2) {
    match response {
        Response::GuardedRollbackV2Rejected { code, .. } => assert_eq!(code, expected),
        other => panic!("expected guarded rollback rejection {expected:?}, got {other:?}"),
    }
}

async fn preview_digest(fixture: &Fixture, caller_uid: u32) -> [u8; 32] {
    match rollback_preview(
        &fixture.state,
        Some(caller_uid),
        fixture.workspace_path.to_str().expect("utf8 path"),
        WS_ID,
        fixture.backend.generation,
        TARGET_ID,
    )
    .await
    {
        Response::GuardedRollbackPreviewV2Ok { diff_digest, .. } => diff_digest,
        other => panic!("expected guarded rollback preview, got {other:?}"),
    }
}

async fn switch(fixture: &Fixture, caller_uid: u32, diff_digest: [u8; 32]) -> Response {
    rollback(
        &fixture.state,
        Some(caller_uid),
        fixture.workspace_path.to_str().expect("utf8 path"),
        WS_ID,
        fixture.backend.generation,
        TARGET_ID,
        diff_digest,
        OPERATION_ID,
        OPERATION_DIGEST,
    )
    .await
}

async fn create_checkpoint(fixture: &Fixture, caller_uid: u32) -> Response {
    checkpoint(
        &fixture.state,
        Some(caller_uid),
        WS_ID,
        fixture.backend.generation,
        CHECKPOINT_ID,
        DIGEST,
        Some("message".to_string()),
        Some(r#"{"key":"value"}"#.to_string()),
        false,
    )
    .await
}

#[tokio::test]
async fn identity_uses_exact_absolute_registration_without_bootstrap() {
    let fixture = Fixture::new(true);
    let response = workspace_identity(
        &fixture.state,
        fixture.workspace_path.to_str().expect("utf8 path"),
    )
    .await;

    match response {
        Response::WorkspaceIdentityV2Ok {
            ws_id,
            registered_path,
            generation,
            ..
        } => {
            assert_eq!(ws_id, WS_ID);
            assert_eq!(registered_path, fixture.workspace_path.to_string_lossy());
            assert_eq!(generation, fixture.backend.generation);
        }
        other => panic!("expected identity, got {other:?}"),
    }
    assert_eq!(fixture.backend.bootstrap_calls.load(Ordering::SeqCst), 0);

    assert_rejected(
        workspace_identity(&fixture.state, "workspace").await,
        GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
    );
    assert_rejected(
        workspace_identity(&fixture.state, "/tmp/../workspace").await,
        GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
    );

    let invalid_path = fixture._temp.path().join("invalid-workspace-id");
    std::fs::create_dir(&invalid_path).expect("invalid-id workspace directory");
    fixture.state.register_workspace(
        "legacy-invalid".to_string(),
        invalid_path.clone(),
        SnapshotIndex::new(invalid_path.clone()),
    );
    assert_rejected(
        workspace_identity(
            &fixture.state,
            invalid_path.to_str().expect("utf8 invalid-id path"),
        )
        .await,
        GuardedCheckpointRejectionCodeV2::InvalidWorkspaceId,
    );
}

#[tokio::test]
async fn guarded_request_never_auto_initializes_or_bootstraps() {
    let fixture = Fixture::new(true);
    assert_rejected(
        checkpoint(
            &fixture.state,
            Some(1000),
            "ws-123456",
            fixture.backend.generation,
            CHECKPOINT_ID,
            DIGEST,
            None,
            None,
            false,
        )
        .await,
        GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
    );
    assert_eq!(fixture.backend.bootstrap_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replaced_registration_path_never_drives_checkpoint_state() {
    let fixture = Fixture::new(true);
    std::fs::remove_file(&fixture.workspace_path).expect("remove registered symlink");
    let replacement = fixture._temp.path().join("replacement");
    std::fs::create_dir(&replacement).expect("replacement directory");
    symlink(&replacement, &fixture.workspace_path).expect("replacement symlink");

    assert_rejected(
        workspace_identity(
            &fixture.state,
            fixture.workspace_path.to_str().expect("utf8 path"),
        )
        .await,
        GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
    );
    assert_rejected(
        create_checkpoint(&fixture, 1000).await,
        GuardedCheckpointRejectionCodeV2::InvalidRegistrationPath,
    );
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stale_generation_and_invalid_metadata_do_not_create_snapshot() {
    let fixture = Fixture::new(true);
    assert_rejected(
        checkpoint(
            &fixture.state,
            Some(1000),
            WS_ID,
            WorkspaceGenerationTokenV2::from_bytes([8; 32]),
            CHECKPOINT_ID,
            DIGEST,
            None,
            None,
            false,
        )
        .await,
        GuardedCheckpointRejectionCodeV2::GenerationMismatch,
    );
    assert_rejected(
        checkpoint(
            &fixture.state,
            Some(1000),
            WS_ID,
            fixture.backend.generation,
            CHECKPOINT_ID,
            DIGEST,
            None,
            Some("{".to_string()),
            false,
        )
        .await,
        GuardedCheckpointRejectionCodeV2::InvalidMetadata,
    );
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dispatcher_rejects_guarded_request_without_peer_context() {
    let fixture = Fixture::new(true);
    let response = dispatcher::dispatch(
        &fixture.state,
        Request::GuardedCheckpointV2 {
            ws_id: WS_ID.to_string(),
            expected_generation: fixture.backend.generation,
            checkpoint_id: CHECKPOINT_ID.to_string(),
            operation_digest: DIGEST,
            message: None,
            metadata: None,
            pin: false,
        },
    )
    .await;
    assert_rejected(
        response,
        GuardedCheckpointRejectionCodeV2::PeerCredentialsUnavailable,
    );
}

#[tokio::test]
async fn listener_binds_kernel_peer_uid_for_guarded_round_trip() {
    let fixture = Fixture::new(false);
    let cancel = CancellationToken::new();
    let listener_state = fixture.state.clone();
    let listener_cancel = cancel.clone();
    let listener = tokio::spawn(async move {
        crate::listener::run_listener(listener_state, listener_cancel).await
    });

    let mut client = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match tokio::net::UnixStream::connect(&fixture.state.socket_path).await {
                Ok(client) => break client,
                Err(_) if listener.is_finished() => panic!("listener exited before accepting"),
                Err(_) => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("listener socket did not become ready");
    let request = Request::GuardedCheckpointV2 {
        ws_id: WS_ID.to_string(),
        expected_generation: fixture.backend.generation,
        checkpoint_id: CHECKPOINT_ID.to_string(),
        operation_digest: DIGEST,
        message: None,
        metadata: None,
        pin: false,
    };
    client
        .write_all(&encode_frame(&request).expect("encode request"))
        .await
        .expect("send request");
    let len = client.read_u32_le().await.expect("response length");
    let mut payload = vec![0; len as usize];
    client
        .read_exact(&mut payload)
        .await
        .expect("response payload");
    let response: Response = decode_payload(&payload).expect("decode response");
    match response {
        Response::GuardedCheckpointV2Ok { evidence } => {
            assert_eq!(evidence.caller_uid, nix::unistd::geteuid().as_raw());
            assert!(matches!(
                evidence.outcome,
                GuardedCheckpointOutcomeV2::Created { .. }
            ));
        }
        other => panic!("expected guarded response, got {other:?}"),
    }

    cancel.cancel();
    listener.await.expect("listener task").expect("listener");
}

#[tokio::test]
async fn empty_workspace_checkpoint_is_created_and_published_after_durable_save() {
    let fixture = Fixture::new(false);
    let response = create_checkpoint(&fixture, 1000).await;
    match response {
        Response::GuardedCheckpointV2Ok { evidence } => assert!(matches!(
            evidence.outcome,
            GuardedCheckpointOutcomeV2::Created { .. }
        )),
        other => panic!("expected created evidence, got {other:?}"),
    }
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 1);
    let workspace = fixture.state.get_by_wsid(WS_ID).expect("registered");
    let workspace = workspace.read().await;
    assert!(workspace.index.snapshots.contains_key(CHECKPOINT_ID));
    assert!(workspace
        .index
        .governed_evidence
        .contains_key(CHECKPOINT_ID));
}

#[tokio::test]
async fn empty_workspace_save_failure_does_not_publish_evidence_in_memory() {
    let fixture = Fixture::new(false);
    std::fs::create_dir_all(
        fixture
            .state
            .index_dir(WS_ID)
            .parent()
            .expect("index parent"),
    )
    .expect("create index parent");
    std::fs::write(fixture.state.index_dir(WS_ID), "not a directory")
        .expect("create index path obstruction");

    assert!(matches!(
        create_checkpoint(&fixture, 1000).await,
        Response::Error { .. }
    ));
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 1);
    let workspace = fixture.state.get_by_wsid(WS_ID).expect("registered");
    let workspace = workspace.read().await;
    assert!(!workspace.index.snapshots.contains_key(CHECKPOINT_ID));
    assert!(!workspace
        .index
        .governed_evidence
        .contains_key(CHECKPOINT_ID));
}

#[tokio::test]
async fn exact_duplicate_is_idempotent_and_conflicting_digest_is_rejected() {
    let fixture = Fixture::new(true);
    assert!(matches!(
        create_checkpoint(&fixture, 1000).await,
        Response::GuardedCheckpointV2Ok { .. }
    ));
    assert!(matches!(
        create_checkpoint(&fixture, 1000).await,
        Response::GuardedCheckpointV2Ok { .. }
    ));
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 1);

    assert_rejected(
        checkpoint(
            &fixture.state,
            Some(1000),
            WS_ID,
            fixture.backend.generation,
            CHECKPOINT_ID,
            [3; 32],
            None,
            None,
            false,
        )
        .await,
        GuardedCheckpointRejectionCodeV2::OperationConflict,
    );
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn evidence_limit_evicts_skipped_or_confirmed_missing_records() {
    use ws_ckpt_common::{GuardedCheckpointEvidenceV2, SnapshotMeta};

    let generation = WorkspaceGenerationTokenV2::from_bytes([7; 32]);
    let mut skipped = SnapshotIndex::new(PathBuf::from("/workspace"));
    for index in 0..GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2 {
        let checkpoint_id = format!("skip-{index}");
        skipped.governed_evidence.insert(
            checkpoint_id.clone(),
            GuardedCheckpointEvidenceV2 {
                ws_id: WS_ID.to_string(),
                registered_path: "/workspace".to_string(),
                generation,
                checkpoint_id,
                operation_digest: [index as u8; 32],
                caller_uid: 1000,
                outcome: GuardedCheckpointOutcomeV2::Skipped {
                    reason: "empty".to_string(),
                },
            },
        );
    }
    let with_slot = index_with_evidence_slot(&skipped).expect("skips are evictable");
    assert_eq!(
        with_slot.governed_evidence.len(),
        GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2 - 1
    );

    let mut created = SnapshotIndex::new(PathBuf::from("/workspace"));
    for index in 0..GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2 {
        let checkpoint_id = format!("created-{index}");
        created.snapshots.insert(
            checkpoint_id.clone(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        created.governed_evidence.insert(
            checkpoint_id.clone(),
            GuardedCheckpointEvidenceV2 {
                ws_id: WS_ID.to_string(),
                registered_path: "/workspace".to_string(),
                generation,
                checkpoint_id: checkpoint_id.clone(),
                operation_digest: [index as u8; 32],
                caller_uid: 1000,
                outcome: GuardedCheckpointOutcomeV2::Created {
                    snapshot_id: checkpoint_id,
                },
            },
        );
    }
    assert!(index_with_evidence_slot(&created).is_none());
    created.snapshots.remove("created-0");
    assert!(
        index_with_evidence_slot(&created).is_none(),
        "temporarily detached Created evidence must not be evicted"
    );
    created.snapshots.insert(
        "created-0".to_string(),
        SnapshotMeta {
            message: None,
            metadata: None,
            pinned: false,
            created_at: chrono::Utc::now(),
            missing: true,
            parent_id: None,
            child_ids: vec![],
        },
    );
    assert_eq!(
        index_with_evidence_slot(&created)
            .expect("confirmed missing Created evidence is evictable")
            .governed_evidence
            .len(),
        GUARDED_CHECKPOINT_EVIDENCE_LIMIT_V2 - 1
    );
}

#[tokio::test]
async fn evidence_survives_a_legacy_save_and_fresh_state_load() {
    let fixture = Fixture::new(true);
    assert!(matches!(
        create_checkpoint(&fixture, 1000).await,
        Response::GuardedCheckpointV2Ok { .. }
    ));

    let index_dir = fixture.state.index_dir(WS_ID);
    let current_index = fixture
        .state
        .get_by_wsid(WS_ID)
        .expect("registered")
        .read()
        .await
        .index
        .clone();
    crate::index_store::save(&index_dir, &current_index)
        .await
        .expect("legacy save");
    let loaded = crate::index_store::load(&index_dir)
        .await
        .expect("fresh index load");

    let restarted = Arc::new(DaemonState::new(
        DaemonConfig {
            socket_path: fixture._temp.path().join("restarted.sock"),
            ..DaemonConfig::default()
        },
        fixture.backend.clone(),
        fixture._temp.path().join("restarted-state"),
    ));
    restarted.register_workspace(WS_ID.to_string(), fixture.workspace_path.clone(), loaded);
    assert!(matches!(
        checkpoint_evidence(
            &restarted,
            Some(1000),
            WS_ID,
            fixture.backend.generation,
            CHECKPOINT_ID,
            DIGEST,
        )
        .await,
        Response::CheckpointEvidenceV2Ok { evidence: Some(_) }
    ));
}

#[tokio::test]
async fn evidence_requires_exact_binding_and_hides_missing_created_snapshot() {
    let fixture = Fixture::new(true);
    assert!(matches!(
        create_checkpoint(&fixture, 1000).await,
        Response::GuardedCheckpointV2Ok { .. }
    ));

    assert_rejected(
        checkpoint_evidence(
            &fixture.state,
            Some(1001),
            WS_ID,
            fixture.backend.generation,
            CHECKPOINT_ID,
            DIGEST,
        )
        .await,
        GuardedCheckpointRejectionCodeV2::CallerMismatch,
    );
    assert_rejected(
        checkpoint_evidence(
            &fixture.state,
            Some(1000),
            WS_ID,
            fixture.backend.generation,
            CHECKPOINT_ID,
            [4; 32],
        )
        .await,
        GuardedCheckpointRejectionCodeV2::OperationConflict,
    );

    let workspace = fixture.state.get_by_wsid(WS_ID).expect("registered");
    workspace
        .write()
        .await
        .index
        .snapshots
        .get_mut(CHECKPOINT_ID)
        .expect("snapshot meta")
        .missing = true;
    assert!(matches!(
        checkpoint_evidence(
            &fixture.state,
            Some(1000),
            WS_ID,
            fixture.backend.generation,
            CHECKPOINT_ID,
            DIGEST,
        )
        .await,
        Response::CheckpointEvidenceV2Ok { evidence: None }
    ));
}

#[tokio::test]
async fn evidence_does_not_compare_against_current_live_generation() {
    let fixture = Fixture::new(false);
    assert!(matches!(
        create_checkpoint(&fixture, 1000).await,
        Response::GuardedCheckpointV2Ok { .. }
    ));
    let generation_calls = fixture.backend.generation_calls.load(Ordering::SeqCst);

    assert!(matches!(
        checkpoint_evidence(
            &fixture.state,
            Some(1000),
            WS_ID,
            fixture.backend.generation,
            CHECKPOINT_ID,
            DIGEST,
        )
        .await,
        Response::CheckpointEvidenceV2Ok { evidence: Some(_) }
    ));
    assert_eq!(
        fixture.backend.generation_calls.load(Ordering::SeqCst),
        generation_calls
    );
}

#[tokio::test]
async fn identity_rechecks_mapping_after_lifecycle_lock_wait() {
    let fixture = Fixture::new(true);
    let guard = fixture.state.lock_wsid(WS_ID).await;
    let state = fixture.state.clone();
    let path = fixture.workspace_path.to_string_lossy().into_owned();
    let lookup = tokio::spawn(async move { workspace_identity(&state, &path).await });
    tokio::task::yield_now().await;
    assert!(!lookup.is_finished());

    fixture.state.unregister_workspace(WS_ID).await;
    drop(guard);
    assert_rejected(
        lookup.await.expect("identity task"),
        GuardedCheckpointRejectionCodeV2::WorkspaceNotFound,
    );
}

#[tokio::test]
async fn post_backend_save_failure_does_not_publish_created_evidence_in_memory() {
    let fixture = Fixture::new(true);
    std::fs::create_dir_all(
        fixture
            .state
            .index_dir(WS_ID)
            .parent()
            .expect("index parent"),
    )
    .expect("create index parent");
    std::fs::write(fixture.state.index_dir(WS_ID), "not a directory")
        .expect("create index path obstruction");

    let response = create_checkpoint(&fixture, 1000).await;
    assert!(matches!(response, Response::Error { .. }));
    assert_eq!(fixture.backend.create_calls.load(Ordering::SeqCst), 1);
    let workspace = fixture.state.get_by_wsid(WS_ID).expect("registered");
    let workspace = workspace.read().await;
    assert!(!workspace.index.snapshots.contains_key(CHECKPOINT_ID));
    assert!(!workspace
        .index
        .governed_evidence
        .contains_key(CHECKPOINT_ID));
}

#[tokio::test]
async fn guarded_rollback_succeeds_with_exact_preview_and_durable_bindings() {
    let fixture = Fixture::new(true);
    fixture.add_target_snapshot().await;
    let digest = preview_digest(&fixture, 1000).await;

    let response = switch(&fixture, 1000, digest).await;
    let evidence = match response {
        Response::GuardedRollbackV2Ok { evidence } => evidence,
        other => panic!("expected guarded rollback success, got {other:?}"),
    };
    assert_eq!(evidence.ws_id, WS_ID);
    assert_eq!(
        evidence.registered_path,
        fixture.workspace_path.to_string_lossy()
    );
    assert_eq!(evidence.expected_generation, fixture.backend.generation);
    assert_eq!(evidence.target_snapshot_id, TARGET_ID);
    assert_eq!(evidence.expected_diff_digest, digest);
    assert_eq!(evidence.operation_id, OPERATION_ID);
    assert_eq!(evidence.operation_digest, OPERATION_DIGEST);
    assert_eq!(evidence.caller_uid, 1000);
    assert!(matches!(
        evidence.outcome,
        GuardedRollbackOutcomeV2::Succeeded { .. }
    ));
    assert_eq!(fixture.backend.rollback_calls.load(Ordering::SeqCst), 1);

    assert!(matches!(
        switch(&fixture, 1000, digest).await,
        Response::GuardedRollbackV2Ok { .. }
    ));
    assert_eq!(
        fixture.backend.rollback_calls.load(Ordering::SeqCst),
        1,
        "an exact replay must return evidence without invoking rollback again"
    );
    assert!(matches!(
        rollback_evidence(
            &fixture.state,
            Some(1000),
            WS_ID,
            OPERATION_ID,
            OPERATION_DIGEST,
        )
        .await,
        Response::GuardedRollbackEvidenceV2Ok { evidence: Some(_) }
    ));
}

#[tokio::test]
async fn mutation_after_preview_rejects_guarded_rollback_before_backend() {
    let fixture = Fixture::new(true);
    fixture.add_target_snapshot().await;
    *fixture.backend.diff_after_first.lock().unwrap() = Some(vec![DiffEntry {
        path: "2.txt".to_string(),
        change_type: ChangeType::Added,
        detail: Some("mutated after preview".to_string()),
    }]);
    let digest = preview_digest(&fixture, 1000).await;

    assert_rollback_rejected(
        switch(&fixture, 1000, digest).await,
        GuardedRollbackRejectionCodeV2::DiffMismatch,
    );
    assert_eq!(fixture.backend.rollback_calls.load(Ordering::SeqCst), 0);
    let workspace = fixture
        .state
        .get_by_wsid(WS_ID)
        .expect("registered workspace");
    assert!(!workspace
        .read()
        .await
        .index
        .guarded_rollbacks
        .contains_key(OPERATION_ID));
}

#[tokio::test]
async fn guarded_rollback_rejects_wrong_generation_digest_and_full_id() {
    let fixture = Fixture::new(true);
    fixture.add_target_snapshot().await;
    let digest = preview_digest(&fixture, 1000).await;

    assert_rollback_rejected(
        rollback(
            &fixture.state,
            Some(1000),
            fixture.workspace_path.to_str().expect("utf8 path"),
            WS_ID,
            WorkspaceGenerationTokenV2::from_bytes([8; 32]),
            TARGET_ID,
            digest,
            "wrong-generation",
            OPERATION_DIGEST,
        )
        .await,
        GuardedRollbackRejectionCodeV2::GenerationMismatch,
    );
    assert_rollback_rejected(
        rollback(
            &fixture.state,
            Some(1000),
            fixture.workspace_path.to_str().expect("utf8 path"),
            WS_ID,
            fixture.backend.generation,
            TARGET_ID,
            [0xff; 32],
            "wrong-digest",
            OPERATION_DIGEST,
        )
        .await,
        GuardedRollbackRejectionCodeV2::DiffMismatch,
    );
    assert_rollback_rejected(
        rollback(
            &fixture.state,
            Some(1000),
            fixture.workspace_path.to_str().expect("utf8 path"),
            WS_ID,
            fixture.backend.generation,
            "target-snapshot",
            digest,
            "prefix-target",
            OPERATION_DIGEST,
        )
        .await,
        GuardedRollbackRejectionCodeV2::SnapshotNotFound,
    );
    assert_eq!(fixture.backend.rollback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn guarded_rollback_evidence_is_bound_to_kernel_peer_uid() {
    let fixture = Fixture::new(true);
    fixture.add_target_snapshot().await;
    let digest = preview_digest(&fixture, 1000).await;
    assert!(matches!(
        switch(&fixture, 1000, digest).await,
        Response::GuardedRollbackV2Ok { .. }
    ));

    assert_rollback_rejected(
        switch(&fixture, 1001, digest).await,
        GuardedRollbackRejectionCodeV2::CallerMismatch,
    );
    assert_rollback_rejected(
        rollback_evidence(
            &fixture.state,
            Some(1001),
            WS_ID,
            OPERATION_ID,
            OPERATION_DIGEST,
        )
        .await,
        GuardedRollbackRejectionCodeV2::CallerMismatch,
    );
    assert_eq!(fixture.backend.rollback_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn uncertain_guarded_rollback_is_durable_and_never_replayed() {
    let fixture = Fixture::new(true);
    fixture.add_target_snapshot().await;
    *fixture.backend.rollback_error.lock().unwrap() =
        Some("backend completion is unknown".to_string());
    let digest = preview_digest(&fixture, 1000).await;

    let first = switch(&fixture, 1000, digest).await;
    match first {
        Response::GuardedRollbackV2Uncertain { evidence } => assert!(matches!(
            evidence.outcome,
            GuardedRollbackOutcomeV2::Unknown { .. }
        )),
        other => panic!("expected uncertain guarded rollback, got {other:?}"),
    }
    assert_eq!(fixture.backend.rollback_calls.load(Ordering::SeqCst), 1);

    assert!(matches!(
        switch(&fixture, 1000, digest).await,
        Response::GuardedRollbackV2Uncertain { .. }
    ));
    assert_eq!(
        fixture.backend.rollback_calls.load(Ordering::SeqCst),
        1,
        "an uncertain operation must never invoke the backend again"
    );
    assert!(matches!(
        rollback_evidence(
            &fixture.state,
            Some(1000),
            WS_ID,
            OPERATION_ID,
            OPERATION_DIGEST,
        )
        .await,
        Response::GuardedRollbackEvidenceV2Ok {
            evidence: Some(ws_ckpt_common::GuardedRollbackEvidenceV2 {
                outcome: GuardedRollbackOutcomeV2::Unknown { .. },
                ..
            })
        }
    ));

    let loaded = crate::index_store::load(&fixture.state.index_dir(WS_ID))
        .await
        .expect("load durable rollback evidence");
    let restarted = Arc::new(DaemonState::new(
        DaemonConfig {
            socket_path: fixture._temp.path().join("rollback-restarted.sock"),
            ..DaemonConfig::default()
        },
        fixture.backend.clone(),
        fixture._temp.path().join("rollback-restarted-state"),
    ));
    restarted.register_workspace(WS_ID.to_string(), fixture.workspace_path.clone(), loaded);
    assert!(matches!(
        rollback(
            &restarted,
            Some(1000),
            fixture.workspace_path.to_str().expect("utf8 path"),
            WS_ID,
            fixture.backend.generation,
            TARGET_ID,
            digest,
            OPERATION_ID,
            OPERATION_DIGEST,
        )
        .await,
        Response::GuardedRollbackV2Uncertain { .. }
    ));
    assert_eq!(fixture.backend.rollback_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn guarded_rollback_diff_digest_is_order_independent_and_content_sensitive() {
    let first = DiffEntry {
        path: "b.txt".to_string(),
        change_type: ChangeType::Modified,
        detail: Some("old -> new".to_string()),
    };
    let second = DiffEntry {
        path: "a.txt".to_string(),
        change_type: ChangeType::Deleted,
        detail: None,
    };
    assert_eq!(
        rollback_diff_digest(&[first.clone(), second.clone()]),
        rollback_diff_digest(&[second.clone(), first.clone()])
    );
    assert_ne!(
        rollback_diff_digest(&[first, second.clone()]),
        rollback_diff_digest(&[second])
    );
}
