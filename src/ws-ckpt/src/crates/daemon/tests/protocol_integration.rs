//! Integration test: full IPC protocol round-trip over Unix Socket.
//!
//! Spins up a mock server on a temporary Unix Socket, sends a Request
//! from a "client", and verifies the Response comes back correctly.

use std::path::PathBuf;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use ws_ckpt_common::{
    decode_payload, encode_frame, ChangeType, CleanupRetention, ConfigReport, DiffEntry,
    EffectivePolicy, GlobalPolicySnapshot, GuardedCheckpointEvidenceV2, GuardedCheckpointOutcomeV2,
    GuardedRollbackEvidenceV2, GuardedRollbackOutcomeV2, PolicyFieldOp, Request, Response,
    SnapshotEntry, SnapshotMeta, StatusReport, WorkspaceGenerationTokenV2, WorkspaceInfo,
    WorkspacePolicy,
};

/// Helper: create a temporary socket path using tempfile
fn temp_socket_path() -> PathBuf {
    let dir = tempfile::tempdir().expect("failed to create tempdir");
    // We leak the tempdir so it's not cleaned up during the test.
    // The OS will clean up /tmp on reboot.
    let path = dir.path().join("test.sock");
    std::mem::forget(dir);
    path
}

/// Server side: read one request frame, process it, send a response frame
async fn mock_server_handle(mut stream: tokio::net::UnixStream) {
    // 1. Read 4-byte LE length
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.expect("read len");
    let len = u32::from_le_bytes(len_buf) as usize;

    // 2. Read payload
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.expect("read payload");

    // 3. Decode request
    let request: Request = decode_payload(&payload).expect("decode request");

    // 4. Build response based on request type
    let response = match request {
        Request::Init { workspace } => Response::InitOk {
            ws_id: format!("ws-{}", &workspace[..6.min(workspace.len())]),
        },
        Request::Checkpoint { .. } => Response::CheckpointOk {
            snapshot_id: "msg1-step0".to_string(),
        },
        Request::Rollback { to, .. } => Response::RollbackOk {
            from: "ws-test".to_string(),
            to: to.unwrap_or_default(),
        },
        Request::RollbackPreview { to, .. } => Response::RollbackPreviewOk {
            to: to.unwrap_or_default(),
            changes: vec![DiffEntry {
                path: "src/main.rs".to_string(),
                change_type: ChangeType::Modified,
                detail: Some("content changed".to_string()),
            }],
        },
        Request::Delete { snapshot, .. } => Response::DeleteOk { target: snapshot },
        Request::List { .. } => Response::ListOk {
            snapshots: vec![SnapshotEntry {
                id: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
                workspace: "/home/user/ws".to_string(),
                meta: SnapshotMeta {
                    message: Some("initial".to_string()),
                    metadata: None,
                    pinned: false,
                    created_at: chrono::Utc::now(),
                    missing: false,
                    parent_id: None,
                    child_ids: vec![],
                },
            }],
        },
        Request::Diff { .. } => Response::DiffOk {
            changes: vec![DiffEntry {
                path: "src/main.rs".to_string(),
                change_type: ChangeType::Modified,
                detail: None,
            }],
        },
        Request::Status { .. } => Response::StatusOk {
            report: StatusReport {
                uptime_secs: 42,
                workspaces: vec![WorkspaceInfo {
                    ws_id: "ws-test".to_string(),
                    path: "/tmp/ws".to_string(),
                    snapshot_count: 3,
                }],
                fs_total_bytes: 1_000_000_000,
                fs_used_bytes: 500_000_000,
            },
        },
        Request::Cleanup { .. } => Response::CleanupOk {
            removed: vec!["msg1-step0".to_string()],
        },
        Request::Config => Response::ConfigOk {
            config: ConfigReport {
                mount_path: "/mnt/btrfs-workspace".to_string(),
                socket_path: "/run/ws-ckpt/ws-ckpt.sock".to_string(),
                log_level: "info".to_string(),
                auto_cleanup: false,
                auto_cleanup_keep: CleanupRetention::Count(20),
                auto_cleanup_interval_secs: 86_400,
                health_check_interval_secs: 300,
                img_size: 30,
                img_max_percent: 40.0,
            },
        },
        Request::ReloadConfig
        | Request::ReloadGlobalConfig
        | Request::ReloadWorkspacePolicy { .. } => Response::ReloadConfigOk {
            config: ConfigReport {
                mount_path: "/mnt/btrfs-workspace".to_string(),
                socket_path: "/run/ws-ckpt/ws-ckpt.sock".to_string(),
                log_level: "info".to_string(),
                auto_cleanup: false,
                auto_cleanup_keep: CleanupRetention::Count(20),
                auto_cleanup_interval_secs: 86_400,
                health_check_interval_secs: 300,
                img_size: 30,
                img_max_percent: 40.0,
            },
            file: None,
        },
        Request::ConfigOverview => Response::ConfigOverviewOk {
            config: ConfigReport {
                mount_path: "/mnt/btrfs-workspace".to_string(),
                socket_path: "/run/ws-ckpt/ws-ckpt.sock".to_string(),
                log_level: "info".to_string(),
                auto_cleanup: false,
                auto_cleanup_keep: CleanupRetention::Count(20),
                auto_cleanup_interval_secs: 86_400,
                health_check_interval_secs: 300,
                img_size: 30,
                img_max_percent: 40.0,
            },
            ws_total: 3,
            ws_with_override: 1,
        },
        Request::Unregister { workspace } => Response::UnregisterOk {
            workspace,
            retained_paths: Vec::new(),
        },
        Request::Recover { workspace } => Response::RecoverOk { workspace },
        Request::HealthAdvisory => Response::HealthAdvisoryOk {
            over_limit_workspace_count: 0,
            fs_total_bytes: 1_000_000_000,
            fs_used_bytes: 500_000_000,
        },
        // Each policy-shaped variant returns a *distinct* response so the
        // matching #[tokio::test] cases below actually catch wire-format
        // regressions: a different shape per request rules out
        // "the test only ever decoded one shape, schema drift went unnoticed".
        Request::GetWorkspacePolicy { workspace } => Response::WorkspacePolicyOk {
            ws_id: format!("get:{}", workspace),
            effective: EffectivePolicy {
                auto_cleanup: true,
                auto_cleanup_keep: CleanupRetention::Count(5),
            },
            local: WorkspacePolicy::default(),
            global: GlobalPolicySnapshot {
                auto_cleanup: true,
                auto_cleanup_keep: CleanupRetention::Count(20),
            },
        },
        Request::ResetWorkspacePolicy { workspace } => {
            // Mirror the real daemon's Reset semantics: the file is gone,
            // local is empty, effective == global. The ws_id prefix tells
            // the test that the right variant arrived (vs Patch).
            let global = GlobalPolicySnapshot {
                auto_cleanup: false,
                auto_cleanup_keep: CleanupRetention::Count(20),
            };
            let effective = EffectivePolicy {
                auto_cleanup: global.auto_cleanup,
                auto_cleanup_keep: global.auto_cleanup_keep.clone(),
            };
            Response::WorkspacePolicyOk {
                ws_id: format!("reset:{}", workspace),
                effective,
                local: WorkspacePolicy::default(),
                global,
            }
        }
        Request::PatchWorkspacePolicy {
            workspace,
            auto_cleanup,
            auto_cleanup_keep,
        } => {
            // Apply the patch on top of an empty baseline so the test can
            // verify the wire-format actually round-trips PolicyFieldOp.
            let local = WorkspacePolicy {
                auto_cleanup: auto_cleanup.apply(None),
                auto_cleanup_keep: auto_cleanup_keep.apply(None),
            };
            let effective = EffectivePolicy {
                auto_cleanup: local.auto_cleanup.unwrap_or(false),
                auto_cleanup_keep: local
                    .auto_cleanup_keep
                    .clone()
                    .unwrap_or(CleanupRetention::Count(20)),
            };
            Response::WorkspacePolicyOk {
                ws_id: format!("patch:{}", workspace),
                effective,
                local,
                global: GlobalPolicySnapshot {
                    auto_cleanup: false,
                    auto_cleanup_keep: CleanupRetention::Count(20),
                },
            }
        }
        Request::WorkspaceIdentityV2 { registration_path } => Response::WorkspaceIdentityV2Ok {
            protocol_version: 2,
            ws_id: "ws-abcdef".to_string(),
            registered_path: registration_path,
            generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
        },
        Request::GuardedCheckpointV2 {
            ws_id,
            expected_generation,
            checkpoint_id,
            operation_digest,
            ..
        } => Response::GuardedCheckpointV2Ok {
            evidence: GuardedCheckpointEvidenceV2 {
                ws_id,
                registered_path: "/tmp/ws".to_string(),
                generation: expected_generation,
                checkpoint_id: checkpoint_id.clone(),
                operation_digest,
                caller_uid: 1000,
                outcome: GuardedCheckpointOutcomeV2::Created {
                    snapshot_id: checkpoint_id,
                },
            },
        },
        Request::CheckpointEvidenceV2 {
            ws_id,
            expected_generation,
            checkpoint_id,
            operation_digest,
        } => Response::CheckpointEvidenceV2Ok {
            evidence: Some(GuardedCheckpointEvidenceV2 {
                ws_id,
                registered_path: "/tmp/ws".to_string(),
                generation: expected_generation,
                checkpoint_id: checkpoint_id.clone(),
                operation_digest,
                caller_uid: 1000,
                outcome: GuardedCheckpointOutcomeV2::Skipped {
                    reason: "empty workspace".to_string(),
                },
            }),
        },
        Request::GuardedRollbackPreviewV2 {
            registered_path,
            ws_id,
            expected_generation,
            target_snapshot_id,
        } => Response::GuardedRollbackPreviewV2Ok {
            protocol_version: 2,
            registered_path,
            ws_id,
            generation: expected_generation,
            target_snapshot_id,
            diff_digest: [8; 32],
            changes: vec![],
            caller_uid: 1000,
        },
        Request::GuardedRollbackV2 {
            registered_path,
            ws_id,
            expected_generation,
            target_snapshot_id,
            expected_diff_digest,
            operation_id,
            operation_digest,
        } => Response::GuardedRollbackV2Ok {
            evidence: GuardedRollbackEvidenceV2 {
                ws_id,
                registered_path,
                expected_generation,
                target_snapshot_id,
                expected_diff_digest,
                operation_id,
                operation_digest,
                caller_uid: 1000,
                outcome: GuardedRollbackOutcomeV2::Succeeded {
                    resulting_generation: WorkspaceGenerationTokenV2::from_bytes([9; 32]),
                },
            },
        },
        Request::GuardedRollbackEvidenceV2 {
            ws_id,
            operation_id,
            operation_digest,
        } => Response::GuardedRollbackEvidenceV2Ok {
            evidence: Some(GuardedRollbackEvidenceV2 {
                ws_id,
                registered_path: "/tmp/ws".to_string(),
                expected_generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                target_snapshot_id: "target".to_string(),
                expected_diff_digest: [8; 32],
                operation_id,
                operation_digest,
                caller_uid: 1000,
                outcome: GuardedRollbackOutcomeV2::Started,
            }),
        },
    };

    // 5. Encode and send response frame
    let frame = encode_frame(&response).expect("encode response");
    stream.write_all(&frame).await.expect("write response");
}

