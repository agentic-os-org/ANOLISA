use std::sync::Arc;

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tracing::{error, info, warn};

use ws_ckpt_common::{load_workspace_policy_with_failsafe, ErrorCode, Response, SnapshotIndex};

use crate::index_store;
use crate::state::{normalize_registration_path, DaemonState};

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

/// Re-adopt storage only through a verified user-facing registration anchor.
async fn adopt_existing_subvol(
    state: &Arc<DaemonState>,
    ws_id: &str,
    requested_path: std::path::PathBuf,
) -> anyhow::Result<Response> {
    let _wsid_guard = state.lock_wsid(ws_id).await;
    let data_root = tokio::fs::canonicalize(state.backend.data_root()).await?;
    let live_path = tokio::fs::canonicalize(data_root.join(ws_id)).await?;
    if live_path.parent() != Some(data_root.as_path()) {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            "workspace storage must be a direct child of the data root",
        ));
    }
    let snap_dir = state.index_dir(ws_id);
    let btrfs_snap_dir = state.backend.snapshots_root().join(ws_id);
    let existing_index = match index_store::load(&snap_dir).await {
        Ok(index) => Some(index),
        Err(_) if !snap_dir.join(ws_ckpt_common::INDEX_FILE).try_exists()? => None,
        Err(error) => return Err(error).context("cannot verify workspace ownership from index"),
    };
    let registered_path = normalize_registration_path(
        existing_index
            .as_ref()
            .map_or(requested_path.as_path(), |index| {
                index.workspace_path.as_path()
            }),
    )?;
    if registered_path.starts_with(&data_root) || registered_path == std::path::Path::new("/") {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            "re-adoption requires a user registration path outside managed storage",
        ));
    }
    if tokio::fs::canonicalize(&registered_path)
        .await
        .ok()
        .as_ref()
        != Some(&live_path)
    {
        return Ok(error_resp(ErrorCode::InvalidPath, "recorded workspace owner is detached; restore its registration link before re-adoption"));
    }
    // A missing index is recoverable only with both a user link to this exact
    // live root and the snapshot bucket created by initialization.
    if !tokio::fs::metadata(&btrfs_snap_dir).await?.is_dir() {
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            "workspace snapshot bucket is not a directory",
        ));
    }
    let mut index = existing_index.unwrap_or_else(|| SnapshotIndex::new(registered_path.clone()));
    index.workspace_path = registered_path.clone();
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
    )?;
    if let Err(e) = state.save_manifest().await {
        warn!("save_manifest failed after subvol re-adoption: {:#}", e);
    }
    Ok(Response::InitOk {
        ws_id: ws_id.to_string(),
    })
}

