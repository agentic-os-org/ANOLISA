use std::sync::Arc;

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tracing::{error, info, warn};

use ws_ckpt_common::{
    load_workspace_policy_with_failsafe, ErrorCode, ResolveError, Response, SnapshotIndex,
};

use crate::index_store;
use crate::state::DaemonState;

// ── helpers ──

fn error_resp(code: ErrorCode, msg: impl Into<String>) -> Response {
    Response::Error {
        code,
        message: msg.into(),
    }
}

/// Strip trailing slashes, preserving root "/". Empty stays empty.
fn strip_trailing_slashes(s: &str) -> &str {
    if s.is_empty() {
        return s;
    }
    let trimmed = s.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// Re-adopt an existing managed subvolume into the daemon state and return
/// `InitOk { ws_id }`. Used when a workspace is discovered out-of-band
/// (e.g. after daemon restart with on-disk subvol intact) — either through
/// a user-facing symlink (Step 0) or through canonical resolution into
/// mount_path (Step 2b).
///
/// Loads the index from disk if present; falls back to rebuilding it from
/// the snapshots directory; persists the rebuilt index. Save_manifest
/// failure is warned but not fatal — the in-memory registration succeeded
/// and subsequent writes will retry persistence.
async fn adopt_existing_subvol(
    state: &Arc<DaemonState>,
    ws_id: &str,
    registered_path: std::path::PathBuf,
) -> Response {
    // Same-ws_id lifecycle lock as init/recover. ws_id here came from the
    // existing on-disk subvol name, not SHA256(path), but it shares the
    // index_dir(ws_id) namespace either way.
    let _wsid_guard = state.lock_wsid(ws_id).await;
    let snap_dir = state.index_dir(ws_id);
    let btrfs_snap_dir = state.backend.snapshots_root().join(ws_id);
    let mut index = if let Ok(idx) = index_store::load(&snap_dir).await {
        idx
    } else {
        SnapshotIndex::new(registered_path.clone())
    };
    if index.snapshots.is_empty() {
        if let Ok(mut rebuilt) =
            index_store::rebuild_from_fs(&btrfs_snap_dir, registered_path.clone()).await
        {
            if !rebuilt.snapshots.is_empty() {
                // Re-adoption may recover orphan snapshot metadata, but it
                // must not discard guarded receipts loaded from index.json.
                rebuilt.governed_evidence = index.governed_evidence.clone();
                rebuilt.guarded_rollbacks = index.guarded_rollbacks.clone();
                info!(
                    "Recovered {} snapshot(s) from filesystem for {}",
                    rebuilt.snapshots.len(),
                    ws_id
                );
                index = rebuilt;
                let _ = index_store::save(&snap_dir, &index).await;
            }
        }
    }
    // Re-adoption must honor any pre-existing per-ws policy.toml; shared
    // fail-safe helper means missing→inherit, on read error→auto_cleanup=false
    // + policy_failsafe=true (won't delete protected snapshots before next
    // reload, PATCH refused until reload/reset). See [[ws-failsafe]].
    let (policy, failsafe) = load_workspace_policy_with_failsafe(&snap_dir, ws_id, "re-adoption");
    state.register_workspace_with_policy(
        ws_id.to_string(),
        registered_path,
        index,
        policy,
        failsafe,
    );
    if let Err(e) = state.save_manifest().await {
        warn!("save_manifest failed after subvol re-adoption: {:#}", e);
    }
    Response::InitOk {
        ws_id: ws_id.to_string(),
    }
}

// ── init ──

pub async fn init(state: &Arc<DaemonState>, workspace: &str) -> anyhow::Result<Response> {
    if workspace.trim().is_empty() {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            "workspace path is empty",
        ));
    }
    let workspace = strip_trailing_slashes(workspace);
    // 0. Early check: detect workspace already managed via symlink to our data_root.
    //    This must run before canonicalize(), which would resolve the symlink
    //    and cause the "inside mount_path" guard to reject it.
    let ws_path = std::path::PathBuf::from(workspace);
    if let Ok(meta) = tokio::fs::symlink_metadata(&ws_path).await {
        if meta.file_type().is_symlink() {
            if let Ok(target) = tokio::fs::read_link(&ws_path).await {
                let data_root = state.backend.data_root();
                if target.starts_with(data_root) {
                    if tokio::fs::metadata(&target).await.is_ok() {
                        // Valid symlink pointing to our managed subvolume
                        let ws_id = target
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();

                        if state.get_by_wsid(&ws_id).is_some() {
                            // Already registered — idempotent success
                            info!(
                                "workspace already initialized: {} -> {:?} (ws_id={})",
                                workspace, target, ws_id
                            );
                            return Ok(Response::InitOk { ws_id });
                        }

                        // Subvolume exists but daemon lost track (e.g. restart).
                        // Re-register (recovery mode).
                        info!(
                            "recovering unregistered workspace: {} -> {:?} (ws_id={})",
                            workspace, target, ws_id
                        );
                        return Ok(adopt_existing_subvol(state, &ws_id, ws_path.clone()).await);
                    } else {
                        // Broken symlink — target subvolume gone; remove and re-init
                        warn!(
                            "workspace symlink target missing: {:?}; re-initializing",
                            target
                        );
                        let _ = tokio::fs::remove_file(&ws_path).await;
                    }
                }
            }
        }
    }

    // 1. Canonicalize (resolves symlinks to real path)
    let abs_path = match tokio::fs::canonicalize(workspace).await {
        Ok(p) => p,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                // Resolution happens in the daemon's mount namespace, which in a
                // sidecar deployment shows only the volumes it shares with the
                // client — so a path the caller can see may still be absent here.
                return Ok(error_resp(
                    ErrorCode::InvalidPath,
                    format!(
                        "path does not exist in the daemon's mount namespace: {}. \
                         In a sidecar deployment the workspace must live on a volume \
                         shared with the daemon container.",
                        workspace
                    ),
                ));
            }
            // A different failure (symlink loop, permission, I/O) means the path
            // exists but cannot be resolved — report the real cause instead of
            // pointing at the sidecar shared-volume layout.
            return Ok(error_resp(
                ErrorCode::InvalidPath,
                format!("cannot resolve workspace path {}: {}", workspace, e),
            ));
        }
    };
    // Reject non-UTF-8: lossy survives in manifest and breaks fs ops after daemon restart.
    if abs_path.to_str().is_none() {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            format!("resolved path is not valid UTF-8: {}", abs_path.display()),
        ));
    }
    if abs_path.to_string_lossy() != workspace {
        info!(
            "workspace path resolved: {} -> {}",
            workspace,
            abs_path.display()
        );
    }

    // Refuse '/' as workspace: rsync would be self-referential and pull in
    // /proc, /sys, etc.; recover() would overwrite the root filesystem.
    if abs_path == std::path::Path::new("/") {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            "root '/' is not a supported workspace; use a specific subdirectory",
        ));
    }

    // 2. Pre-checks
    let meta = match tokio::fs::metadata(&abs_path).await {
        Ok(m) => m,
        Err(_) => {
            return Ok(error_resp(
                ErrorCode::InvalidPath,
                format!("cannot stat path: {}", abs_path.display()),
            ));
        }
    };
    if !meta.is_dir() {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            format!("not a directory: {}", abs_path.display()),
        ));
    }
    if let Some(existing) = state.get_by_path(&abs_path) {
        let ws = existing.read().await;
        let expected_target = state.backend.data_root().join(&ws.ws_id);
        let read_link_res = tokio::fs::read_link(&abs_path).await;
        let symlink_ok = matches!(&read_link_res, Ok(t) if *t == expected_target);
        if symlink_ok {
            info!(
                "workspace already initialized via path: {} (ws_id={})",
                abs_path.display(),
                ws.ws_id
            );
            return Ok(Response::InitOk {
                ws_id: ws.ws_id.clone(),
            });
        }
        let hint = if read_link_res.is_err() {
            "\n  note: path is currently a regular directory — \
             move or rename it before running recover to avoid data loss"
        } else {
            ""
        };
        warn!(
            "workspace {} registered as {} but symlink missing or incorrect; \
             run 'ws-ckpt recover -w {}' to repair",
            abs_path.display(),
            ws.ws_id,
            abs_path.display()
        );
        return Ok(error_resp(
            ErrorCode::InternalError,
            format!(
                "workspace registered (ws_id={}) but symlink missing or broken; \
                 run 'ws-ckpt recover -w {}' to restore, then re-init{}",
                ws.ws_id,
                abs_path.display(),
                hint,
            ),
        ));
    }
    if abs_path.starts_with(&state.mount_path) {
        // The user-facing path canonicalises into our mount root. Two
        // sub-cases need different handling:
        //   (a) `abs_path == mount_path/<ws_id>` for some `ws_id` we
        //       manage. The user is effectively reaching one of our
        //       subvolumes through a bind mount or symlink chain — treat
        //       this as idempotent (already registered) or auto-adopt
        //       (orphan subvol after restart).
        //   (b) Anything else under mount_path (e.g. `.snapshots/...`, a
        //       nested directory inside a subvol, or an unknown name at
        //       the root). This is real self-referential nesting and
        //       must stay an error.
        if let Ok(rest) = abs_path.strip_prefix(&state.mount_path) {
            let mut comps = rest.components();
            let single = match (comps.next(), comps.next()) {
                (Some(first), None) => Some(first.as_os_str().to_string_lossy().to_string()),
                _ => None,
            };
            if let Some(ws_id) = single {
                if let Some(existing) = state.get_by_wsid(&ws_id) {
                    let ws = existing.read().await;
                    warn!(
                        "init target {} resolves to managed subvolume {:?}; \
                         treating as already initialized",
                        workspace, abs_path
                    );
                    return Ok(Response::InitOk {
                        ws_id: ws.ws_id.clone(),
                    });
                }
                // Orphan subvol — re-adopt if its snapshot bucket exists
                // (created at init, proving it was a real workspace).
                if tokio::fs::metadata(state.backend.snapshots_root().join(&ws_id))
                    .await
                    .is_ok()
                {
                    warn!(
                        "init target {} resolves to orphan subvolume {:?}; \
                         re-adopting (ws_id={})",
                        workspace, abs_path, ws_id
                    );
                    return Ok(adopt_existing_subvol(state, &ws_id, abs_path.clone()).await);
                }
            }
        }
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            format!(
                "path is inside mount_path ({}): {}",
                state.mount_path.display(),
                abs_path.display()
            ),
        ));
    }

    let abs_path_str = abs_path.to_string_lossy().to_string();

    // Every backend's init moves the original directory aside as a backup, and
    // rename(2) returns EBUSY for a directory that is itself a mount point. Any
    // filesystem qualifies (an in-place SkillFS over-mount is the common case).
    // Reject here so the operator gets an actionable message instead of the raw
    // "failed to rename original directory to backup: Device or resource busy".
    if crate::util::is_mounted(&abs_path_str).await? {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            format!(
                "workspace root is an active mount point: {}; initialize a \
                 subdirectory of the mount instead, or unmount it first and re-run",
                abs_path.display()
            ),
        ));
    }

    if let Some(resp) = crate::util::guard_cwd_occupants(&abs_path_str).await {
        return Ok(resp);
    }

    // Check rsync available
    let rsync_check = Command::new("which")
        .arg("rsync")
        .output()
        .await
        .context("failed to run 'which rsync'")?;
    if !rsync_check.status.success() {
        return Ok(error_resp(
            ErrorCode::InternalError,
            "rsync is not installed or not in PATH",
        ));
    }

    // 3. Generate ws-id
    let mount_path = &state.mount_path;
    let base_id = generate_ws_id_base(&abs_path.to_string_lossy());
    let mut ws_id = base_id.clone();
    let mut suffix = 2u32;
    while mount_path.join(&ws_id).exists() {
        ws_id = format!("{}-{}", base_id, suffix);
        suffix += 1;
    }

    // Serialize against a concurrent `recover` of the same path: ws_id is
    // SHA256(path), so a recover already in flight would race with our
    // index_store::save / register / save_manifest below.
    let _wsid_guard = state.lock_wsid(&ws_id).await;

    let abs_path_str = abs_path.to_string_lossy().to_string();

    // Steps 4-11 via backend, with cleanup handled internally
    if let Err(e) = state.backend.init_workspace(&abs_path_str, &ws_id).await {
        error!("init failed: {:#}", e);
        return Err(e);
    }

    // 12. Create and save index
    let snap_dir = state.index_dir(&ws_id);
    tokio::fs::create_dir_all(&snap_dir)
        .await
        .context("Failed to create index dir")?;
    // Check for existing snapshot subvolumes before creating empty index
    // Note: rebuild_from_fs scans the btrfs snapshot directory (backend snapshots_root),
    //       not the index directory
    let snapshots_ws_dir = state.backend.snapshots_root().join(&ws_id);
    let index = if let Ok(rebuilt) =
        index_store::rebuild_from_fs(&snapshots_ws_dir, abs_path.clone()).await
    {
        if !rebuilt.snapshots.is_empty() {
            info!(
                "Found {} existing snapshot(s) for {}, rebuilding index",
                rebuilt.snapshots.len(),
                ws_id
            );
            rebuilt
        } else {
            SnapshotIndex::new(abs_path.clone())
        }
    } else {
        SnapshotIndex::new(abs_path.clone())
    };
    index_store::save(&snap_dir, &index).await?;

    // 12a. Pick up any pre-existing per-ws policy.toml before registering.
    // If `state.json` was lost but `policy.toml` survived, ws_id stays stable
    // (SHA256(path)), so default = "inherit global" here would silently
    // revert and let scheduler delete protected snapshots. Shared helper
    // handles missing→inherit / Err→fail-safe. See [[ws-failsafe]].
    let (policy, failsafe) = load_workspace_policy_with_failsafe(&snap_dir, &ws_id, "init");

    // 13. Register to state
    state.register_workspace_with_policy(ws_id.clone(), abs_path.clone(), index, policy, failsafe);

    // 13a. Save manifest
    if let Err(e) = state.save_manifest().await {
        warn!("save_manifest failed after init: {:#}", e);
    }

    // Lifecycle window done (state + index_dir written). Watcher / warmup
    // below only read paths, so let a queued recover proceed.
    drop(_wsid_guard);

    // 13b. Start file watcher for write-lock detection
    match crate::fs_watcher::WorkspaceWatcher::start(&abs_path) {
        Ok(watcher) => {
            state.register_watcher(ws_id.clone(), watcher);
        }
        Err(e) => {
            warn!("Failed to start watcher for {}: {}", ws_id, e);
        }
    }

    // 13b. Warmup btrfs metadata cache for subsequent operations
    let subvol_path = state.backend.data_root().join(&ws_id);
    info!(
        "warming up btrfs metadata cache for workspace: {}",
        subvol_path.display()
    );
    crate::backends::btrfs_common::warmup_snapshot_metadata(&subvol_path).await;

    info!("workspace initialized: {}", ws_id);

    // 14. Return
    Ok(Response::InitOk { ws_id })
}