#[tokio::test]
async fn full_init_request_response_over_socket() {
    let socket_path = temp_socket_path();

    // Start server
    let listener = UnixListener::bind(&socket_path).expect("bind failed");
    let server_handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept failed");
        mock_server_handle(stream).await;
    });

    // Give server a moment to start
    tokio::task::yield_now().await;

    // Client connects
    let mut client = UnixStream::connect(&socket_path)
        .await
        .expect("connect failed");

    // Send Init request
    let request = Request::Init {
        workspace: "/tmp/my-workspace".to_string(),
    };
    let frame = encode_frame(&request).expect("encode request");
    client.write_all(&frame).await.expect("write request");

    // Read response
    let mut len_buf = [0u8; 4];
    client
        .read_exact(&mut len_buf)
        .await
        .expect("read resp len");
    let len = u32::from_le_bytes(len_buf) as usize;

    let mut payload = vec![0u8; len];
    client
        .read_exact(&mut payload)
        .await
        .expect("read resp payload");

    let response: Response = decode_payload(&payload).expect("decode response");

    // Verify
    match response {
        Response::InitOk { ws_id } => {
            assert!(ws_id.starts_with("ws-"));
        }
        _ => panic!("expected InitOk, got {:?}", response),
    }

    server_handle.await.unwrap();
}