// Resolve the parent only: an interrupted init may have removed the final
// directory or left a dangling workspace symlink.
pub(crate) async fn orphan_workspace_path(
    workspace: &str,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    let path = std::path::Path::new(strip_trailing_slashes(workspace));
    let original = match normalize_registration_path(path) {
        Ok(path) => path,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let original_str = original
        .to_str()
        .context("workspace path is not valid UTF-8")?;
    match tokio::fs::symlink_metadata(crate::backends::btrfs_common::backup_path_for(original_str))
        .await
    {
        Ok(_) => Ok(Some(original)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// A truncated path hash is not proof of ownership. Candidates block automatic
// recovery, but are never deleted or copied over the complete original backup.
async fn orphan_storage_paths(
    state: &Arc<DaemonState>,
    original: &str,
) -> anyhow::Result<Vec<String>> {
    let base = generate_ws_id_base(original);
    let mut paths = Vec::new();
    let mut entries = match tokio::fs::read_dir(state.backend.data_root()).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == base
            || name
                .strip_prefix(&format!("{base}-"))
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            paths.push(entry.path().display().to_string());
        }
    }
    paths.sort();
    Ok(paths)
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
    // Lock before resolving aliases or observing the rename-to-symlink gap.
    // Init is globally serialized; use stable path identities if
    // concurrent initialization of unrelated workspaces becomes necessary.
    let init_guard = state.init_lock.lock().await;
    if let Some(existing) = state.resolve_workspace(workspace).await {
        if let Some(error) = state.detached_registration_error(&existing).await {
            return Ok(error);
        }
        return Ok(Response::InitOk {
            ws_id: existing.read().await.ws_id.clone(),
        });
    }
    if let Some(original) = orphan_workspace_path(workspace).await? {
        if state.registration_path_is_internal(&original)? || original == std::path::Path::new("/")
        {
            return Ok(error_resp(
                ErrorCode::InvalidPath,
                "cannot restore an orphan inside managed storage",
            ));
        }
        let original_str = original
            .to_str()
            .context("workspace path is not valid UTF-8")?;
        if state.get_by_path(&original).is_none() {
            let candidates = orphan_storage_paths(state, original_str).await?;
            let subvol = candidates
                .first()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    state
                        .backend
                        .data_root()
                        .join(generate_ws_id_base(original_str))
                });
            crate::backends::btrfs_common::recover_orphan_backup(original_str, &subvol).await?;
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
    // BtrfsBase can use an auto-detected mount unrelated to config.mount_path.
    // Recognize only direct managed children; reject all other paths in storage.
    let canonical_data_root = tokio::fs::canonicalize(state.backend.data_root())
        .await
        .ok();
    if let Some(rest) = canonical_data_root
        .as_ref()
        .and_then(|root| abs_path.strip_prefix(root).ok())
    {
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
                return adopt_existing_subvol(
                    state,
                    &ws_id,
                    normalize_registration_path(std::path::Path::new(workspace))?,
                )
                .await;
            }
        }
        return Ok(error_resp(
            ErrorCode::InvalidPath,
            format!(
                "path is inside backend data root ({}): {}",
                state.backend.data_root().display(),
                abs_path.display()
            ),
        ));
    }
    if abs_path.starts_with(&state.mount_path) {
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

    // 3. Generate and reserve a ws-id before migrating any user data. The
    // lifecycle lock closes the gap between checking every ownership source and
    // publishing the new registration.
    let data_root = state.backend.data_root();
    let base_id = generate_ws_id_base(&abs_path.to_string_lossy());
    let mut ws_id = base_id.clone();
    let mut suffix = 2u32;
    let _wsid_guard = loop {
        let guard = state.lock_wsid(&ws_id).await;
        let index_dir = state.index_dir(&ws_id);
        let occupied = state.get_by_wsid(&ws_id).is_some()
            || data_root.join(&ws_id).try_exists()?
            || state.backend.snapshots_root().join(&ws_id).try_exists()?
            || index_dir.try_exists()?
            || index_dir
                .with_file_name(format!("{}.unregistered", ws_id))
                .try_exists()?;
        if !occupied {
            break guard;
        }
        drop(guard);
        ws_id = format!("{}-{}", base_id, suffix);
        suffix += 1;
    };

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
    state.register_workspace_with_policy(
        ws_id.clone(),
        abs_path.clone(),
        index,
        policy,
        failsafe,
    )?;

    // 13a. Save manifest
    if let Err(e) = state.save_manifest().await {
        warn!("save_manifest failed after init: {:#}", e);
    }

    // Lifecycle window done (state + index_dir written). Watcher / warmup
    // below only read paths, so let a queued recover proceed.
    drop(_wsid_guard);
    drop(init_guard);

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

    // Never reinterpret a previously valid ID as a prefix after its removal.
    if !ws.index.snapshots.contains_key(snapshot_id) {
        return Ok(error_resp(
            ErrorCode::SnapshotNotFound,
            format!("snapshot not found: {snapshot_id}"),
        ));
    }
    let resolved_id = snapshot_id.to_string();

    // 3. Check pinned
    if let Some(meta) = ws.index.snapshots.get(&resolved_id) {
        if meta.pinned && !force {
            return Ok(error_resp(
                ErrorCode::ConfirmationRequired,
                "Snapshot is pinned, use --force to confirm deletion".to_string(),
            ));
        }
    }

    // Check disk as well: cleanup may have stopped before persisting a marker.
    let is_missing = !tokio::fs::try_exists(
        state
            .backend
            .snapshots_root()
            .join(&ws.ws_id)
            .join(&resolved_id),
    )
    .await
    .with_context(|| format!("inspect snapshot {resolved_id}"))?;
    if is_missing && ws.index.governed_evidence.contains_key(&resolved_id) {
        if let Some(meta) = ws.index.snapshots.get_mut(&resolved_id) {
            meta.missing = true;
        }
        index_store::save(&state.index_dir(&ws.ws_id), &ws.index).await?;
        return Ok(error_resp(
            ErrorCode::SnapshotNotFound,
            format!("snapshot subvolume is missing: {resolved_id}; guarded evidence retained"),
        ));
    }

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
    ws.index.recovered_orphans.remove(&resolved_id);
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

    if is_missing {
        return Ok(error_resp(
            ErrorCode::SnapshotNotFound,
            format!("snapshot subvolume is missing: {resolved_id}; record removed"),
        ));
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
    recover_workspace_inner(state, workspace, None).await
}

pub async fn recover_workspace_confirmed(
    state: &Arc<DaemonState>,
    preview: &ws_ckpt_common::RecoveryPreview,
) -> anyhow::Result<Response> {
    let workspace = preview
        .ws_id
        .as_deref()
        .unwrap_or(&preview.registration_path);
    recover_workspace_inner(state, workspace, Some(preview)).await
}

async fn recover_workspace_inner(
    state: &Arc<DaemonState>,
    workspace: &str,
    expected: Option<&ws_ckpt_common::RecoveryPreview>,
) -> anyhow::Result<Response> {
    let _init_guard = state.init_lock.lock().await;
    // 1. resolve workspace (by ID, path, or relative)
    let mut resolved = state.resolve_workspace(workspace).await;
    if resolved.is_none() {
        if let Some(original) = orphan_workspace_path(workspace).await? {
            // Parent aliases can hide a registered workspace from full-path
            // canonicalization, which follows the final managed symlink.
            resolved = state.get_by_path(&original);
            if resolved.is_none() {
                let original_str = original
                    .to_str()
                    .context("workspace path is not valid UTF-8")?;
                if state.registration_path_is_internal(&original)?
                    || original == std::path::Path::new("/")
                {
                    return Ok(error_resp(
                        ErrorCode::InvalidPath,
                        "cannot restore an orphan inside managed storage",
                    ));
                }
                if let Some(expected) = expected {
                    let current = crate::recover_preview::orphan_preview(state, &original).await?;
                    if &current != expected {
                        return Ok(recovery_preview_changed());
                    }
                }
                let retained = orphan_storage_paths(state, original_str).await?;
                crate::backends::btrfs_common::restore_orphan_backup(original_str).await?;
                return Ok(Response::RecoverWithWarning {
                    workspace: original_str.to_string(),
                    warning: format!(
                        "Restored the pre-init backup. Potentially partial or newer storage remains at {:?}; \
                         snapshots under {:?} are also retained. Inspect these before removing them. You can run init again.",
                        retained, state.backend.snapshots_root()
                    ),
                });
            }
        }
    }
    let Some(ws_lock) = resolved else {
        return Ok(error_resp(
            ErrorCode::WorkspaceNotFound,
            format!("workspace not found: {}", workspace),
        ));
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
    let ws = ws_lock.read().await;
    if let Some(expected) = expected {
        let current = crate::recover_preview::registered_preview(state, &ws).await?;
        if &current != expected {
            return Ok(recovery_preview_changed());
        }
    }
    let original_path = ws.path.to_string_lossy().to_string();
    drop(ws);

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
    let backup = crate::backends::btrfs_common::backup_path_for(&original_path);
    let warning = match archive_recovered_backup(&backup).await {
        Ok(Some(archive)) => Some(format!(
            "Pre-init backup archived at {:?}; inspect it before removal. You can run init again.",
            archive
        )),
        Ok(None) => None,
        Err(error) => Some(format!(
            "Workspace recovered, but pre-init backup {:?} could not be archived: {error:#}. \
             Inspect and move this backup to another location before running init again",
            backup
        )),
    };
    Ok(match warning {
        Some(warning) => Response::RecoverWithWarning {
            workspace: original_path,
            warning,
        },
        None => Response::RecoverOk {
            workspace: original_path,
        },
    })
}

fn recovery_preview_changed() -> Response {
    error_resp(
        ErrorCode::ConfirmationRequired,
        "workspace or snapshots changed since recovery preview; preview and confirm again",
    )
}

// Keep historical data outside the reserved orphan name so recover -> init
// does not mistake a completed recovery for an interrupted migration.
async fn archive_recovered_backup(backup: &str) -> anyhow::Result<Option<std::path::PathBuf>> {
    let backup = std::path::PathBuf::from(backup);
    tokio::task::spawn_blocking(move || {
        match std::fs::symlink_metadata(&backup) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        // Repeated recoveries retain distinct backups; bound name allocation
        // and never replace even an empty directory or a dangling symlink.
        for suffix in 0..1000 {
            let archive = if suffix == 0 {
                std::path::PathBuf::from(format!("{}.recovered", backup.display()))
            } else {
                std::path::PathBuf::from(format!("{}.recovered-{}", backup.display(), suffix))
            };
            match nix::fcntl::renameat2(
                None,
                backup.as_path(),
                None,
                archive.as_path(),
                nix::fcntl::RenameFlags::RENAME_NOREPLACE,
            ) {
                Ok(()) => return Ok(Some(archive)),
                Err(nix::errno::Errno::EEXIST) => continue,
                Err(e) => return Err(e).context("failed to archive pre-init backup"),
            }
        }
        anyhow::bail!("all 1000 backup archive names are occupied")
    })
    .await
    .context("backup archive task failed")?
}

/// Remove stale registration only after proving that the live subvolume is absent.
/// Keep snapshots and pre-init backups available for manual data recovery.
pub async fn unregister_missing_workspace(
    state: &Arc<DaemonState>,
    workspace: &str,
) -> anyhow::Result<Response> {
    let _init_guard = state.init_lock.lock().await;
    let Some(ws_lock) = state.resolve_workspace(workspace).await else {
        return Ok(error_resp(
            ErrorCode::WorkspaceNotFound,
            format!("workspace not found: {}", workspace),
        ));
    };
    let Some((ws_id, _mutation_guard)) = state.lock_workspace_mutation_if_current(&ws_lock).await
    else {
        return Ok(error_resp(
            ErrorCode::WorkspaceNotFound,
            format!("workspace not found: {}", workspace),
        ));
    };
    let original = ws_lock.read().await.path.clone();
    let subvol = state.backend.data_root().join(&ws_id);
    match tokio::fs::symlink_metadata(&subvol).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("cannot verify that the live subvolume is missing"),
        Ok(_) => {
            return Ok(error_resp(
                ErrorCode::InvalidPath,
                format!(
                    "cannot unregister: live subvolume {:?} still exists; use recover",
                    subvol
                ),
            ))
        }
    }
    let index_dir = state.index_dir(&ws_id);
    // Preserve snapshot metadata outside the active index namespace.
    let retained_index = index_dir.with_file_name(format!("{}.unregistered", ws_id));
    let archive_index = index_dir.try_exists()?;
    if archive_index {
        if retained_index.try_exists()? {
            anyhow::bail!(
                "retained index {:?} already exists; move it before unregistering",
                retained_index
            );
        }
        tokio::fs::rename(&index_dir, &retained_index).await?;
    }
    let persisted = async {
        if archive_index {
            // The archive must be durable before the manifest stops reserving
            // its active index; syncing state_dir alone does not sync indexes/.
            index_store::sync_parent(&index_dir).await?;
        }
        state.persist_unregister_workspace(&ws_id).await
    }
    .await;
    if let Err(error) = persisted {
        // save_state only returns errors before its atomic rename; runtime state
        // is unchanged, so restore the active index namespace for a safe retry.
        if archive_index {
            tokio::fs::rename(&retained_index, &index_dir).await.with_context(|| {
                format!("unregister persistence failed ({error:#}); also failed to restore archived index {:?} to {:?}", retained_index, index_dir)
            })?;
            index_store::sync_parent(&index_dir).await.with_context(|| {
                format!("unregister persistence failed ({error:#}); also failed to sync restored index {:?}", index_dir)
            })?;
        }
        return Err(error);
    }
    // Only remove our dangling link. Foreign files/directories remain untouched.
    if tokio::fs::read_link(&original)
        .await
        .is_ok_and(|target| target == subvol)
    {
        tokio::fs::remove_file(&original).await.with_context(|| {
            format!("registration removed, but could not remove dangling symlink {:?}; remove it before reinitializing", original)
        })?;
    }

    let mut retained_paths = Vec::new();
    for path in [
        state.backend.snapshots_root().join(&ws_id),
        crate::backends::btrfs_common::backup_path_for(&original.to_string_lossy()).into(),
        retained_index,
    ] {
        match tokio::fs::symlink_metadata(&path).await {
            Ok(_) => retained_paths.push(path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).context("registration removed, but cannot inspect retained data")
            }
        }
    }
    Ok(Response::UnregisterOk {
        workspace: original.display().to_string(),
        retained_paths,
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
        state
            .register_workspace(
                "ws-exist".to_string(),
                ws_link.clone(),
                SnapshotIndex::new(ws_link.clone()),
            )
            .unwrap();
        let resp = init(&state, &ws_link.to_string_lossy()).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&subvol).await;
        match resp {
            Response::InitOk { ws_id } => assert_eq!(ws_id, "ws-exist"),
            _ => panic!("expected InitOk for already-initialized workspace"),
        }
    }

    #[tokio::test]
    async fn init_skips_ws_id_reserved_only_by_registry() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let backend = Arc::new(RecorderStubBackend {
            data_root_path: temp.path().join("mount/data"),
            snapshots_root_path: temp.path().join("mount/snapshots"),
            ..RecorderStubBackend::new()
        });
        let state = Arc::new(DaemonState::new(
            DaemonConfig {
                mount_path: temp.path().join("configured-mount"),
                ..test_config()
            },
            backend.clone(),
            temp.path().join("state"),
        ));
        let base_id = generate_ws_id_base(workspace.to_str().unwrap());
        let detached_owner = temp.path().join("detached-owner");
        state
            .register_workspace(
                base_id.clone(),
                detached_owner.clone(),
                SnapshotIndex::new(detached_owner.clone()),
            )
            .unwrap();

        let response = init(&state, workspace.to_str().unwrap()).await.unwrap();

        assert!(matches!(response, Response::InitOk { ws_id } if ws_id == format!("{base_id}-2")));
        let existing = state.get_by_wsid(&base_id).unwrap();
        assert_eq!(existing.read().await.path, detached_owner);
        assert_eq!(
            backend.init_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn init_skips_ws_id_reserved_only_by_active_index() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let backend = Arc::new(RecorderStubBackend {
            data_root_path: temp.path().join("mount/data"),
            snapshots_root_path: temp.path().join("mount/snapshots"),
            ..RecorderStubBackend::new()
        });
        let state = Arc::new(DaemonState::new(
            DaemonConfig {
                mount_path: temp.path().join("configured-mount"),
                ..test_config()
            },
            backend.clone(),
            temp.path().join("state"),
        ));
        let base_id = generate_ws_id_base(workspace.to_str().unwrap());
        let existing_index_dir = state.index_dir(&base_id);
        std::fs::create_dir_all(&existing_index_dir).unwrap();
        let existing_index = existing_index_dir.join(ws_ckpt_common::INDEX_FILE);
        std::fs::write(&existing_index, b"keep existing index").unwrap();

        let response = init(&state, workspace.to_str().unwrap()).await.unwrap();

        assert!(matches!(response, Response::InitOk { ws_id } if ws_id == format!("{base_id}-2")));
        assert_eq!(
            std::fs::read(&existing_index).unwrap(),
            b"keep existing index"
        );
        assert!(state.get_by_wsid(&base_id).is_none());
        assert_eq!(
            backend.init_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_checkpoint_waits_through_storage_migration() {
        use std::time::Duration;

        // Exercise both ordinary allocation and a pre-existing ID collision.
        for collision in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = temp.path().join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::write(workspace.join("payload"), b"keep me").unwrap();
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&workspace, &alias).unwrap();
            let backend = Arc::new(RecorderStubBackend {
                data_root_path: temp.path().join("mount/data"),
                snapshots_root_path: temp.path().join("mount/snapshots"),
                pause_migration: true,
                ..RecorderStubBackend::new()
            });
            let base_id = generate_ws_id_base(workspace.to_str().unwrap());
            if collision {
                std::fs::create_dir_all(backend.data_root().join(&base_id)).unwrap();
            }
            let expected_id = if collision {
                format!("{base_id}-2")
            } else {
                base_id.clone()
            };
            let config = DaemonConfig {
                mount_path: temp.path().join("configured-mount"),
                ..test_config()
            };
            let state = Arc::new(DaemonState::new(
                config,
                backend.clone(),
                temp.path().join("state"),
            ));
            let workspace_str = workspace.to_str().unwrap();
            let alias_str = alias.to_str().unwrap();
            let checkpoint_request = |path: &str, id: &str| ws_ckpt_common::Request::Checkpoint {
                workspace: path.to_string(),
                id: id.to_string(),
                message: None,
                metadata: None,
                pin: false,
            };
            let leader =
                crate::dispatcher::dispatch(&state, checkpoint_request(workspace_str, "leader"));
            tokio::pin!(leader);

            // Hold the first init after creating storage, before renaming.
            tokio::select! {
                response = &mut leader => panic!("init returned before migration: {response:?}"),
                permit = backend.init_progress.acquire() => permit.unwrap().forget(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("init never reached storage"),
            }
            let follower =
                crate::dispatcher::dispatch(&state, checkpoint_request(workspace_str, "follower"));
            tokio::pin!(follower);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut follower)
                    .await
                    .is_err()
            );
            assert_eq!(
                backend.init_calls.load(std::sync::atomic::Ordering::SeqCst),
                1
            );

            backend.init_continue.add_permits(1);
            tokio::select! {
                response = &mut leader => panic!("init returned before symlink: {response:?}"),
                permit = backend.init_progress.acquire() => permit.unwrap().forget(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("init never renamed workspace"),
            }
            assert!(!workspace.exists());
            // Both the original spelling and an alias must wait through the
            // missing-path window rather than reporting InvalidPath.
            let during_gap = crate::dispatcher::dispatch(
                &state,
                checkpoint_request(workspace_str, "during-gap"),
            );
            let via_alias =
                crate::dispatcher::dispatch(&state, checkpoint_request(alias_str, "via-alias"));
            tokio::pin!(during_gap, via_alias);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut during_gap)
                    .await
                    .is_err()
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut via_alias)
                    .await
                    .is_err()
            );
            backend.init_continue.add_permits(1);
            let responses = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(leader, follower, during_gap, via_alias)
            })
            .await
            .expect("concurrent init did not finish");
            for (response, expected_snapshot) in
                [responses.0, responses.1, responses.2, responses.3]
                    .into_iter()
                    .zip(["leader", "follower", "during-gap", "via-alias"])
            {
                assert!(
                    matches!(&response, Response::CheckpointOk { snapshot_id } if snapshot_id == expected_snapshot),
                    "{response:?}"
                );
                assert_eq!(
                    std::fs::read(
                        backend
                            .snapshots_root()
                            .join(&expected_id)
                            .join(expected_snapshot)
                            .join("payload")
                    )
                    .unwrap(),
                    b"keep me"
                );
            }
            let registered = state.get_by_wsid(&expected_id).unwrap();
            assert_eq!(registered.read().await.index.snapshots.len(), 4);
            assert_eq!(
                backend.init_calls.load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            assert!(state.get_by_path(&workspace).is_some());
            assert_eq!(
                std::fs::read(workspace.join("payload")).unwrap(),
                b"keep me"
            );
            assert!(
                !std::path::Path::new(&crate::backends::btrfs_common::backup_path_for(
                    workspace_str
                ))
                .exists()
            );
            assert!(state.index_dir(&expected_id).join("index.json").is_file());
            assert!(!backend.data_root().join(format!("{base_id}-3")).exists());
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
        state
            .register_workspace(
                "ws-gone".to_string(),
                canon.clone(),
                SnapshotIndex::new(canon),
            )
            .unwrap();
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
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            mount_path.clone(),
            mount_path.join("test.img"),
        ));
        let state = Arc::new(DaemonState::new(cfg, backend, test_state_dir()));
        let user_dir = tempfile::tempdir().unwrap();
        let user_path = user_dir.path().join("repo");
        std::os::unix::fs::symlink(&subvol_path, &user_path).unwrap();
        state
            .register_workspace(
                ws_id.to_string(),
                user_path.clone(),
                SnapshotIndex::new(user_path),
            )
            .unwrap();

        let resp = init(&state, &subvol_path.to_string_lossy()).await.unwrap();
        match resp {
            Response::InitOk { ws_id: returned } => assert_eq!(returned, ws_id),
            other => panic!("expected idempotent InitOk, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn init_btrfs_base_checks_data_root_outside_configured_mount() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(crate::backends::btrfs_base::BtrfsBaseBackend::new(
            temp.path().join("detected-mount"),
            crate::backends::btrfs_base::BtrfsBaseScenario::InPlace,
        ));
        let config = DaemonConfig {
            mount_path: temp.path().join("configured-mount"),
            ..test_config()
        };
        let state = Arc::new(DaemonState::new(
            config,
            backend.clone(),
            temp.path().join("state"),
        ));
        let subvol = backend.data_root().join("ws-existing");
        tokio::fs::create_dir_all(subvol.join("nested"))
            .await
            .unwrap();
        let workspace = temp.path().join("workspace");
        let alias = temp.path().join("alias");
        tokio::fs::symlink(&subvol, &workspace).await.unwrap();
        tokio::fs::symlink(&workspace, &alias).await.unwrap();
        state
            .register_workspace(
                "ws-existing".into(),
                workspace.clone(),
                SnapshotIndex::new(workspace.clone()),
            )
            .unwrap();
        for path in [&subvol, &alias] {
            let response = init(&state, path.to_str().unwrap()).await.unwrap();
            assert!(matches!(response, Response::InitOk { ws_id } if ws_id == "ws-existing"));
            let resolved = state
                .resolve_workspace(path.to_str().unwrap())
                .await
                .unwrap();
            assert_eq!(resolved.read().await.ws_id, "ws-existing");
        }
        let snapshots = backend.snapshots_root().to_path_buf();
        tokio::fs::create_dir_all(&snapshots).await.unwrap();
        for path in [
            backend.data_root().to_path_buf(),
            subvol.join("nested"),
            snapshots,
        ] {
            assert!(state
                .resolve_workspace(path.to_str().unwrap())
                .await
                .is_none());
            let response = init(&state, path.to_str().unwrap()).await.unwrap();
            assert!(
                matches!(
                    response,
                    Response::Error {
                        code: ErrorCode::InvalidPath,
                        ..
                    }
                ),
                "{response:?}"
            );
        }
        // An exact registered spelling retargeted to another workspace must
        // still fail its own detach guard, never checkpoint the other one.
        let other = backend.data_root().join("ws-other");
        tokio::fs::create_dir(&other).await.unwrap();
        let other_link = temp.path().join("other-link");
        tokio::fs::symlink(&other, &other_link).await.unwrap();
        state
            .register_workspace(
                "ws-other".into(),
                other_link.clone(),
                SnapshotIndex::new(other_link),
            )
            .unwrap();
        tokio::fs::remove_file(&workspace).await.unwrap();
        tokio::fs::symlink(&other, &workspace).await.unwrap();
        let resolved = state
            .resolve_workspace(workspace.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(resolved.read().await.ws_id, "ws-existing");
        assert!(state.detached_registration_error(&resolved).await.is_some());
        tokio::fs::remove_file(&workspace).await.unwrap();
        tokio::fs::symlink(&subvol, &workspace).await.unwrap();
        // Internal storage alone cannot supply a user-facing recovery anchor.
        let orphan = backend.data_root().join("ws-orphan");
        tokio::fs::create_dir_all(&orphan).await.unwrap();
        tokio::fs::create_dir_all(backend.snapshots_root().join("ws-orphan"))
            .await
            .unwrap();
        assert!(matches!(
            init(&state, orphan.to_str().unwrap()).await.unwrap(),
            Response::Error {
                code: ErrorCode::InvalidPath,
                ..
            }
        ));
        let orphan_link = temp.path().join("orphan-link");
        tokio::fs::symlink(&orphan, &orphan_link).await.unwrap();
        assert!(
            matches!(init(&state, orphan_link.to_str().unwrap()).await.unwrap(), Response::InitOk { ws_id } if ws_id == "ws-orphan")
        );
        assert_eq!(
            state.get_by_wsid("ws-orphan").unwrap().read().await.path,
            orphan_link
        );
        assert_eq!(tokio::fs::read_link(&workspace).await.unwrap(), subvol);
    }

    #[tokio::test]
    async fn adoption_preserves_owner_and_rejects_nested_storage() {
        for base in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let backend: Arc<dyn StorageBackend> = if base {
                Arc::new(crate::backends::btrfs_base::BtrfsBaseBackend::new(
                    temp.path().join("detected"),
                    crate::backends::btrfs_base::BtrfsBaseScenario::InPlace,
                ))
            } else {
                Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
                    temp.path().join("storage"),
                    temp.path().join("image"),
                ))
            };
            let state = Arc::new(DaemonState::new(
                test_config(),
                backend.clone(),
                temp.path().join("state"),
            ));
            let live = backend.data_root().join("ws-owned");
            std::fs::create_dir_all(live.join("child")).unwrap();
            std::fs::create_dir_all(backend.snapshots_root().join("ws-owned")).unwrap();
            let owner = temp.path().join("repo");
            let alias = temp.path().join("alias");
            let nested = temp.path().join("nested");
            std::os::unix::fs::symlink(&live, &owner).unwrap();
            std::os::unix::fs::symlink(&owner, &alias).unwrap();
            std::os::unix::fs::symlink(live.join("child"), &nested).unwrap();
            index_store::save(
                &state.index_dir("ws-owned"),
                &SnapshotIndex::new(owner.clone()),
            )
            .await
            .unwrap();
            let response = init(&state, nested.to_str().unwrap()).await.unwrap();
            assert!(
                matches!(
                    response,
                    Response::Error {
                        code: ErrorCode::InvalidPath,
                        ..
                    }
                ),
                "{response:?}"
            );
            assert!(state.all_workspaces().is_empty());
            let response = init(&state, alias.to_str().unwrap()).await.unwrap();
            assert!(matches!(response, Response::InitOk { .. }), "{response:?}");
            let ws = state.get_by_wsid("ws-owned").unwrap();
            assert_eq!(ws.read().await.path, owner);
            assert_eq!(ws.read().await.index.workspace_path, owner);
            // Re-adoption from a direct internal path is safe only when its
            // persisted index proves a live, external registration anchor.
            state.unregister_workspace("ws-owned").await;
            assert!(matches!(
                init(&state, live.to_str().unwrap()).await.unwrap(),
                Response::InitOk { .. }
            ));
            assert_eq!(
                state.get_by_wsid("ws-owned").unwrap().read().await.path,
                owner
            );
            state.unregister_workspace("ws-owned").await;
            std::fs::remove_file(&owner).unwrap();
            std::fs::create_dir(&owner).unwrap();
            let response = init(&state, live.to_str().unwrap()).await.unwrap();
            assert!(
                matches!(
                    response,
                    Response::Error {
                        code: ErrorCode::InvalidPath,
                        ..
                    }
                ),
                "{response:?}"
            );
            assert!(state.all_workspaces().is_empty());
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
                temp.path().join("data"),
                temp.path().join("test.img"),
            ));
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend,
            temp.path().join("state"),
        ));
        let subvol = state.backend.data_root().join(ws_id);
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
        state
            .register_workspace(ws_id.to_string(), ws_link.clone(), index)
            .unwrap();
        (state, ws_link, temp)
    }

    #[tokio::test]
    async fn delete_absent_subvolume_prunes_record_and_remains_not_found() {
        let (state, _, _temp) = live_topology("ws-del-absent");
        for _ in 0..2 {
            let response = delete_snapshot(&state, "ws-del-absent", "snap-1", false)
                .await
                .unwrap();
            assert!(matches!(
                response,
                Response::Error {
                    code: ErrorCode::SnapshotNotFound,
                    ..
                }
            ));
            let index = index_store::load(&state.index_dir("ws-del-absent"))
                .await
                .unwrap();
            assert!(index.snapshots.is_empty());
        }
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

    #[tokio::test]
    async fn orphan_recovery_preserves_complete_backup_and_partial_storage() {
        for base_backend in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let mount = tmp.path().join("mount");
            let backend: Arc<dyn StorageBackend> = if base_backend {
                Arc::new(crate::backends::btrfs_base::BtrfsBaseBackend::new(
                    mount.clone(),
                    crate::backends::btrfs_base::BtrfsBaseScenario::CrossDisk,
                ))
            } else {
                Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
                    mount.clone(),
                    tmp.path().join("image"),
                ))
            };
            let state = Arc::new(DaemonState::new(
                DaemonConfig {
                    mount_path: mount,
                    ..test_config()
                },
                backend,
                tmp.path().join("state"),
            ));
            let original = tmp.path().join("workspace");
            let original_str = original.to_str().unwrap();
            let backup = std::path::PathBuf::from(crate::backends::btrfs_common::backup_path_for(
                original_str,
            ));
            tokio::fs::create_dir(&backup).await.unwrap();
            tokio::fs::write(backup.join("complete"), b"complete original")
                .await
                .unwrap();
            // A suffix from a hash collision must not hide historical storage.
            let ws_id = format!("{}-2", generate_ws_id_base(original_str));
            let subvol = state.backend.data_root().join(&ws_id);
            tokio::fs::create_dir_all(&subvol).await.unwrap();
            tokio::fs::write(subvol.join("partial"), b"newer or incomplete")
                .await
                .unwrap();
            let snapshots = state.backend.snapshots_root().join(&ws_id);
            tokio::fs::create_dir_all(&snapshots).await.unwrap();
            tokio::fs::write(snapshots.join("snapshot"), b"keep")
                .await
                .unwrap();
            let error = init(&state, original_str).await.unwrap_err();
            assert!(error.to_string().contains("ws-ckpt recover"));
            // Public backend entry must reject before cleanup deletes old storage.
            assert!(state
                .backend
                .init_workspace(original_str, &ws_id)
                .await
                .is_err());
            assert!(subvol.join("partial").exists());
            let response = recover_workspace(&state, original_str).await.unwrap();
            match response {
                Response::RecoverWithWarning { warning, .. } => {
                    assert!(warning.contains(subvol.to_str().unwrap()))
                }
                other => panic!("expected recovery warning, got {:?}", other),
            }
            assert_eq!(
                tokio::fs::read(original.join("complete")).await.unwrap(),
                b"complete original"
            );
            assert_eq!(
                tokio::fs::read(subvol.join("partial")).await.unwrap(),
                b"newer or incomplete"
            );
            assert!(snapshots.join("snapshot").exists());
            assert!(!backup.exists());
        }
    }

    #[tokio::test]
    async fn init_restores_backup_before_canonicalizing_missing_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("workspace");
        let backup = tmp.path().join("workspace.pre-init-bak");
        tokio::fs::create_dir(&backup).await.unwrap();
        tokio::fs::write(backup.join("complete"), b"original")
            .await
            .unwrap();
        let backend = Arc::new(RecorderStubBackend::new());
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend,
            tmp.path().join("state"),
        ));
        let response = init(&state, original.to_str().unwrap()).await.unwrap();
        assert!(
            matches!(response, Response::InitOk { .. }),
            "{:?}",
            response
        );
        assert_eq!(
            tokio::fs::read(original.join("complete")).await.unwrap(),
            b"original"
        );
        assert!(!backup.exists());
    }

    #[tokio::test]
    async fn unregister_only_missing_storage_and_preserve_recovery_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("mount");
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            mount.clone(),
            tmp.path().join("image"),
        ));
        let state = Arc::new(DaemonState::new(
            DaemonConfig {
                mount_path: mount.clone(),
                ..test_config()
            },
            backend,
            tmp.path().join("state"),
        ));
        let original = tmp.path().join("workspace");
        let ws_id_owned = generate_ws_id_base(original.to_str().unwrap());
        let ws_id = ws_id_owned.as_str();
        let subvol = mount.join(ws_id);
        tokio::fs::create_dir_all(&subvol).await.unwrap();
        tokio::fs::symlink(&subvol, &original).await.unwrap();
        state
            .register_workspace(
                ws_id.to_string(),
                original.clone(),
                SnapshotIndex::new(original.clone()),
            )
            .unwrap();
        let index = state.index_dir(ws_id);
        tokio::fs::create_dir_all(&index).await.unwrap();
        tokio::fs::write(index.join("index.json"), b"metadata")
            .await
            .unwrap();
        let snapshots = state.backend.snapshots_root().join(ws_id);
        tokio::fs::create_dir_all(&snapshots).await.unwrap();
        tokio::fs::write(snapshots.join("keep"), b"snapshot")
            .await
            .unwrap();
        let response = unregister_missing_workspace(&state, ws_id).await.unwrap();
        assert!(matches!(
            response,
            Response::Error {
                code: ErrorCode::InvalidPath,
                ..
            }
        ));
        assert!(state.get_by_wsid(ws_id).is_some());
        tokio::fs::remove_dir(&subvol).await.unwrap();
        let error = recover_workspace(&state, ws_id).await.unwrap_err();
        assert!(format!("{error:#}").contains("ws-ckpt unregister"));
        assert!(
            tokio::fs::symlink_metadata(&original).await.is_ok(),
            "failed recover must preserve dangling symlink"
        );
        let response = unregister_missing_workspace(&state, ws_id).await.unwrap();
        assert!(matches!(response, Response::UnregisterOk { .. }));
        assert!(state.get_by_wsid(ws_id).is_none());
        assert!(tokio::fs::symlink_metadata(&original).await.is_err());
        assert!(!index.exists());
        assert_eq!(
            tokio::fs::read(
                index
                    .with_file_name(format!("{}.unregistered", ws_id))
                    .join("index.json")
            )
            .await
            .unwrap(),
            b"metadata"
        );
        assert_eq!(
            tokio::fs::read(snapshots.join("keep")).await.unwrap(),
            b"snapshot"
        );
        let manifest = tokio::fs::read_to_string(tmp.path().join("state/state.json"))
            .await
            .unwrap();
        assert!(!manifest.contains(ws_id));
        tokio::fs::create_dir(&original).await.unwrap();
        let reinit_backend = Arc::new(RecorderStubBackend {
            data_root_path: mount.clone(),
            snapshots_root_path: state.backend.snapshots_root().to_path_buf(),
            ..RecorderStubBackend::new()
        });
        let reinit_state = Arc::new(DaemonState::new(
            DaemonConfig {
                mount_path: mount,
                ..test_config()
            },
            reinit_backend,
            tmp.path().join("state"),
        ));
        let response = init(&reinit_state, original.to_str().unwrap())
            .await
            .unwrap();
        assert!(
            matches!(response, Response::InitOk { ws_id: new_id } if new_id == format!("{}-2", ws_id))
        );
        assert_eq!(
            tokio::fs::read(snapshots.join("keep")).await.unwrap(),
            b"snapshot"
        );
    }

    #[tokio::test]
    async fn unregister_manifest_failure_preserves_registration_and_allows_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("workspace");
        let mount = tmp.path().join("mount");
        let backend = Arc::new(crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            mount.clone(),
            tmp.path().join("image"),
        ));
        let state_dir = tmp.path().join("state");
        let state = Arc::new(DaemonState::new(
            DaemonConfig {
                mount_path: mount.clone(),
                ..test_config()
            },
            backend,
            state_dir.clone(),
        ));
        let ws_id = "ws-retry";
        state
            .register_workspace(
                ws_id.to_string(),
                original.clone(),
                SnapshotIndex::new(original.clone()),
            )
            .unwrap();
        let registered = state.get_by_wsid(ws_id).unwrap();
        registered.write().await.policy_failsafe = true;
        tokio::fs::symlink(mount.join(ws_id), &original)
            .await
            .unwrap();
        let index_dir = state.index_dir(ws_id);
        tokio::fs::create_dir_all(&index_dir).await.unwrap();
        tokio::fs::write(index_dir.join("policy.toml"), b"auto_cleanup = false")
            .await
            .unwrap();
        state.save_manifest().await.unwrap();
        let manifest = tokio::fs::read(state_dir.join("state.json")).await.unwrap();
        // Fail before atomic_write can rename the manifest into place.
        tokio::fs::create_dir(state_dir.join("state.json.tmp"))
            .await
            .unwrap();
        assert!(unregister_missing_workspace(&state, ws_id).await.is_err());
        let still_registered = state.get_by_wsid(ws_id).unwrap();
        assert!(Arc::ptr_eq(&registered, &still_registered));
        assert!(still_registered.read().await.policy_failsafe);
        assert!(state.get_by_path(&original).is_some());
        assert_eq!(
            tokio::fs::read(state_dir.join("state.json")).await.unwrap(),
            manifest
        );
        assert!(index_dir.join("policy.toml").exists());
        assert!(!index_dir.with_file_name("ws-retry.unregistered").exists());
        assert!(tokio::fs::symlink_metadata(&original).await.is_ok());
        tokio::fs::remove_dir(state_dir.join("state.json.tmp"))
            .await
            .unwrap();
        assert!(matches!(
            unregister_missing_workspace(&state, ws_id).await.unwrap(),
            Response::UnregisterOk { .. }
        ));
        assert!(state.get_by_wsid(ws_id).is_none());
        assert!(index_dir
            .with_file_name("ws-retry.unregistered")
            .join("policy.toml")
            .exists());
        assert!(!tokio::fs::read_to_string(state_dir.join("state.json"))
            .await
            .unwrap()
            .contains(ws_id));
    }

    #[tokio::test]
    async fn registered_recovery_archives_backup_and_allows_reinit_through_aliases() {
        for use_alias in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let parent = tmp.path().join("parent");
            tokio::fs::create_dir(&parent).await.unwrap();
            let original = parent.join("workspace");
            let alias = tmp.path().join("alias");
            tokio::fs::symlink(&parent, &alias).await.unwrap();
            let backend = Arc::new(RecorderStubBackend {
                data_root_path: tmp.path().join("data"),
                snapshots_root_path: tmp.path().join("snapshots"),
                restore_live_directory: true,
                ..RecorderStubBackend::new()
            });
            let ws_id = generate_ws_id_base(original.to_str().unwrap());
            let live = backend.data_root().join(&ws_id);
            tokio::fs::create_dir_all(&live).await.unwrap();
            tokio::fs::write(live.join("payload"), b"new live data")
                .await
                .unwrap();
            tokio::fs::symlink(&live, &original).await.unwrap();
            let backup = crate::backends::btrfs_common::backup_path_for(original.to_str().unwrap());
            tokio::fs::create_dir(&backup).await.unwrap();
            tokio::fs::write(PathBuf::from(&backup).join("payload"), b"old backup")
                .await
                .unwrap();
            // Existing archives, including dangling links and empty directories,
            // must never be overwritten by a later successful recovery.
            let occupied = format!("{backup}.recovered");
            tokio::fs::symlink(tmp.path().join("missing"), &occupied)
                .await
                .unwrap();
            tokio::fs::create_dir(format!("{backup}.recovered-1"))
                .await
                .unwrap();
            let state = Arc::new(DaemonState::new(
                test_config(),
                backend.clone(),
                tmp.path().join("state"),
            ));
            state
                .register_workspace(
                    ws_id.clone(),
                    original.clone(),
                    SnapshotIndex::new(original.clone()),
                )
                .unwrap();
            let input = if use_alias {
                alias.join("workspace")
            } else {
                original.clone()
            };
            let response = recover_workspace(&state, input.to_str().unwrap())
                .await
                .unwrap();
            let archive = format!("{backup}.recovered-2");
            assert!(
                matches!(response, Response::RecoverWithWarning { warning, .. } if warning.contains(&archive))
            );
            assert_eq!(
                backend.recover_call_count(),
                1,
                "aliases must use registered recovery"
            );
            assert!(state.get_by_wsid(&ws_id).is_none());
            assert_eq!(
                tokio::fs::read(original.join("payload")).await.unwrap(),
                b"new live data"
            );
            assert_eq!(
                tokio::fs::read(PathBuf::from(&archive).join("payload"))
                    .await
                    .unwrap(),
                b"old backup"
            );
            assert!(tokio::fs::read_link(&occupied).await.is_ok());
            assert!(!PathBuf::from(&backup).exists());
            assert!(matches!(
                init(&state, original.to_str().unwrap()).await.unwrap(),
                Response::InitOk { .. }
            ));
            assert_eq!(
                tokio::fs::read(original.join("payload")).await.unwrap(),
                b"new live data"
            );
        }
    }

    #[tokio::test]
    async fn unregister_reports_only_existing_recovery_material() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("workspace");
        let backend = Arc::new(RecorderStubBackend {
            data_root_path: tmp.path().join("data"),
            snapshots_root_path: tmp.path().join("snapshots"),
            ..RecorderStubBackend::new()
        });
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend,
            tmp.path().join("state"),
        ));
        let ws_id = "ws-no-index";
        state
            .register_workspace(
                ws_id.into(),
                original.clone(),
                SnapshotIndex::new(original.clone()),
            )
            .unwrap();
        let backup = crate::backends::btrfs_common::backup_path_for(original.to_str().unwrap());
        // A dangling backup link still occupies a retained filesystem entry.
        tokio::fs::symlink(tmp.path().join("missing"), &backup)
            .await
            .unwrap();
        let response = unregister_missing_workspace(&state, ws_id).await.unwrap();
        assert!(
            matches!(response, Response::UnregisterOk { retained_paths, .. } if retained_paths == vec![backup])
        );
        assert!(state.get_by_wsid(ws_id).is_none());
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
        state
            .register_workspace(
                ws_id.to_string(),
                canon.clone(),
                SnapshotIndex::new(canon.clone()),
            )
            .unwrap();
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
    async fn confirmed_recovery_rechecks_deletion_scope_and_pins_identity() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(RecorderStubBackend {
            data_root_path: temp.path().join("data"),
            snapshots_root_path: temp.path().join("snapshots"),
            ..RecorderStubBackend::new()
        });
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend.clone(),
            temp.path().join("state"),
        ));
        let owner = temp.path().join("repo");
        let alias = temp.path().join("alias");
        std::fs::create_dir_all(backend.data_root().join("ws-A")).unwrap();
        std::os::unix::fs::symlink(backend.data_root().join("ws-A"), &owner).unwrap();
        std::os::unix::fs::symlink(&owner, &alias).unwrap();
        state
            .register_workspace(
                "ws-A".into(),
                owner.clone(),
                SnapshotIndex::new(owner.clone()),
            )
            .unwrap();
        let Response::RecoverPreviewOk { preview } =
            crate::recover_preview::preview(&state, alias.to_str().unwrap())
                .await
                .unwrap()
        else {
            panic!("preview")
        };
        assert_eq!(preview.ws_id.as_deref(), Some("ws-A"));
        assert_eq!(preview.registration_path, owner.to_str().unwrap());
        // Recover deletes unindexed physical snapshots too: the confirmation
        // must expire if that deletion scope grows while the user is deciding.
        let physical = backend.snapshots_root().join("ws-A/unindexed");
        std::fs::create_dir_all(&physical).unwrap();
        assert!(matches!(
            recover_workspace_confirmed(&state, &preview).await.unwrap(),
            Response::Error {
                code: ErrorCode::ConfirmationRequired,
                ..
            }
        ));
        assert_eq!(backend.recover_call_count(), 0);
        let Response::RecoverPreviewOk { preview } =
            crate::recover_preview::preview(&state, alias.to_str().unwrap())
                .await
                .unwrap()
        else {
            panic!("preview")
        };
        assert_eq!(preview.snapshot_count, 1);
        // The user-facing alias can change; execution still names the confirmed ID.
        std::fs::create_dir_all(backend.data_root().join("ws-B")).unwrap();
        let other = temp.path().join("other");
        std::os::unix::fs::symlink(backend.data_root().join("ws-B"), &other).unwrap();
        state
            .register_workspace("ws-B".into(), other.clone(), SnapshotIndex::new(other))
            .unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(backend.data_root().join("ws-B"), &alias).unwrap();
        assert!(
            matches!(recover_workspace_confirmed(&state, &preview).await.unwrap(), Response::RecoverOk { workspace } if workspace == owner.to_str().unwrap())
        );
        assert_eq!(backend.recover_call_count(), 1);
        assert!(state.get_by_wsid("ws-A").is_none());
        assert!(state.get_by_wsid("ws-B").is_some());
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
        state
            .register_workspace(
                ws_id.clone(),
                canon.clone(),
                SnapshotIndex::new(canon.clone()),
            )
            .unwrap();

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
        init_calls: std::sync::atomic::AtomicUsize,
        pause_migration: bool,
        restore_live_directory: bool,
        init_progress: tokio::sync::Semaphore,
        init_continue: tokio::sync::Semaphore,
    }

    impl RecorderStubBackend {
        fn new() -> Self {
            Self {
                data_root_path: PathBuf::from("/tmp/stub-data"),
                snapshots_root_path: PathBuf::from("/tmp/stub-snapshots"),
                recover_calls: std::sync::atomic::AtomicUsize::new(0),
                init_calls: std::sync::atomic::AtomicUsize::new(0),
                pause_migration: false,
                restore_live_directory: false,
                init_progress: tokio::sync::Semaphore::new(0),
                init_continue: tokio::sync::Semaphore::new(0),
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
        async fn recover_workspace(&self, ws_id: &str, original: &str) -> anyhow::Result<()> {
            self.recover_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.restore_live_directory {
                tokio::fs::remove_file(original).await?;
                tokio::fs::rename(self.data_root().join(ws_id), original).await?;
            }
            Ok(())
        }
        async fn init_workspace(
            &self,
            original_path: &str,
            ws_id: &str,
        ) -> anyhow::Result<ws_ckpt_common::WorkspaceInfo> {
            self.init_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(std::path::Path::new(original_path).is_dir());
            if self.pause_migration {
                let subvol = self.data_root().join(ws_id);
                tokio::fs::create_dir_all(&subvol).await?;
                tokio::fs::create_dir_all(self.snapshots_root().join(ws_id)).await?;
                self.init_progress.add_permits(1);
                self.init_continue.acquire().await?.forget();
                let backup = crate::backends::btrfs_common::backup_path_for(original_path);
                tokio::fs::rename(original_path, &backup).await?;
                self.init_progress.add_permits(1);
                self.init_continue.acquire().await?.forget();
                tokio::fs::copy(
                    std::path::Path::new(&backup).join("payload"),
                    subvol.join("payload"),
                )
                .await?;
                tokio::fs::symlink(&subvol, original_path).await?;
                tokio::fs::remove_dir_all(backup).await?;
            }
            Ok(ws_ckpt_common::WorkspaceInfo {
                ws_id: ws_id.to_string(),
                path: original_path.to_string(),
                snapshot_count: 0,
            })
        }
        async fn create_snapshot(&self, ws_id: &str, snapshot_id: &str) -> anyhow::Result<()> {
            let snapshot = self.snapshots_root().join(ws_id).join(snapshot_id);
            tokio::fs::create_dir(&snapshot).await?;
            tokio::fs::copy(
                self.data_root().join(ws_id).join("payload"),
                snapshot.join("payload"),
            )
            .await?;
            Ok(())
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