/// Generate a ws-id from a workspace path. Pure logic, extracted for testability.
/// Returns the base ws-id (without collision suffix).
fn generate_ws_id_base(path: &str) -> String {
    let hash = hex::encode(&Sha256::digest(path.as_bytes())[..3]);
    format!("ws-{}", hash)
}

// ── delete ──

pub async fn delete_snapshot(
    state: &Arc<DaemonState>,
    workspace: &str,
    snapshot_id: &str,
    force: bool,
) -> anyhow::Result<Response> {
    // 1. Resolve workspace (by ID, absolute path, or relative path)
    let ws_lock = match state.resolve_workspace(workspace).await {
        Some(ws) => ws,
        None => {
            return Ok(error_resp(
                ErrorCode::WorkspaceNotFound,
                format!("workspace not found: {}", workspace),
            ));
        }
    };
    let Some((_ws_id, _mutation_guard)) = state.lock_workspace_mutation_if_current(&ws_lock).await
    else {
        return Ok(error_resp(
            ErrorCode::WorkspaceNotFound,
            format!("workspace not found: {}", workspace),
        ));
    };

    // 1a. Detached-registration guard: refuse before unlinking snapshots of a
    // subvolume the registered path no longer exposes to the user.
    if let Some(resp) = state.detached_registration_error(&ws_lock).await {
        return Ok(resp);
    }

    // 2. Write lock after the mutation mutex.
    let mut ws = ws_lock.write().await;

    // 2a. Resolve snapshot by prefix within this workspace
    let resolved_id = match ws.index.resolve_by_prefix(snapshot_id) {
        Ok((id, _)) => id.clone(),
        Err(ResolveError::NotFound) => {
            return Ok(error_resp(
                ErrorCode::SnapshotNotFound,
                format!("snapshot not found: {}", snapshot_id),
            ));
        }
        Err(ResolveError::Ambiguous(n)) => {
            return Ok(error_resp(
                ErrorCode::SnapshotNotFound,
                format!("ambiguous snapshot prefix '{}': {} matches", snapshot_id, n),
            ));
        }
    };

    // 3. Check pinned
    if let Some(meta) = ws.index.snapshots.get(&resolved_id) {
        if meta.pinned && !force {
            return Ok(error_resp(
                ErrorCode::ConfirmationRequired,
                "Snapshot is pinned, use --force to confirm deletion".to_string(),
            ));
        }
    }

    // 4. Delete subvolume (skip if snapshot is marked missing — subvolume already gone)
    let is_missing = ws
        .index
        .snapshots
        .get(&resolved_id)
        .map(|m| m.missing)
        .unwrap_or(false);
    if !is_missing {
        state
            .backend
            .delete_snapshot(&ws.ws_id, &resolved_id)
            .await?;
    }

    // 5. Unlink from DAG, then remove from index + save
    ws.index.unlink_node(&resolved_id);
    ws.index.snapshots.remove(&resolved_id);
    ws.index.governed_evidence.remove(&resolved_id);
    let snap_dir = state.index_dir(&ws.ws_id);
    tokio::fs::create_dir_all(&snap_dir)
        .await
        .with_context(|| format!("Failed to create index dir: {:?}", snap_dir))?;
    index_store::save(&snap_dir, &ws.index).await?;

    // 5a. Release write lock before save_manifest
    drop(ws);

    // 5b. Save manifest
    if let Err(e) = state.save_manifest().await {
        warn!("save_manifest failed after delete_snapshot: {:#}", e);
    }

    // 6. Return
    Ok(Response::DeleteOk {
        target: resolved_id,
    })
}