#[tokio::test]
async fn full_checkpoint_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::Checkpoint {
        workspace: "/ws".to_string(),
        id: "msg1-step0".to_string(),
        message: Some("test message".to_string()),
        metadata: None,
        pin: true,
    };
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::CheckpointOk { snapshot_id } => {
            assert_eq!(snapshot_id, "msg1-step0");
        }
        _ => panic!("expected CheckpointOk, got {:?}", response),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn full_rollback_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::Rollback {
        workspace: "/ws".to_string(),
        to: Some("msg1-step2".to_string()),
        num_ancestors: None,
    };
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::RollbackOk { from, to } => {
            assert_eq!(from, "ws-test");
            assert_eq!(to, "msg1-step2");
        }
        _ => panic!("expected RollbackOk, got {:?}", response),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn full_rollback_preview_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::RollbackPreview {
        workspace: "/ws".to_string(),
        to: Some("msg1-step2".to_string()),
        num_ancestors: None,
    };
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::RollbackPreviewOk { to, changes } => {
            assert_eq!(to, "msg1-step2");
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].change_type, ChangeType::Modified);
        }
        _ => panic!("expected RollbackPreviewOk, got {:?}", response),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn frame_length_prefix_matches_payload() {
    // Verify the frame protocol: first 4 bytes = LE payload length
    let request = Request::Delete {
        workspace: Some("/ws".to_string()),
        snapshot: "msg1-step0".to_string(),
        force: true,
    };
    let frame = encode_frame(&request).unwrap();

    let declared_len = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
    let actual_payload = &frame[4..];
    assert_eq!(declared_len, actual_payload.len());

    // Verify the payload can be decoded back
    let decoded: Request = decode_payload(actual_payload).unwrap();
    match decoded {
        Request::Delete { force, .. } => assert!(force),
        _ => panic!("expected Delete"),
    }
}

