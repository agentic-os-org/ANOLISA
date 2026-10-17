use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::state::DaemonState;
use ws_ckpt_common::{
    decode_payload, encode_frame, ErrorCode, Request, Response, WsCkptError, MAX_FRAME_SIZE,
};

pub async fn run_listener(
    state: Arc<DaemonState>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // 1. Clean up residual socket file
    let _ = std::fs::remove_file(&state.socket_path);

    // 2. Ensure socket parent directory exists
    if let Some(parent) = state.socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("Failed to create socket parent directory")?;
    }

    // 3. Bind the Unix listener
    let listener = UnixListener::bind(&state.socket_path).context("Failed to bind Unix socket")?;
    info!("Listening on {:?}", state.socket_path);

    // 4. Set socket permissions to 0o666
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state.socket_path, std::fs::Permissions::from_mode(0o666))
        .context("Failed to set socket permissions")?;

    // 5. Accept loop
    let mut join_set = JoinSet::new();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let state = Arc::clone(&state);
                        join_set.spawn(async move {
                            if let Err(e) = handle_connection(stream, state).await {
                                error!("Connection error: {:#}", e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }
            _ = cancel.cancelled() => {
                info!("Listener received cancellation signal");
                break;
            }
        }

        // Reap finished connections. JoinSet holds each task's handle and output
        // slot until it is joined, so without this the set grows by one entry per
        // CLI invocation and is only released at shutdown. try_join_next never
        // awaits, so reaping cannot delay the next accept; a join_next branch in
        // the select! above would instead need a second mutable borrow of
        // join_set while the accept arm still spawns into it.
        while join_set.try_join_next().is_some() {}
    }

    // 7. Wait for in-flight tasks to complete (with timeout)
    info!("Waiting for in-flight connections to complete...");
    let drain = async { while join_set.join_next().await.is_some() {} };
    if tokio::time::timeout(Duration::from_secs(10), drain)
        .await
        .is_err()
    {
        error!("Timed out waiting for in-flight connections; aborting remaining tasks");
        join_set.abort_all();
    }

    // Clean up socket file
    let _ = std::fs::remove_file(&state.socket_path);
    info!("Listener shut down");
    Ok(())
}