// ── recover ──

/// Remove the entire per-ws index dir (`index.json` + `policy.toml` + any
/// future siblings) recursively. NotFound is fine, other errors warn-only.
///
/// Called by `recover_workspace` after `unregister`: ws_id is SHA256(path),
/// so a future init at the same path would collide on the same dir and
/// inherit stale `index.json` *and* `policy.toml` — both would mislead
/// scheduler/PATCH about a workspace the user just tore down.
async fn wipe_index_dir(state: &Arc<DaemonState>, ws_id: &str) {
    let dir = state.index_dir(ws_id);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(
            "wipe index dir {:?} for ws {}: {:#} \
             (next init at this path may inherit stale metadata)",
            dir, ws_id, e
        ),
    }
}

pub async fn recover_workspace(
    state: &Arc<DaemonState>,
    workspace: &str,
) -> anyhow::Result<Response> {
    // 1. resolve workspace (by ID, path, or relative)
    let ws_lock = match state.resolve_workspace(workspace).await {
        Some(ws) => ws,
        None => {
            return Ok(error_resp(
                ErrorCode::WorkspaceNotFound,
                format!("workspace not found: {}", workspace),
            ));
        }
    };

    // Block every mutation of this workspace through unregister, index removal,
    // and manifest persistence.
    let Some((ws_id, _mutation_guard)) = state.lock_workspace_mutation_if_current(&ws_lock).await
    else {
        return Ok(error_resp(
            ErrorCode::WorkspaceNotFound,
            format!("workspace not found: {}", workspace),
        ));
    };
    let original_path = ws_lock.read().await.path.to_string_lossy().to_string();

    // Intentionally no cwd guard: recover is a terminal "tear out" operation
    // gated by CLI ConfirmationRequired. The CLI prompt is the contract.

    // 3. call backend recover
    state
        .backend
        .recover_workspace(&ws_id, &original_path)
        .await?;

    // 4. unregister workspace from state
    state.unregister_workspace(&ws_id).await;

    // 4a. Wipe the entire per-ws index dir (policy.toml + index.json) so a
    // future init at the same path doesn't inherit stale metadata
    // (ws_id is SHA256(path), so it would collide).
    wipe_index_dir(state, &ws_id).await;

    // 4b. Save manifest
    if let Err(e) = state.save_manifest().await {
        warn!("save_manifest failed after recover: {:#}", e);
    }

    // 5. return
    Ok(Response::RecoverOk {
        workspace: original_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ws_ckpt_common::backend::StorageBackend;
    use ws_ckpt_common::{CleanupRetention, DaemonConfig, ErrorCode, SnapshotIndex};

    fn test_backend() -> Arc<dyn StorageBackend> {
        Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            PathBuf::from("/tmp/test-mount"),
            PathBuf::from("/tmp/test.img"),
        ))
    }

    fn test_config() -> DaemonConfig {
        DaemonConfig {
            mount_path: PathBuf::from("/tmp/test-mount"),
            socket_path: PathBuf::from("/tmp/test.sock"),
            log_level: "info".to_string(),
            auto_cleanup: false,
            auto_cleanup_keep: CleanupRetention::Count(20),
            auto_cleanup_interval_secs: 86_400,
            health_check_interval_secs: 300,
            backend_type: "auto".to_string(),
            img_size: 30,
            img_max_percent: 40.0,
            min_free_bytes: 512 * 1024 * 1024,
            min_free_percent: 1.0,
        }
    }

    fn test_state_dir() -> PathBuf {
        PathBuf::from("/tmp/test-state")
    }

    // ── ws-id generation tests ──

    #[test]
    fn ws_id_format_is_workspace_dash_6hex() {
        let id = generate_ws_id_base("/home/user/project");
        assert!(id.starts_with("ws-"), "ws-id should start with 'ws-'");
        let hash_part = id.strip_prefix("ws-").unwrap();
        assert_eq!(
            hash_part.len(),
            6,
            "hash part should be 6 hex chars (3 bytes)"
        );
        assert!(
            hash_part.chars().all(|c| c.is_ascii_hexdigit()),
            "hash part should be valid hex"
        );
    }

    #[test]
    fn ws_id_same_path_produces_same_id() {
        let id1 = generate_ws_id_base("/home/user/project");
        let id2 = generate_ws_id_base("/home/user/project");
        assert_eq!(id1, id2);
    }

    #[test]
    fn ws_id_different_paths_produce_different_ids() {
        let id1 = generate_ws_id_base("/home/user/project-a");
        let id2 = generate_ws_id_base("/home/user/project-b");
        assert_ne!(id1, id2);
    }

    #[test]
    fn ws_id_hash_matches_sha256_first_3_bytes() {
        use sha2::{Digest, Sha256};
        let path = "/some/test/path";
        let expected_hash = hex::encode(&Sha256::digest(path.as_bytes())[..3]);
        let id = generate_ws_id_base(path);
        assert_eq!(id, format!("ws-{}", expected_hash));
    }

    #[test]
    fn ws_id_collision_suffix_format() {
        // Verify the collision suffix pattern ws-{hash}-2, -3, etc.
        // We can't easily test the filesystem-dependent loop, but we can verify the format
        let base = generate_ws_id_base("/some/path");
        let suffixed_2 = format!("{}-2", base);
        let suffixed_3 = format!("{}-3", base);
        assert!(suffixed_2.starts_with("ws-"));
        assert!(suffixed_2.ends_with("-2"));
        assert!(suffixed_3.ends_with("-3"));
    }

    // ── error_resp helper test ──

    #[test]
    fn error_resp_constructs_correct_response() {
        let resp = error_resp(ErrorCode::WorkspaceNotFound, "not found");
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::WorkspaceNotFound);
                assert_eq!(message, "not found");
            }
            _ => panic!("expected Error variant"),
        }
    }

    // ── ConfirmationRequired tests ──

    #[test]
    fn confirmation_required_delete_pinned_snapshot_response() {
        let resp = error_resp(
            ErrorCode::ConfirmationRequired,
            "Snapshot is pinned, use --force to confirm deletion",
        );
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::ConfirmationRequired);
                assert!(message.contains("pinned"));
                assert!(message.contains("--force"));
            }
            _ => panic!("expected ConfirmationRequired error"),
        }
    }

    // ── Integration tests that require root + btrfs ──

    // ── Non-ignored async tests (use tempdir, no btrfs needed) ──

    #[tokio::test]
    async fn init_nonexistent_path_returns_invalid_path() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let resp = init(&state, "/nonexistent/path/12345").await.unwrap();
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidPath),
            _ => panic!("expected InvalidPath error"),
        }
    }

    #[tokio::test]
    async fn init_symlink_loop_reports_resolution_error_not_shared_volume_hint() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        // A self-referential symlink exists on disk (so it passes any lstat-based
        // pre-check), but canonicalize cannot resolve it (ELOOP). The error must
        // report the real resolution failure, not the sidecar "path does not
        // exist in the daemon's mount namespace" hint, which would send users
        // looking at their shared-volume layout for a symlink problem.
        let tmpdir = tempfile::tempdir().unwrap();
        let loop_link = tmpdir.path().join("loop");
        tokio::fs::symlink("loop", &loop_link).await.unwrap();
        let resp = init(&state, &loop_link.to_string_lossy()).await.unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InvalidPath);
                assert!(
                    message.contains("cannot resolve"),
                    "expected a resolution error, got: {}",
                    message
                );
                assert!(
                    !message.contains("shared with the daemon container"),
                    "must not point at the sidecar shared-volume layout: {}",
                    message
                );
            }
            other => panic!("expected InvalidPath error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn init_empty_workspace_returns_invalid_path() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        for blank in ["", "   ", "\t"] {
            let resp = init(&state, blank).await.unwrap();
            match resp {
                Response::Error { code, message } => {
                    assert_eq!(code, ErrorCode::InvalidPath);
                    assert!(
                        message.contains("empty"),
                        "expected empty-path message, got: {}",
                        message
                    );
                }
                other => panic!(
                    "expected InvalidPath error for blank input, got {:?}",
                    other
                ),
            }
        }
    }

    #[tokio::test]
    async fn init_root_path_returns_invalid_path() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        // All of these canonicalize to "/" and must be rejected.
        for variant in ["/", "///", "/.", "/./"] {
            let resp = init(&state, variant).await.unwrap();
            match resp {
                Response::Error { code, message } => {
                    assert_eq!(code, ErrorCode::InvalidPath, "variant {:?}", variant);
                    assert!(
                        message.contains("root"),
                        "variant {:?}: expected root-rejection message, got: {}",
                        variant,
                        message
                    );
                }
                other => panic!(
                    "variant {:?}: expected InvalidPath error, got {:?}",
                    variant, other
                ),
            }
        }
    }

    #[test]
    fn strip_trailing_slashes_preserves_empty_and_root() {
        assert_eq!(strip_trailing_slashes(""), "");
        assert_eq!(strip_trailing_slashes("/"), "/");
        assert_eq!(strip_trailing_slashes("///"), "/");
        assert_eq!(strip_trailing_slashes("/foo/"), "/foo");
        assert_eq!(strip_trailing_slashes("/foo"), "/foo");
        assert_eq!(strip_trailing_slashes("foo/"), "foo");
    }

    #[tokio::test]
    async fn init_already_initialized_returns_ok() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let data_root = state.backend.data_root().to_path_buf();
        let subvol = data_root.join("ws-exist");
        tokio::fs::create_dir_all(&subvol).await.unwrap();
        let tmpdir = tempfile::tempdir().unwrap();
        let ws_link = tmpdir.path().join("myws");
        tokio::fs::symlink(&subvol, &ws_link).await.unwrap();
        state.register_workspace(
            "ws-exist".to_string(),
            ws_link.clone(),
            SnapshotIndex::new(ws_link.clone()),
        );
        let resp = init(&state, &ws_link.to_string_lossy()).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&subvol).await;
        match resp {
            Response::InitOk { ws_id } => assert_eq!(ws_id, "ws-exist"),
            _ => panic!("expected InitOk for already-initialized workspace"),
        }
    }

    #[tokio::test]
    async fn init_registered_but_regular_dir_returns_error_with_hint() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().to_string_lossy().to_string();
        let canon = tokio::fs::canonicalize(&path).await.unwrap();
        state.register_workspace(
            "ws-gone".to_string(),
            canon.clone(),
            SnapshotIndex::new(canon),
        );
        let resp = init(&state, &path).await.unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InternalError);
                assert!(
                    message.contains("regular directory"),
                    "hint missing: {message}"
                );
            }
            _ => panic!("expected error for registered workspace whose symlink was replaced"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn init_rejects_non_utf8_canonicalized_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tmpdir = tempfile::tempdir().unwrap();
        // Real directory with a non-UTF-8 byte (\xFF) in its name.
        let raw_name = OsStr::from_bytes(b"non-utf8-\xFFdir");
        let raw_dir = tmpdir.path().join(raw_name);
        tokio::fs::create_dir(&raw_dir).await.unwrap();
        // ASCII symlink so the user-facing path is valid UTF-8 but resolves to
        // the non-UTF-8 directory after canonicalize.
        let link = tmpdir.path().join("ascii-link");
        tokio::fs::symlink(&raw_dir, &link).await.unwrap();

        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let resp = init(&state, &link.to_string_lossy()).await.unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InvalidPath);
                assert!(message.contains("not valid UTF-8"), "message: {}", message);
            }
            _ => panic!("expected InvalidPath error for non-UTF-8 canonicalized path"),
        }
    }

    #[tokio::test]
    async fn init_path_inside_mount_path_returns_invalid_path() {
        let mount_dir = tempfile::tempdir().unwrap();
        let inside_path = mount_dir.path().join("subdir");
        tokio::fs::create_dir_all(&inside_path).await.unwrap();
        let config = DaemonConfig {
            mount_path: tokio::fs::canonicalize(mount_dir.path()).await.unwrap(),
            socket_path: PathBuf::from("/tmp/test.sock"),
            log_level: "info".to_string(),
            auto_cleanup: false,
            auto_cleanup_keep: CleanupRetention::Count(20),
            auto_cleanup_interval_secs: 86_400,
            health_check_interval_secs: 300,
            backend_type: "auto".to_string(),
            img_size: 30,
            img_max_percent: 40.0,
            min_free_bytes: 512 * 1024 * 1024,
            min_free_percent: 1.0,
        };
        let state = Arc::new(DaemonState::new(config, test_backend(), test_state_dir()));
        let resp = init(&state, &inside_path.to_string_lossy()).await.unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InvalidPath);
                assert!(message.contains("inside mount_path"));
            }
            _ => panic!("expected InvalidPath error for path inside mount_path"),
        }
    }

    #[tokio::test]
    async fn init_canonical_into_managed_subvol_is_idempotent() {
        // User-facing path resolves (via bind mount / symlink chain) into
        // `mount_path/<ws_id>` for a workspace that's already registered.
        // Expectation: warn + InitOk, not InvalidPath.
        let mount_dir = tempfile::tempdir().unwrap();
        let mount_path = tokio::fs::canonicalize(mount_dir.path()).await.unwrap();
        let ws_id = "ws-abc123";
        let subvol_path = mount_path.join(ws_id);
        tokio::fs::create_dir_all(&subvol_path).await.unwrap();

        let mut cfg = test_config();
        cfg.mount_path = mount_path.clone();
        let state = Arc::new(DaemonState::new(cfg, test_backend(), test_state_dir()));
        state.register_workspace(
            ws_id.to_string(),
            PathBuf::from("/some/user/facing/path"),
            SnapshotIndex::new(PathBuf::from("/some/user/facing/path")),
        );

        let resp = init(&state, &subvol_path.to_string_lossy()).await.unwrap();
        match resp {
            Response::InitOk { ws_id: returned } => assert_eq!(returned, ws_id),
            other => panic!("expected idempotent InitOk, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn init_non_directory_returns_invalid_path() {
        let tmpdir = tempfile::tempdir().unwrap();
        let file_path = tmpdir.path().join("not-a-dir.txt");
        tokio::fs::write(&file_path, "hello").await.unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let resp = init(&state, &file_path.to_string_lossy()).await.unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InvalidPath);
                assert!(message.contains("not a directory"));
            }
            _ => panic!("expected InvalidPath error for non-directory"),
        }
    }

    #[tokio::test]
    async fn delete_snapshot_unregistered_workspace_returns_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().to_string_lossy().to_string();
        let resp = delete_snapshot(&state, &path, "msg1-step0", false)
            .await
            .unwrap();
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error"),
        }
    }

    // ── Detached-registration guard: delete_snapshot ──

    /// Real registration topology: tempdir as backend data root,
    /// `<root>/ws-<id>` as the live-subvolume stand-in, workspace symlink at it.
    /// Returns (state, workspace symlink, tempdir to keep alive).
    fn live_topology(ws_id: &str) -> (Arc<DaemonState>, PathBuf, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> =
            Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
                temp.path().to_path_buf(),
                temp.path().join("test.img"),
            ));
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend,
            temp.path().join("state"),
        ));
        let subvol = temp.path().join(ws_id);
        std::fs::create_dir_all(&subvol).unwrap();
        let ws_link = temp.path().join("ws-link");
        std::os::unix::fs::symlink(&subvol, &ws_link).unwrap();
        let mut index = SnapshotIndex::new(ws_link.clone());
        index.snapshots.insert(
            "snap-1".to_string(),
            ws_ckpt_common::SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );
        state.register_workspace(ws_id.to_string(), ws_link.clone(), index);
        (state, ws_link, temp)
    }

    #[tokio::test]
    async fn delete_snapshot_healthy_registration_reaches_snapshot_resolution() {
        let (state, _ws_link, _temp) = live_topology("ws-del-live");
        // A healthy registration must get PAST the guard: the unknown snapshot
        // id fails at the later resolve-by-prefix stage, not the detach guard.
        let resp = delete_snapshot(&state, "ws-del-live", "no-such", false)
            .await
            .unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::SnapshotNotFound);
                assert!(message.contains("no-such"), "got: {message}");
            }
            other => panic!("expected SnapshotNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_snapshot_refuses_detached_registration() {
        let (state, ws_link, _temp) = live_topology("ws-del-gone");
        // Detach: replace the symlink with a plain directory (issue #3059 repro).
        std::fs::remove_file(&ws_link).unwrap();
        std::fs::create_dir(&ws_link).unwrap();

        // Addressing by registered path …
        let resp = delete_snapshot(&state, &ws_link.to_string_lossy(), "snap-1", false)
            .await
            .unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InternalError);
                assert!(message.contains("recover"), "hint missing: {message}");
                assert!(
                    message.contains("regular directory"),
                    "note missing: {message}"
                );
            }
            other => panic!("expected detach error, got {other:?}"),
        }
        // … and by ws_id: the guard checks the registered path either way.
        let resp = delete_snapshot(&state, "ws-del-gone", "snap-1", false)
            .await
            .unwrap();
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InternalError);
                assert!(message.contains("recover"), "hint missing: {message}");
            }
            other => panic!("expected detach error, got {other:?}"),
        }
    }

    // ── Pure logic: ws-id edge cases ──

    #[test]
    fn ws_id_empty_path() {
        let id = generate_ws_id_base("");
        assert!(id.starts_with("ws-"));
        let hash_part = id.strip_prefix("ws-").unwrap();
        assert_eq!(hash_part.len(), 6);
    }

    #[test]
    fn ws_id_special_characters_in_path() {
        let id = generate_ws_id_base("/home/user/my project (2)/src");
        assert!(id.starts_with("ws-"));
        let hash_part = id.strip_prefix("ws-").unwrap();
        assert_eq!(hash_part.len(), 6);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ws_id_very_long_path() {
        let long_path = format!("/home/{}", "a".repeat(1000));
        let id = generate_ws_id_base(&long_path);
        assert!(id.starts_with("ws-"));
        let hash_part = id.strip_prefix("ws-").unwrap();
        assert_eq!(hash_part.len(), 6);
    }

    #[test]
    fn ws_id_unicode_path() {
        let id = generate_ws_id_base("/home/用户/项目");
        assert!(id.starts_with("ws-"));
        let hash_part = id.strip_prefix("ws-").unwrap();
        assert_eq!(hash_part.len(), 6);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ── recover tests ──

    #[tokio::test]
    async fn recover_unregistered_workspace_returns_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            test_state_dir(),
        ));
        let resp = recover_workspace(&state, "/nonexistent/path/12345")
            .await
            .unwrap();
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error"),
        }
    }

    #[tokio::test]
    async fn wipe_index_dir_removes_policy_and_is_idempotent() {
        // Recover must clear per-ws metadata so a future init at the same path
        // doesn't inherit a corrupted policy.toml (which would trigger fail-safe
        // and lock out PATCH).
        let state_tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            state_tmp.path().to_path_buf(),
        ));
        let ws_id = "ws-wipe-me";
        let dir = state.index_dir(ws_id);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("policy.toml"), b"auto_cleanup = true\n")
            .await
            .unwrap();
        tokio::fs::write(dir.join("index.json"), b"{}")
            .await
            .unwrap();
        assert!(dir.exists());

        wipe_index_dir(&state, ws_id).await;
        assert!(!dir.exists(), "wipe must remove the index dir");

        // Second call on already-absent dir must not error or panic.
        wipe_index_dir(&state, ws_id).await;
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn recover_orchestration_calls_backend_then_unregisters_and_wipes_index() {
        // Happy-path orchestration: backend called, ws unregistered, index dir wiped.
        let state_tmp = tempfile::tempdir().unwrap();
        let backend = Arc::new(RecorderStubBackend::new());
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend.clone() as Arc<dyn StorageBackend>,
            state_tmp.path().to_path_buf(),
        ));
        let ws_tmp = tempfile::tempdir().unwrap();
        let canon = tokio::fs::canonicalize(ws_tmp.path()).await.unwrap();
        let ws_id = "ws-recov-orch";
        state.register_workspace(
            ws_id.to_string(),
            canon.clone(),
            SnapshotIndex::new(canon.clone()),
        );
        // Simulate prior PATCH: write a real policy.toml inside index_dir.
        let dir = state.index_dir(ws_id);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("policy.toml"), b"auto_cleanup = true\n")
            .await
            .unwrap();

        let resp = recover_workspace(&state, &canon.to_string_lossy())
            .await
            .unwrap();
        assert!(matches!(resp, Response::RecoverOk { .. }));
        assert_eq!(
            backend.recover_call_count(),
            1,
            "backend.recover_workspace must run once"
        );
        assert!(
            state.get_by_wsid(ws_id).is_none(),
            "ws must be unregistered"
        );
        assert!(!dir.exists(), "recover must wipe stale per-ws index dir");
    }

    #[tokio::test]
    async fn recover_blocks_on_lock_wsid_held_by_concurrent_lifecycle_op() {
        // White-box proof that `recover_workspace` takes `state.lock_wsid(ws_id)`
        // on the same key that init/adopt take — without it, a concurrent
        // init on the same path could race recover's unregister+wipe.
        // We hold the lock externally, spawn recover, and assert it has
        // NOT reached the backend until we drop our guard.
        use std::time::Duration;

        let state_tmp = tempfile::tempdir().unwrap();
        let backend = Arc::new(RecorderStubBackend::new());
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend.clone() as Arc<dyn StorageBackend>,
            state_tmp.path().to_path_buf(),
        ));
        let ws_tmp = tempfile::tempdir().unwrap();
        let canon = tokio::fs::canonicalize(ws_tmp.path()).await.unwrap();

        // ws_id is what `init` would have computed: SHA256(canonicalize(path))[:6].
        let mut hasher = Sha256::new();
        hasher.update(canon.to_string_lossy().as_bytes());
        let ws_id = format!("ws-{}", &format!("{:x}", hasher.finalize())[..6]);
        state.register_workspace(
            ws_id.clone(),
            canon.clone(),
            SnapshotIndex::new(canon.clone()),
        );

        // Outside the recover, hold the per-ws_id lifecycle lock — simulates
        // a concurrent init / adopt that's mid-flight on the same ws_id.
        let held_guard = state.lock_wsid(&ws_id).await;

        let state_for_task = state.clone();
        let canon_str = canon.to_string_lossy().to_string();
        let handle = tokio::spawn(async move {
            recover_workspace(&state_for_task, &canon_str)
                .await
                .unwrap()
        });

        // Give the task time to reach `lock_wsid` and park on it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            backend.recover_call_count(),
            0,
            "recover must NOT have reached backend while lock_wsid is held",
        );

        // Release the lock — recover must now proceed.
        drop(held_guard);
        let resp = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("recover did not progress after lock_wsid was released")
            .unwrap();
        assert!(matches!(resp, Response::RecoverOk { .. }));
        assert_eq!(backend.recover_call_count(), 1);
    }

    #[tokio::test]
    async fn recover_unresolvable_path_returns_not_found_no_backend_call() {
        // Unresolvable path short-circuits before reaching the backend.
        let backend = Arc::new(RecorderStubBackend::new());
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend.clone() as Arc<dyn StorageBackend>,
            tempfile::tempdir().unwrap().path().to_path_buf(),
        ));
        let resp = recover_workspace(&state, "/nonexistent/path/xyz")
            .await
            .unwrap();
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            other => panic!("expected WorkspaceNotFound, got {:?}", other),
        }
        assert_eq!(backend.recover_call_count(), 0);
    }

    // ── Stub backend: records recover_workspace calls; other methods panic ──
    struct RecorderStubBackend {
        data_root_path: PathBuf,
        snapshots_root_path: PathBuf,
        recover_calls: std::sync::atomic::AtomicUsize,
    }

    impl RecorderStubBackend {
        fn new() -> Self {
            Self {
                data_root_path: PathBuf::from("/tmp/stub-data"),
                snapshots_root_path: PathBuf::from("/tmp/stub-snapshots"),
                recover_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn recover_call_count(&self) -> usize {
            self.recover_calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl StorageBackend for RecorderStubBackend {
        fn backend_type(&self) -> ws_ckpt_common::backend::BackendType {
            ws_ckpt_common::backend::BackendType::BtrfsBase
        }
        fn data_root(&self) -> &std::path::Path {
            &self.data_root_path
        }
        fn snapshots_root(&self) -> &std::path::Path {
            &self.snapshots_root_path
        }
        async fn recover_workspace(&self, _: &str, _: &str) -> anyhow::Result<()> {
            self.recover_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn init_workspace(
            &self,
            _: &str,
            _: &str,
        ) -> anyhow::Result<ws_ckpt_common::WorkspaceInfo> {
            unimplemented!("stub: init_workspace not used")
        }
        async fn create_snapshot(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn rollback(&self, _: &str, _: &str) -> anyhow::Result<PathBuf> {
            unimplemented!()
        }
        async fn delete_snapshot(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn diff(
            &self,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<ws_ckpt_common::DiffEntry>> {
            unimplemented!()
        }
        async fn cleanup_snapshots(
            &self,
            _: &str,
            _: &[String],
        ) -> anyhow::Result<Vec<(String, ws_ckpt_common::backend::SnapshotDeleteOutcome)>> {
            unimplemented!()
        }
        async fn fork(&self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn gc_generations(
            &self,
            _: &str,
        ) -> anyhow::Result<ws_ckpt_common::backend::GcResult> {
            unimplemented!()
        }
        async fn check_environment(
            &self,
        ) -> anyhow::Result<ws_ckpt_common::backend::EnvironmentStatus> {
            unimplemented!()
        }
        async fn get_usage(&self) -> anyhow::Result<(u64, u64)> {
            unimplemented!()
        }
    }
}