// ── Phase 2 protocol integration tests ──

#[tokio::test]
async fn full_list_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::List {
        workspace: Some("/tmp/ws".to_string()),
        format: Some("json".to_string()),
    };
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::ListOk { snapshots } => {
            assert_eq!(snapshots.len(), 1);
            assert_eq!(snapshots[0].id, "abcdef1234567890abcdef1234567890abcdef12");
        }
        _ => panic!("expected ListOk, got {:?}", response),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn full_diff_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::Diff {
        workspace: "/tmp/ws".to_string(),
        from: "msg1-step0".to_string(),
        to: Some("msg2-step0".to_string()),
    };
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::DiffOk { changes } => {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].change_type, ChangeType::Modified);
        }
        _ => panic!("expected DiffOk, got {:?}", response),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn full_status_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::Status { workspace: None };
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::StatusOk { report } => {
            assert_eq!(report.uptime_secs, 42);
            assert_eq!(report.workspaces.len(), 1);
            assert_eq!(report.workspaces[0].ws_id, "ws-test");
        }
        _ => panic!("expected StatusOk, got {:?}", response),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn full_cleanup_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::Cleanup {
        workspace: "/tmp/ws".to_string(),
        keep: Some(10),
    };
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::CleanupOk { removed } => {
            assert_eq!(removed.len(), 1);
            assert_eq!(removed[0], "msg1-step0");
        }
        _ => panic!("expected CleanupOk, got {:?}", response),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn full_reload_config_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::ReloadConfig;
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::ReloadConfigOk { config, .. } => {
            assert_eq!(config.mount_path, "/mnt/btrfs-workspace");
            assert_eq!(config.auto_cleanup_keep, CleanupRetention::Count(20));
        }
        other => panic!("expected ReloadConfigOk, got {:?}", other),
    }

    server.await.unwrap();
}

#[tokio::test]
async fn full_config_request_response_over_socket() {
    let socket_path = temp_socket_path();

    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });

    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();

    let request = Request::Config;
    let frame = encode_frame(&request).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let response: Response = decode_payload(&payload).unwrap();
    match response {
        Response::ConfigOk { config } => {
            assert_eq!(config.auto_cleanup_keep, CleanupRetention::Count(20));
            assert_eq!(config.auto_cleanup_interval_secs, 86_400);
        }
        _ => panic!("expected ConfigOk, got {:?}", response),
    }

    server.await.unwrap();
}

// ── Per-workspace policy: real end-to-end IPC tests ──
//
// Each variant goes through encode → socket → decode on both ends, so a
// future bincode-tag shuffle, serde rename, or `EffectivePolicy` /
// `GlobalPolicySnapshot` field reorder will fail loudly here instead of
// silently breaking the wire format.

