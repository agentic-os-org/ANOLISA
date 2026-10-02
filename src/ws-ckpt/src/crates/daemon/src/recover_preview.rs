//! Resolve recovery confirmation in the daemon and bind it to the deletion scope.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use sha2::{Digest, Sha256};
use ws_ckpt_common::{ErrorCode, RecoveryPreview, Response};

use crate::state::{DaemonState, WorkspaceState};

pub(crate) async fn preview(state: &Arc<DaemonState>, workspace: &str) -> anyhow::Result<Response> {
    let _init_guard = state.init_lock.lock().await;
    if let Some(arc) = state.resolve_workspace(workspace).await {
        let Some((_, _mutation_guard)) = state.lock_workspace_mutation_if_current(&arc).await
        else {
            return Ok(not_found(workspace));
        };
        let ws = arc.read().await;
        return Ok(Response::RecoverPreviewOk {
            preview: registered_preview(state, &ws).await?,
        });
    }
    if let Some(original) = crate::workspace_mgr::orphan_workspace_path(workspace).await? {
        if state.registration_path_is_internal(&original)? || original == Path::new("/") {
            return Ok(Response::Error {
                code: ErrorCode::InvalidPath,
                message: "cannot restore an orphan inside managed storage".into(),
            });
        }
        return Ok(Response::RecoverPreviewOk {
            preview: orphan_preview(state, &original).await?,
        });
    }
    Ok(not_found(workspace))
}

fn not_found(workspace: &str) -> Response {
    Response::Error {
        code: ErrorCode::WorkspaceNotFound,
        message: format!("workspace not found: {workspace}"),
    }
}

// Length-prefix components so paths and snapshot IDs cannot collide at boundaries.
fn digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

async fn digest_entry(digest: &mut Sha256, path: &Path) -> anyhow::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => {
            digest.update([1]);
            digest.update(meta.dev().to_le_bytes());
            digest.update(meta.ino().to_le_bytes());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => digest.update([0]),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect recovery target {path:?}"))
        }
    }
    Ok(())
}

// Caller holds the workspace mutation lock; detached registrations remain recoverable.
pub(crate) async fn registered_preview(
    state: &Arc<DaemonState>,
    ws: &WorkspaceState,
) -> anyhow::Result<RecoveryPreview> {
    let registration_path = ws
        .path
        .to_str()
        .context("workspace path is not valid UTF-8")?
        .to_string();
    let mut digest = Sha256::new();
    digest_field(&mut digest, b"registered-recovery");
    digest_field(&mut digest, ws.ws_id.as_bytes());
    digest_field(&mut digest, registration_path.as_bytes());
    let mut snapshots: Vec<_> = ws.index.snapshots.iter().collect();
    snapshots.sort_unstable_by_key(|(id, _)| *id);
    for (id, snapshot) in snapshots {
        digest_field(&mut digest, id.as_bytes());
        digest_field(&mut digest, snapshot.created_at.to_rfc3339().as_bytes());
    }
    // Match backend teardown, which also removes snapshots absent from index.json.
    let mut physical_snapshots = crate::backends::btrfs_common::recovery_snapshot_paths(
        &state.backend.snapshots_root().join(&ws.ws_id),
    )
    .await?;
    physical_snapshots.sort_unstable();
    for path in &physical_snapshots {
        digest_field(&mut digest, path.as_os_str().as_bytes());
        digest_entry(&mut digest, path).await?;
    }
    digest_entry(&mut digest, &state.backend.data_root().join(&ws.ws_id)).await?;
    Ok(RecoveryPreview {
        ws_id: Some(ws.ws_id.clone()),
        registration_path,
        snapshot_count: u32::try_from(physical_snapshots.len()).context("too many snapshots")?,
        confirmation_digest: digest.finalize().into(),
    })
}

// Orphan recovery retains migrated storage and snapshots; only the backup is restored.
pub(crate) async fn orphan_preview(
    _state: &Arc<DaemonState>,
    original: &Path,
) -> anyhow::Result<RecoveryPreview> {
    let registration_path = original
        .to_str()
        .context("workspace path is not valid UTF-8")?
        .to_string();
    let backup = crate::backends::btrfs_common::backup_path_for(&registration_path);
    let mut digest = Sha256::new();
    digest_field(&mut digest, b"orphan-recovery");
    digest_field(&mut digest, registration_path.as_bytes());
    digest_entry(&mut digest, Path::new(&backup)).await?;
    Ok(RecoveryPreview {
        ws_id: None,
        registration_path,
        snapshot_count: 0,
        confirmation_digest: digest.finalize().into(),
    })
}