async fn handle_connection(
    mut stream: tokio::net::UnixStream,
    state: Arc<DaemonState>,
) -> anyhow::Result<()> {
    // Read 4-byte LE length
    let len = stream
        .read_u32_le()
        .await
        .context("Failed to read frame length")?;

    // Validate frame size
    if len > MAX_FRAME_SIZE {
        let err_resp = Response::Error {
            code: ErrorCode::InternalError,
            message: format!("Frame too large: {} bytes (max {})", len, MAX_FRAME_SIZE),
        };
        let frame = encode_frame(&err_resp)?;
        stream.write_all(&frame).await?;
        anyhow::bail!("Frame too large: {} bytes", len);
    }

    // Read payload
    let mut payload = vec![0u8; len as usize];
    stream
        .read_exact(&mut payload)
        .await
        .context("Failed to read frame payload")?;

    // Decode request. An undecodable payload is a protocol error, not a
    // reason to drop the socket silently: the frame-size guard above replies
    // with a structured Response::Error, and this path must too — otherwise
    // a version-skewed client (newer Request variant, older daemon) only
    // ever sees an unexpected EOF on its response read, with no hint that
    // the two binaries disagree.
    let request: Request = match decode_payload(&payload) {
        Ok(request) => request,
        Err(error) => {
            let err_resp = Response::Error {
                code: ErrorCode::InternalError,
                message: format!(
                    "failed to decode request frame: {error}; the client and daemon \
                     may be different ws-ckpt versions — upgrade both to the same \
                     version"
                ),
            };
            // Best-effort: the client may already be gone; a failed write
            // only means the error frame could not be delivered.
            if let Ok(frame) = encode_frame(&err_resp) {
                let _ = stream.write_all(&frame).await;
            }
            return Err(error).context("failed to decode request frame");
        }
    };

    let peer_cred = stream.peer_cred().ok();
    let agent_name = peer_cred
        .as_ref()
        .and_then(|cred| cred.pid().map(|p| p as u32))
        .map(crate::ops_log::detect_agent_name)
        .unwrap_or_else(|| "user".to_string());
    let ops_name = crate::ops_log::ops_name_from_request(&request);

    // Dispatch
    let context = crate::dispatcher::DispatchContext::new(peer_cred.map(|cred| cred.uid()));
    let mut response = crate::dispatcher::dispatch_with_context(&state, request, context).await;

    let frame = match encode_frame(&response) {
        Err(err @ WsCkptError::FrameTooLarge { .. }) => {
            let advice = if matches!(response, Response::ListOk { .. }) {
                "; use list --limit <N> and the returned cursor; use status for snapshot counts"
            } else {
                ""
            };
            response = Response::Error {
                code: ErrorCode::InternalError,
                message: format!("{err}{advice}"),
            };
            encode_frame(&response)?
        }
        result => result.context("Failed to encode response")?,
    };

    if let Some(name) = ops_name {
        crate::ops_log::log_operation(name, &agent_name, &response);
    }

    // Encode and write response
    stream
        .write_all(&frame)
        .await
        .context("Failed to write response")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ws_ckpt_common::{DaemonConfig, SnapshotIndex, SnapshotMeta};

    async fn exchange(state: &Arc<DaemonState>, request: Request) -> Response {
        // Exercise the real connection handler without a daemon or background task.
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let client_io = async {
            client
                .write_all(&encode_frame(&request).unwrap())
                .await
                .unwrap();
            let len = client.read_u32_le().await.unwrap();
            assert!(len <= MAX_FRAME_SIZE);
            let mut payload = vec![0; len as usize];
            client.read_exact(&mut payload).await.unwrap();
            decode_payload(&payload).unwrap()
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            let (server, response) =
                tokio::join!(handle_connection(server, state.clone()), client_io);
            server.unwrap();
            response
        })
        .await
        .expect("IPC round trip timed out")
    }

    /// Drive the real handle_connection with raw client bytes (not
    /// encode_frame(&Request)) and capture whatever response frame comes
    /// back — None when the server closes without replying.
    async fn raw_exchange(
        state: &Arc<DaemonState>,
        bytes: &[u8],
    ) -> Option<Result<Response, anyhow::Error>> {
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let client_io = async {
            client.write_all(bytes).await.unwrap();
            // Try to read a response frame; EOF maps to None.
            match client.read_u32_le().await {
                Ok(len) => {
                    let mut payload = vec![0u8; len as usize];
                    client.read_exact(&mut payload).await.unwrap();
                    Some(Ok(decode_payload(&payload).unwrap()))
                }
                Err(_) => None,
            }
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            let (server, response) =
                tokio::join!(handle_connection(server, state.clone()), client_io);
            // The server's Err is expected on malformed frames; surface the
            // client's view alongside it.
            let _ = server;
            response
        })
        .await
        .expect("IPC round trip timed out")
    }

    fn frame_state() -> Arc<DaemonState> {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            temp.path().join("data"),
            temp.path().join("test.img"),
        ));
        let state_dir = temp.path().join("state");
        // Leak the tempdir: DaemonState only needs the paths to exist for
        // this test's lifetime and the state is read-only here.
        let _keep = temp.keep();
        Arc::new(DaemonState::new(
            DaemonConfig::default(),
            backend,
            state_dir,
        ))
    }

    fn prefixed(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[tokio::test]
    async fn malformed_payload_gets_structured_error_reply() {
        // A payload that fails bincode decoding must produce a structured
        // Response::Error frame, not a silent close — a version-skewed
        // client otherwise only sees an unexpected EOF.
        let state = frame_state();
        // 0xFF is not a valid Request variant tag.
        let response =
            raw_exchange(&state, &prefixed(&[0xFF, 0xFF, 0xFF, 0xFF])).await;
        let response = response.expect("a response frame must be written");
        let response = response.expect("response decodes");
        match response {
            Response::Error { message, .. } => {
                assert!(
                    message.contains("failed to decode request frame"),
                    "message should name the decode failure: {message}"
                );
                assert!(
                    message.contains("versions"),
                    "message should hint at version skew: {message}"
                );
            }
            other => panic!("expected Response::Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn zero_length_frame_gets_structured_error_reply() {
        // len == 0: read_exact succeeds on an empty buffer, decode of &[]
        // fails — must also reply, not close.
        let state = frame_state();
        let response = raw_exchange(&state, &prefixed(&[])).await;
        let response = response.expect("a response frame must be written");
        assert!(matches!(
            response.expect("decodes"),
            Response::Error { .. }
        ));
    }

    #[tokio::test]
    async fn oversized_frame_gets_structured_error_reply() {
        // The pre-existing frame-size guard: pin that it also replies with a
        // structured error (previously untested).
        let state = frame_state();
        let mut bytes = (MAX_FRAME_SIZE as u32 + 1).to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0u8; 8]);
        let response = raw_exchange(&state, &bytes).await;
        let response = response.expect("a response frame must be written");
        match response.expect("decodes") {
            Response::Error { message, .. } => {
                assert!(message.contains("Frame too large"), "message: {message}")
            }
            other => panic!("expected Response::Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn truncated_frame_closes_without_reply() {
        // A client that disconnects mid-payload: the server reads EOF on
        // read_exact and closes with no reply. The client half-closes its
        // side after the partial write so the server's read_exact sees EOF
        // (keeping the socket open would block the server forever).
        let state = frame_state();
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let client_io = async {
            let mut bytes = (64u32).to_le_bytes().to_vec();
            bytes.extend_from_slice(&[0u8; 4]); // only 4 of 64 payload bytes
            client.write_all(&bytes).await.unwrap();
            client.shutdown().await.unwrap();
            // The server must not write a response frame: read to EOF.
            match client.read_u32_le().await {
                Ok(_) => panic!("mid-frame disconnect must not produce a reply"),
                Err(_) => None::<Result<Response, anyhow::Error>>,
            }
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            let (server, response) =
                tokio::join!(handle_connection(server, state.clone()), client_io);
            // The server errors on the truncated read; the client saw EOF.
            assert!(server.is_err(), "server must fail on a truncated frame");
            response
        })
        .await
        .expect("IPC round trip timed out");
    }

    #[tokio::test]
    async fn list_above_frame_limit_remains_manageable() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            temp.path().join("data"),
            temp.path().join("test.img"),
        ));
        let state = Arc::new(DaemonState::new(
            DaemonConfig::default(),
            backend,
            temp.path().join("state"),
        ));
        let created_at = chrono::Utc::now();
        for (ws_id, count) in [("ws-a", 100_000), ("ws-b", 2)] {
            let subvol = state.backend.data_root().join(ws_id);
            std::fs::create_dir_all(&subvol).unwrap();
            let path = temp.path().join(ws_id);
            std::os::unix::fs::symlink(subvol, &path).unwrap();
            let mut index = SnapshotIndex::new(path.clone());
            for i in (0..count).rev() {
                index.snapshots.insert(
                    format!("s-{i:06}"),
                    SnapshotMeta {
                        message: Some("x".repeat(128)),
                        metadata: None,
                        pinned: false,
                        created_at,
                        missing: false,
                        parent_id: None,
                        child_ids: vec![],
                    },
                );
            }
            state.register_workspace(ws_id.into(), path, index).unwrap();
        }

        for workspace in [Some("ws-a".to_string()), None] {
            let response = exchange(
                &state,
                Request::List {
                    workspace: workspace.clone(),
                    format: None,
                },
            )
            .await;
            assert!(matches!(response, Response::Error { message, .. }
                if message.contains("frame too large") && message.contains("--limit")));

            let first = exchange(
                &state,
                Request::ListPage {
                    orphans_only: false,
                    workspace: workspace.clone(),
                    limit: 2,
                    cursor: None,
                },
            )
            .await;
            let Response::ListPageOk {
                snapshots,
                next_cursor,
            } = first
            else {
                panic!("expected first page")
            };
            let mut ids = snapshots
                .into_iter()
                .map(|entry| match entry {
                    ws_ckpt_common::SnapshotListItem::Full(entry) => entry.id,
                    ws_ckpt_common::SnapshotListItem::Summary(summary) => summary.id,
                })
                .collect::<Vec<_>>();
            let second = exchange(
                &state,
                Request::ListPage {
                    orphans_only: false,
                    workspace: workspace.clone(),
                    limit: 2,
                    cursor: next_cursor,
                },
            )
            .await;
            let Response::ListPageOk { snapshots, .. } = second else {
                panic!("expected second page")
            };
            ids.extend(snapshots.into_iter().map(|entry| match entry {
                ws_ckpt_common::SnapshotListItem::Full(entry) => entry.id,
                ws_ckpt_common::SnapshotListItem::Summary(summary) => summary.id,
            }));
            assert_eq!(ids, ["s-000000", "s-000001", "s-000002", "s-000003"]);

            let response = exchange(
                &state,
                Request::ListPage {
                    orphans_only: false,
                    workspace: workspace.clone(),
                    limit: 2,
                    cursor: Some("not-a-cursor".into()),
                },
            )
            .await;
            assert!(matches!(response, Response::Error { message, .. }
                if message.contains("cursor")));
            let response = exchange(
                &state,
                Request::ListPage {
                    orphans_only: false,
                    workspace,
                    limit: 0,
                    cursor: None,
                },
            )
            .await;
            assert!(
                matches!(response, Response::Error { message, .. } if message.contains("between 1"))
            );
        }

        let first = exchange(
            &state,
            Request::ListPage {
                orphans_only: false,
                workspace: Some("ws-b".into()),
                limit: 1,
                cursor: None,
            },
        )
        .await;
        let Response::ListPageOk {
            next_cursor: Some(ws_b_cursor),
            ..
        } = first
        else {
            panic!("expected ws-b continuation cursor")
        };
        let scope_error = exchange(
            &state,
            Request::ListPage {
                orphans_only: false,
                workspace: Some("ws-a".into()),
                limit: 1,
                cursor: Some(ws_b_cursor.clone()),
            },
        )
        .await;
        assert!(matches!(scope_error, Response::Error { message, .. }
            if message.contains("does not match")));

        {
            let arc = state.get_by_wsid("ws-b").unwrap();
            let mut guard = arc.write().await;
            guard.index.snapshots.remove("s-000001");
            guard.index.snapshots.insert(
                "s-000000a".into(),
                SnapshotMeta {
                    message: Some("inserted within first-page bound".into()),
                    metadata: None,
                    pinned: false,
                    created_at,
                    missing: false,
                    parent_id: None,
                    child_ids: vec![],
                },
            );
            guard.index.snapshots.insert(
                "s-newer".into(),
                SnapshotMeta {
                    message: Some("newer than first-page bound".into()),
                    metadata: None,
                    pinned: false,
                    created_at: created_at + chrono::Duration::seconds(1),
                    missing: false,
                    parent_id: None,
                    child_ids: vec![],
                },
            );
        }
        let second = exchange(
            &state,
            Request::ListPage {
                orphans_only: false,
                workspace: Some("ws-b".into()),
                limit: 10,
                cursor: Some(ws_b_cursor),
            },
        )
        .await;
        let Response::ListPageOk {
            snapshots,
            next_cursor,
        } = second
        else {
            panic!("expected ws-b continuation page")
        };
        assert!(next_cursor.is_none());
        assert!(matches!(snapshots.as_slice(),
            [ws_ckpt_common::SnapshotListItem::Full(entry)] if entry.id == "s-000000a"));

        let response = exchange(
            &state,
            Request::Status {
                workspace: Some("ws-a".into()),
            },
        )
        .await;
        assert!(
            matches!(response, Response::StatusOk { report } if report.workspaces[0].snapshot_count == 100_000)
        );
        let response = exchange(
            &state,
            Request::List {
                workspace: Some("ws-b".into()),
                format: None,
            },
        )
        .await;
        assert!(matches!(response, Response::ListOk { snapshots } if snapshots.len() == 3));
        let response = exchange(
            &state,
            Request::ListPage {
                orphans_only: false,
                workspace: Some("unknown".into()),
                limit: 2,
                cursor: None,
            },
        )
        .await;
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::WorkspaceNotFound,
                ..
            }
        ));

        let arc = state.get_by_wsid("ws-a").unwrap();
        arc.write()
            .await
            .index
            .snapshots
            .get_mut("s-000000")
            .unwrap()
            .message = Some("x".repeat(MAX_FRAME_SIZE as usize));
        let response = exchange(
            &state,
            Request::ListPage {
                orphans_only: false,
                workspace: Some("ws-a".into()),
                limit: 1,
                cursor: None,
            },
        )
        .await;
        let Response::ListPageOk { snapshots, .. } = response else {
            panic!("expected summary page")
        };
        assert_eq!(snapshots.len(), 1);
        let ws_ckpt_common::SnapshotListItem::Summary(summary) = &snapshots[0] else {
            panic!("expected oversized entry summary, got {:?}", snapshots[0])
        };
        assert_eq!(summary.id, "s-000000");
        assert_eq!(
            summary.omitted_fields,
            ["message", "metadata", "parent_id", "child_ids"]
        );
        std::fs::remove_file(temp.path().join("ws-a")).unwrap();
        let response = exchange(
            &state,
            Request::ListPage {
                orphans_only: false,
                workspace: Some("ws-a".into()),
                limit: 1,
                cursor: None,
            },
        )
        .await;
        assert!(matches!(response, Response::Error { message, .. } if message.contains("recover")));
    }
}