/// Shared helper: spawn mock server, encode `req`, return decoded `Response`.
/// Renamed from `run_policy_request` — also used by the reload-IPC e2e tests
/// below that aren't policy-shaped.
async fn run_policy_request(req: Request) -> Response {
    let socket_path = temp_socket_path();
    let listener = UnixListener::bind(&socket_path).expect("bind");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        mock_server_handle(stream).await;
    });
    tokio::task::yield_now().await;

    let mut client = UnixStream::connect(&socket_path).await.unwrap();
    let frame = encode_frame(&req).unwrap();
    client.write_all(&frame).await.unwrap();

    let mut len_buf = [0u8; 4];
    client.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    client.read_exact(&mut payload).await.unwrap();

    let resp: Response = decode_payload(&payload).unwrap();
    server.await.unwrap();
    resp
}

#[tokio::test]
async fn full_get_workspace_policy_over_socket() {
    let resp = run_policy_request(Request::GetWorkspacePolicy {
        workspace: "/tmp/ws".to_string(),
    })
    .await;
    match resp {
        Response::WorkspacePolicyOk {
            ws_id,
            effective,
            local,
            global,
        } => {
            // The mock server prefixes "get:" so we know the request reached
            // it as the right variant (and not, say, as Set after a tag drift).
            assert_eq!(ws_id, "get:/tmp/ws");
            assert_eq!(effective.auto_cleanup_keep, CleanupRetention::Count(5));
            assert!(effective.auto_cleanup);
            assert!(local.is_empty());
            assert_eq!(global.auto_cleanup_keep, CleanupRetention::Count(20));
        }
        _ => panic!("expected WorkspacePolicyOk, got {:?}", resp),
    }
}

// (full_set_workspace_policy_over_socket removed: there is no longer a
// "write whole policy" IPC — Patch handles edits, Reset handles removal.)

#[tokio::test]
async fn full_reset_workspace_policy_over_socket() {
    // ResetWorkspacePolicy is the only whole-file IPC. Mock+daemon must
    // both behave the same way: file gone, local empty, effective ==
    // global. Distinct `reset:` ws_id prefix from the mock so a future
    // tag-shuffle that delivered the wrong variant fails here.
    let resp = run_policy_request(Request::ResetWorkspacePolicy {
        workspace: "ws-reset".to_string(),
    })
    .await;
    match resp {
        Response::WorkspacePolicyOk {
            ws_id,
            local,
            effective,
            global,
        } => {
            assert_eq!(ws_id, "reset:ws-reset");
            assert!(local.is_empty(), "reset must produce an empty local");
            assert_eq!(
                effective.auto_cleanup, global.auto_cleanup,
                "with local empty, effective.auto_cleanup must match global"
            );
            assert_eq!(
                effective.auto_cleanup_keep, global.auto_cleanup_keep,
                "with local empty, effective.auto_cleanup_keep must match global"
            );
        }
        _ => panic!("expected WorkspacePolicyOk, got {:?}", resp),
    }
}

#[tokio::test]
async fn full_patch_workspace_policy_over_socket() {
    // Cover both PolicyFieldOp variants (Set + Unchanged) on a single
    // request so each one touches the wire.
    let resp = run_policy_request(Request::PatchWorkspacePolicy {
        workspace: "ws-patch".to_string(),
        auto_cleanup: PolicyFieldOp::Set(true),
        auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(7)),
    })
    .await;
    match resp {
        Response::WorkspacePolicyOk {
            ws_id,
            local,
            effective,
            ..
        } => {
            assert_eq!(ws_id, "patch:ws-patch");
            assert_eq!(local.auto_cleanup, Some(true));
            assert_eq!(local.auto_cleanup_keep, Some(CleanupRetention::Count(7)));
            assert!(effective.auto_cleanup);
            assert_eq!(effective.auto_cleanup_keep, CleanupRetention::Count(7));
        }
        _ => panic!("expected WorkspacePolicyOk, got {:?}", resp),
    }
}

#[tokio::test]
async fn full_patch_workspace_policy_unchanged_only_over_socket() {
    // All-Unchanged Patch: nothing should be applied. Used here mainly to
    // exercise the `Unchanged` discriminant on the wire (the Set test
    // above only ever encodes `Set` for both fields).
    let resp = run_policy_request(Request::PatchWorkspacePolicy {
        workspace: "ws-patch-noop".to_string(),
        auto_cleanup: PolicyFieldOp::Unchanged,
        auto_cleanup_keep: PolicyFieldOp::Unchanged,
    })
    .await;
    match resp {
        Response::WorkspacePolicyOk { local, .. } => {
            // Unchanged applied to None stays None — both fields remain unset.
            assert_eq!(local.auto_cleanup, None);
            assert_eq!(local.auto_cleanup_keep, None);
        }
        _ => panic!("expected WorkspacePolicyOk, got {:?}", resp),
    }
}

// ── Reload IPCs: exercise every variant on the wire ──
//
// All three reload variants share the `ReloadConfigOk { config, file }` reply
// shape. Mock server returns the same body for all three (combined match
// arm), so these tests verify the *request* tags round-trip distinctly
// — a bincode tag shuffle that swapped `ReloadConfig` and
// `ReloadGlobalConfig` would still go through the same arm, but a
// reorder against any other Request variant (Init/Recover/etc.) would
// blow up at decode.

#[tokio::test]
async fn full_reload_config_over_socket() {
    let resp = run_policy_request(Request::ReloadConfig).await;
    match resp {
        Response::ReloadConfigOk { config, .. } => {
            assert_eq!(config.mount_path, "/mnt/btrfs-workspace");
            assert_eq!(config.auto_cleanup_keep, CleanupRetention::Count(20));
        }
        other => panic!("expected ReloadConfigOk, got {:?}", other),
    }
}

#[tokio::test]
async fn full_reload_global_config_over_socket() {
    let resp = run_policy_request(Request::ReloadGlobalConfig).await;
    match resp {
        Response::ReloadConfigOk { config, .. } => {
            assert_eq!(config.mount_path, "/mnt/btrfs-workspace");
        }
        other => panic!("expected ReloadConfigOk, got {:?}", other),
    }
}

#[tokio::test]
async fn full_reload_workspace_policy_over_socket() {
    let resp = run_policy_request(Request::ReloadWorkspacePolicy {
        workspace: "/tmp/ws".to_string(),
    })
    .await;
    match resp {
        Response::ReloadConfigOk { config, .. } => {
            // Mock reuses the same global config payload for the per-ws
            // reload reply; the point of the test is the request side.
            assert_eq!(config.mount_path, "/mnt/btrfs-workspace");
        }
        other => panic!("expected ReloadConfigOk, got {:?}", other),
    }
}

#[tokio::test]
async fn full_guarded_checkpoint_v2_over_socket() {
    let generation = WorkspaceGenerationTokenV2::from_bytes([7; 32]);
    let response = run_policy_request(Request::GuardedCheckpointV2 {
        ws_id: "ws-abcdef".to_string(),
        expected_generation: generation,
        checkpoint_id: "checkpoint-1".to_string(),
        operation_digest: [9; 32],
        message: Some("guarded".to_string()),
        metadata: Some(r#"{"source":"test"}"#.to_string()),
        pin: true,
    })
    .await;

    match response {
        Response::GuardedCheckpointV2Ok { evidence } => {
            assert_eq!(evidence.ws_id, "ws-abcdef");
            assert_eq!(evidence.generation, generation);
            assert_eq!(evidence.operation_digest, [9; 32]);
            assert_eq!(evidence.caller_uid, 1000);
            assert_eq!(
                evidence.outcome,
                GuardedCheckpointOutcomeV2::Created {
                    snapshot_id: "checkpoint-1".to_string()
                }
            );
        }
        other => panic!("expected GuardedCheckpointV2Ok, got {other:?}"),
    }
}

#[tokio::test]
async fn full_checkpoint_evidence_v2_over_socket() {
    let generation = WorkspaceGenerationTokenV2::from_bytes([8; 32]);
    let response = run_policy_request(Request::CheckpointEvidenceV2 {
        ws_id: "ws-abcdef".to_string(),
        expected_generation: generation,
        checkpoint_id: "checkpoint-2".to_string(),
        operation_digest: [10; 32],
    })
    .await;

    match response {
        Response::CheckpointEvidenceV2Ok {
            evidence: Some(evidence),
        } => {
            assert_eq!(evidence.generation, generation);
            assert_eq!(evidence.operation_digest, [10; 32]);
            assert_eq!(
                evidence.outcome,
                GuardedCheckpointOutcomeV2::Skipped {
                    reason: "empty workspace".to_string()
                }
            );
        }
        other => panic!("expected CheckpointEvidenceV2Ok, got {other:?}"),
    }
}
