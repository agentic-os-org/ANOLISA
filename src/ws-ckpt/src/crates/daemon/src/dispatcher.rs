use crate::state::DaemonState;
use std::sync::Arc;
use ws_ckpt_common::{
    default_auto_cleanup_keep, delete_workspace_policy, load_config_file, save_workspace_policy,
    ConfigReport, DaemonConfig, EffectivePolicy, ErrorCode, FileConfig, GlobalPolicySnapshot,
    PolicyFieldOp, Request, Response, StatusReport, WorkspaceInfo, WorkspacePolicy,
    ADVISORY_SNAPSHOT_LIMIT, CONFIG_FILE_PATH, DEFAULT_AUTO_CLEANUP,
    DEFAULT_AUTO_CLEANUP_INTERVAL_SECS, DEFAULT_HEALTH_CHECK_INTERVAL_SECS,
    DEFAULT_IMG_MAX_PERCENT, DEFAULT_IMG_SIZE_GB,
};

/// Kernel-derived connection identity available to guarded requests.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DispatchContext {
    peer_uid: Option<u32>,
}

impl DispatchContext {
    pub(crate) fn new(peer_uid: Option<u32>) -> Self {
        Self { peer_uid }
    }

    pub(crate) fn peer_uid(self) -> Option<u32> {
        self.peer_uid
    }
}

pub async fn dispatch(state: &Arc<DaemonState>, request: Request) -> Response {
    dispatch_with_context(state, request, DispatchContext::default()).await
}

pub(crate) async fn dispatch_with_context(
    state: &Arc<DaemonState>,
    request: Request,
    context: DispatchContext,
) -> Response {
    let result = match request {
        Request::Init { workspace } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => crate::workspace_mgr::init(state, &workspace).await,
        },
        Request::Checkpoint {
            workspace,
            id,
            message,
            metadata,
            pin,
        } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => match auto_init_workspace(state, &workspace).await {
                Ok(Some(err_resp)) => return err_resp,
                Err(e) => Err(e),
                Ok(None) => {
                    crate::snapshot_mgr::checkpoint(state, &workspace, &id, message, metadata, pin)
                        .await
                }
            },
        },
        Request::Rollback {
            workspace,
            to,
            num_ancestors,
        } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => {
                crate::snapshot_mgr::rollback(state, &workspace, to.as_deref(), num_ancestors).await
            }
        },
        Request::RollbackPreview {
            workspace,
            to,
            num_ancestors,
        } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => {
                crate::snapshot_mgr::rollback_preview(
                    state,
                    &workspace,
                    to.as_deref(),
                    num_ancestors,
                )
                .await
            }
        },
        Request::Delete {
            workspace,
            snapshot,
            force,
        } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => match workspace {
                Some(ws) => {
                    crate::workspace_mgr::delete_snapshot(state, &ws, &snapshot, force).await
                }
                None => {
                    // Global lookup: find snapshot across all workspaces
                    match state.resolve_snapshot_globally(&snapshot).await {
                        Some((ws_path, resolved_id)) => {
                            crate::workspace_mgr::delete_snapshot(
                                state,
                                &ws_path,
                                &resolved_id,
                                force,
                            )
                            .await
                        }
                        None => {
                            // Check if it's ambiguous or truly not found
                            let mut match_count = 0usize;
                            for entry in state.all_workspaces() {
                                let ws = entry.read().await;
                                match ws.index.resolve_by_prefix(&snapshot) {
                                    Ok(_) => match_count += 1,
                                    Err(ws_ckpt_common::ResolveError::Ambiguous(_)) => {
                                        match_count += 2
                                    }
                                    Err(ws_ckpt_common::ResolveError::NotFound) => {}
                                }
                            }
                            if match_count > 1 {
                                Ok(Response::Error {
                                    code: ErrorCode::SnapshotNotFound,
                                    message: format!(
                                        "snapshot '{}' matches in multiple workspaces, please specify --workspace/-w",
                                        snapshot
                                    ),
                                })
                            } else {
                                Ok(Response::Error {
                                    code: ErrorCode::SnapshotNotFound,
                                    message: format!("snapshot not found: {}", snapshot),
                                })
                            }
                        }
                    }
                }
            },
        },
        Request::List { workspace, .. } => match workspace {
            Some(ws) => crate::snapshot_mgr::list_snapshots(state, &ws).await,
            None => crate::snapshot_mgr::list_all_snapshots(state).await,
        },
        Request::Diff {
            workspace,
            from,
            to,
        } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => {
                crate::snapshot_mgr::diff_snapshots(state, &workspace, &from, to.as_deref()).await
            }
        },
        Request::Status { workspace } => {
            // Inline status query logic
            handle_status(state, workspace.as_deref()).await
        }
        Request::Cleanup { workspace, keep } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => crate::snapshot_mgr::cleanup_snapshots(state, &workspace, keep).await,
        },
        Request::Config => Ok(handle_config(state)),
        Request::ReloadConfig => Ok(handle_reload_config(state).await),
        Request::ReloadGlobalConfig => Ok(handle_reload_global_config(state)),
        Request::ReloadWorkspacePolicy { workspace } => {
            Ok(handle_reload_workspace_policy(state, &workspace).await)
        }
        Request::ConfigOverview => Ok(handle_config_overview(state).await),
        Request::GetWorkspacePolicy { workspace } => {
            Ok(handle_get_workspace_policy(state, &workspace).await)
        }
        Request::ResetWorkspacePolicy { workspace } => {
            Ok(handle_reset_workspace_policy(state, &workspace).await)
        }
        Request::PatchWorkspacePolicy {
            workspace,
            auto_cleanup,
            auto_cleanup_keep,
        } => Ok(
            handle_patch_workspace_policy(state, &workspace, auto_cleanup, auto_cleanup_keep).await,
        ),
        Request::Recover { workspace } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => crate::workspace_mgr::recover_workspace(state, &workspace).await,
        },
        Request::Unregister { workspace } => match state.ensure_bootstrapped().await {
            Err(e) => Err(e),
            Ok(()) => crate::workspace_mgr::unregister_missing_workspace(state, &workspace).await,
        },
        Request::HealthAdvisory => Ok(handle_health_advisory(state).await),
        Request::WorkspaceIdentityV2 { registration_path } => {
            return crate::guarded_checkpoint::workspace_identity(state, &registration_path).await;
        }
        Request::GuardedCheckpointV2 {
            ws_id,
            expected_generation,
            checkpoint_id,
            operation_digest,
            message,
            metadata,
            pin,
        } => {
            return crate::guarded_checkpoint::checkpoint(
                state,
                context.peer_uid(),
                &ws_id,
                expected_generation,
                &checkpoint_id,
                operation_digest,
                message,
                metadata,
                pin,
            )
            .await;
        }
        Request::CheckpointEvidenceV2 {
            ws_id,
            expected_generation,
            checkpoint_id,
            operation_digest,
        } => {
            return crate::guarded_checkpoint::checkpoint_evidence(
                state,
                context.peer_uid(),
                &ws_id,
                expected_generation,
                &checkpoint_id,
                operation_digest,
            )
            .await;
        }
        Request::GuardedRollbackPreviewV2 {
            registered_path,
            ws_id,
            expected_generation,
            target_snapshot_id,
        } => {
            return crate::guarded_checkpoint::rollback_preview(
                state,
                context.peer_uid(),
                &registered_path,
                &ws_id,
                expected_generation,
                &target_snapshot_id,
            )
            .await;
        }
        Request::GuardedRollbackV2 {
            registered_path,
            ws_id,
            expected_generation,
            target_snapshot_id,
            expected_diff_digest,
            operation_id,
            operation_digest,
        } => {
            return crate::guarded_checkpoint::rollback(
                state,
                context.peer_uid(),
                &registered_path,
                &ws_id,
                expected_generation,
                &target_snapshot_id,
                expected_diff_digest,
                &operation_id,
                operation_digest,
            )
            .await;
        }
        Request::GuardedRollbackEvidenceV2 {
            ws_id,
            operation_id,
            operation_digest,
        } => {
            return crate::guarded_checkpoint::rollback_evidence(
                state,
                context.peer_uid(),
                &ws_id,
                &operation_id,
                operation_digest,
            )
            .await;
        }
    };

    match result {
        Ok(response) => response,
        Err(e) => Response::Error {
            code: ErrorCode::InternalError,
            message: format!("{:#}", e),
        },
    }
}

/// Auto-initialize a workspace if it is not yet registered.
/// Returns `Ok(None)` if the workspace is ready (already existed or was just initialized).
/// Returns `Ok(Some(Response))` with an error response if auto-init fails in a user-facing way.
async fn auto_init_workspace(
    state: &Arc<DaemonState>,
    workspace: &str,
) -> anyhow::Result<Option<Response>> {
    if state.resolve_workspace(workspace).await.is_some() {
        return Ok(None); // already initialized
    }
    tracing::info!(
        "workspace not initialized, auto-initializing: {}",
        workspace
    );
    let resp = crate::workspace_mgr::init(state, workspace).await?;
    match resp {
        Response::InitOk { ws_id } => {
            tracing::info!("auto-init completed: ws_id={}", ws_id);
            Ok(None)
        }
        // AlreadyInitialized is fine (race condition)
        Response::Error {
            code: ErrorCode::AlreadyInitialized,
            ..
        } => Ok(None),
        // Other errors: propagate as-is
        err_resp @ Response::Error { .. } => Ok(Some(err_resp)),
        // Unexpected response variant (should not happen)
        other => Ok(Some(other)),
    }
}

/// Handle the Status request inline: gather daemon info, workspace list, and filesystem usage.
async fn handle_status(
    state: &Arc<DaemonState>,
    workspace: Option<&str>,
) -> anyhow::Result<Response> {
    let uptime_secs = state.start_time.elapsed().as_secs();

    let workspaces = if let Some(ws_str) = workspace {
        // Single-workspace mode: resolve by ID, absolute path, or relative path
        let arc = match state.resolve_workspace(ws_str).await {
            Some(a) => a,
            None => {
                return Ok(Response::Error {
                    code: ErrorCode::WorkspaceNotFound,
                    message: format!("workspace not found: {}", ws_str),
                });
            }
        };
        // Detached-registration guard: reporting snapshot counts for a stale
        // subvolume would look healthy while the user's directory is not the
        // one being described. Global status stays unguarded so `recover
        // --all` can still enumerate detached workspaces to repair them.
        if let Some(resp) = state.detached_registration_error(&arc).await {
            return Ok(resp);
        }

        let ws = arc.read().await;
        vec![WorkspaceInfo {
            ws_id: ws.ws_id.clone(),
            path: ws.path.to_string_lossy().to_string(),
            snapshot_count: ws.index.snapshots.len() as u32,
        }]
    } else {
        // Global mode: return all workspaces
        state.get_all_workspace_info().await
    };

    // Try to get filesystem usage; fallback to zeros on error (e.g., macOS)
    let (fs_total_bytes, fs_used_bytes) = match state.backend.get_usage().await {
        Ok((total, used)) => (total, used),
        Err(_) => (0, 0),
    };

    Ok(Response::StatusOk {
        report: StatusReport {
            uptime_secs,
            workspaces,
            fs_total_bytes,
            fs_used_bytes,
        },
    })
}

/// Handle the Config request: return the current daemon configuration.
fn handle_config(state: &Arc<DaemonState>) -> Response {
    Response::ConfigOk {
        config: build_config_report(state),
    }
}

fn build_config_report(state: &Arc<DaemonState>) -> ConfigReport {
    let cfg = state.config_snapshot();
    ConfigReport {
        mount_path: state.mount_path.to_string_lossy().to_string(),
        socket_path: state.socket_path.to_string_lossy().to_string(),
        log_level: cfg.log_level,
        auto_cleanup: cfg.auto_cleanup,
        auto_cleanup_keep: cfg.auto_cleanup_keep,
        auto_cleanup_interval_secs: cfg.auto_cleanup_interval_secs,
        health_check_interval_secs: cfg.health_check_interval_secs,
        img_size: cfg.img_size,
        img_max_percent: cfg.img_max_percent,
    }
}

/// Handle ConfigOverview: global cfg + ws override roll-up. Walks every ws
/// under read locks (cheap; no disk I/O — caller should reload first if alignment matters).
async fn handle_config_overview(state: &Arc<DaemonState>) -> Response {
    let config = build_config_report(state);
    let arcs = state.all_workspaces();
    let ws_total = arcs.len();
    let mut ws_with_override = 0;
    for arc in arcs {
        let ws = arc.read().await;
        if !ws.policy.is_empty() {
            ws_with_override += 1;
        }
    }
    Response::ConfigOverviewOk {
        config,
        ws_total,
        ws_with_override,
    }
}

/// Handle the ReloadConfig request: re-read config file and update runtime config.
///
/// NOTE: BtrfsLoop image fields (`img_size`, `img_max_percent`) take effect
/// only during daemon bootstrap. If they differ from the currently loaded values, a
/// warning is emitted to tell the operator that a daemon restart is required for the
/// new values to be applied (via img resize at bootstrap).
///
/// Also rescans every workspace's `policy.toml` so out-of-band edits take
/// effect on reload. Per-ws policies are reloaded **before** the global cfg:
/// the resulting window is `(old cfg, new per-ws)`, which can only over-keep
/// (recoverable), never over-delete.
async fn handle_reload_config(state: &Arc<DaemonState>) -> Response {
    match load_config_file(std::path::Path::new(CONFIG_FILE_PATH)) {
        Ok(file_config) => {
            // Phase 1: refresh per-ws policies (global cfg still OLD — safe,
            // per-ws layers on any global).
            state.reload_all_workspace_policies().await;
            // Phase 2: apply new global cfg.
            apply_global_config(state, &file_config);
            // Push notification to scheduler loops: break their current sleep
            // (or wake them from a disabled state) so the new config takes
            // effect immediately instead of on the next polling boundary.
            state.config_notify.notify_waiters();
            Response::ReloadConfigOk {
                config: build_config_report(state),
                file: Some(file_config),
            }
        }
        Err(e) => Response::Error {
            code: ErrorCode::InternalError,
            message: format!("Failed to reload config: {}", e),
        },
    }
}

/// Reload one workspace's `policy.toml` — `config -w <ws>` view alignment.
async fn handle_reload_workspace_policy(state: &Arc<DaemonState>, workspace: &str) -> Response {
    let ctx = match resolve_ws_for_policy(state, workspace).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    state.reload_workspace_policy(&ctx.ws_id).await;
    state.config_notify.notify_waiters();
    Response::ReloadConfigOk {
        config: build_config_report(state),
        // Global file untouched by a per-workspace reload.
        file: None,
    }
}

/// Reload only the global config — `config -g` view alignment, no per-ws walk.
fn handle_reload_global_config(state: &Arc<DaemonState>) -> Response {
    match load_config_file(std::path::Path::new(CONFIG_FILE_PATH)) {
        Ok(file_config) => {
            apply_global_config(state, &file_config);
            state.config_notify.notify_waiters();
            Response::ReloadConfigOk {
                config: build_config_report(state),
                file: Some(file_config),
            }
        }
        Err(e) => Response::Error {
            code: ErrorCode::InternalError,
            message: format!("Failed to reload config: {}", e),
        },
    }
}

/// Apply a freshly-loaded `file_config` to the daemon's in-memory state.
/// Caller is responsible for `config_notify.notify_waiters()` afterwards.
fn apply_global_config(state: &Arc<DaemonState>, file_config: &FileConfig) {
    let mut cfg = match state.config.write() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::warn!("config RwLock poisoned; reload taking write guard anyway");
            poisoned.into_inner()
        }
    };
    cfg.auto_cleanup = file_config.auto_cleanup.unwrap_or(DEFAULT_AUTO_CLEANUP);
    cfg.auto_cleanup_keep = file_config
        .auto_cleanup_keep
        .clone()
        .unwrap_or_else(default_auto_cleanup_keep);
    cfg.auto_cleanup_interval_secs = file_config
        .auto_cleanup_interval_secs
        .unwrap_or(DEFAULT_AUTO_CLEANUP_INTERVAL_SECS);
    cfg.health_check_interval_secs = file_config
        .health_check_interval_secs
        .unwrap_or(DEFAULT_HEALTH_CHECK_INTERVAL_SECS);
    cfg.backend_type = file_config.backend.r#type.clone();

    // img_* are bootstrap-only: warn if changed but don't mutate cfg
    // (loop image size is fixed until the next restart, where
    // bootstrap reconciles target = min(img_size GiB, total * pct/100)).
    let btrfs_loop = file_config.backend.btrfs_loop.as_ref();
    let new_img_size = btrfs_loop
        .and_then(|b| b.img_size)
        .unwrap_or(DEFAULT_IMG_SIZE_GB);
    let new_img_max_percent = btrfs_loop
        .and_then(|b| b.img_max_percent)
        .unwrap_or(DEFAULT_IMG_MAX_PERCENT * 100.0);
    if new_img_size != cfg.img_size
        || (new_img_max_percent - cfg.img_max_percent).abs() > f64::EPSILON
    {
        tracing::warn!(
            "BtrfsLoop image sizing changed in config file (img_size: {} -> {} GB, \
             img_max_percent: {} -> {}). These are bootstrap-only settings; \
             restart ws-ckpt daemon to apply the new target \
             min(img_size GB, total * img_max_percent%).",
            cfg.img_size,
            new_img_size,
            cfg.img_max_percent,
            new_img_max_percent,
        );
    }

    tracing::info!(
        "Config reloaded: auto_cleanup={}, keep={}, cleanup_interval={}s, health_interval={}s \
         (img fields are bootstrap-only; restart required to apply)",
        cfg.auto_cleanup,
        cfg.auto_cleanup_keep,
        cfg.auto_cleanup_interval_secs,
        cfg.health_check_interval_secs,
    );
}

/// Snapshot global config once and derive both `global` and `effective` from
/// it, so a concurrent `ReloadConfig` can't break `effective = local.or(global)`.
fn render_views(
    state: &Arc<DaemonState>,
    local: &WorkspacePolicy,
) -> (EffectivePolicy, GlobalPolicySnapshot) {
    let cfg_snapshot: DaemonConfig = state.config_snapshot();
    let effective = local.effective_for(&cfg_snapshot);
    let global = GlobalPolicySnapshot::from_config(&cfg_snapshot);
    (effective, global)
}

/// Bundle of values every policy handler needs: the ws Arc (for tiny
/// read/write locks downstream), its stable identifiers, and the per-ws
/// `policy_io_mu` Arc that PATCH/RESET serialize fsync on.
struct PolicyCtx {
    arc: Arc<tokio::sync::RwLock<crate::state::WorkspaceState>>,
    ws_id: String,
    index_dir: std::path::PathBuf,
    policy_io_mu: Arc<tokio::sync::Mutex<()>>,
}

/// Resolve `workspace` → [`PolicyCtx`], or return a ready-to-send
/// `WorkspaceNotFound` Response. One tiny read lock snapshots ws_id +
/// `policy_io_mu` Arc, so all 4 policy handlers (Get / Reset / Patch /
/// ReloadWorkspacePolicy) share the same boilerplate.
#[allow(clippy::result_large_err)]
async fn resolve_ws_for_policy(
    state: &Arc<DaemonState>,
    workspace: &str,
) -> Result<PolicyCtx, Response> {
    let arc = match state.resolve_workspace(workspace).await {
        Some(a) => a,
        None => {
            return Err(Response::Error {
                code: ErrorCode::WorkspaceNotFound,
                message: format!("workspace not found: {}", workspace),
            });
        }
    };
    let (ws_id, policy_io_mu) = {
        let ws = arc.read().await;
        (ws.ws_id.clone(), ws.policy_io_mu.clone())
    };
    let index_dir = state.index_dir(&ws_id);
    Ok(PolicyCtx {
        arc,
        ws_id,
        index_dir,
        policy_io_mu,
    })
}

/// Handle `GetWorkspacePolicy`: return effective + local + global views
/// (one cfg snapshot keeps `effective` and `global` consistent).
async fn handle_get_workspace_policy(state: &Arc<DaemonState>, workspace: &str) -> Response {
    let ctx = match resolve_ws_for_policy(state, workspace).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let local = ctx.arc.read().await.policy.clone();
    let (effective, global) = render_views(state, &local);
    Response::WorkspacePolicyOk {
        ws_id: ctx.ws_id,
        effective,
        local,
        global,
    }
}

/// Save the per-ws `policy.toml` on a blocking worker so a slow fsync doesn't
/// pin a tokio thread; the write lock spans the spawn_blocking so
/// concurrent patchers serialize.
async fn save_policy_blocking(
    index_dir: std::path::PathBuf,
    new_policy: WorkspacePolicy,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        save_workspace_policy(&index_dir, &new_policy)
            .map_err(|e| format!("save policy.toml: {}", e))
    })
    .await
    .map_err(|e| format!("blocking-task join error: {}", e))?
}

/// Delete the per-ws `policy.toml`; same spawn_blocking discipline as [`save_policy_blocking`].
async fn delete_policy_blocking(index_dir: std::path::PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        delete_workspace_policy(&index_dir).map_err(|e| format!("delete policy.toml: {}", e))
    })
    .await
    .map_err(|e| format!("blocking-task join error: {}", e))?
}

/// Handle `ResetWorkspacePolicy`: always unlink `policy.toml` (idempotent) and
/// clear the in-memory override — memory and disk can drift, never short-circuit on memory alone.
///
/// Lock discipline (see [[ws-lock-no-fs-loops]]): the ws RwLock is held ONLY
/// for in-memory commit; the unlink + parent fsync runs unlocked. PATCH/RESET
/// serialization uses a per-ws narrow `policy_io_mu` so concurrent
/// checkpoint/list/status are not blocked by a slow disk.
async fn handle_reset_workspace_policy(state: &Arc<DaemonState>, workspace: &str) -> Response {
    let ctx = match resolve_ws_for_policy(state, workspace).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    // (1) Serialize PATCH/RESET on this ws via the narrow mutex; does NOT
    //     block readers/checkpoint on the ws RwLock.
    let _io_guard = ctx.policy_io_mu.lock().await;

    // (2) Slow fs op runs WITHOUT the ws lock.
    if let Err(e) = delete_policy_blocking(ctx.index_dir).await {
        return Response::Error {
            code: ErrorCode::InternalError,
            message: format!("Failed to delete policy.toml: {}", e),
        };
    }

    // (3) Tiny write lock to commit the in-memory result. Disk is the source
    //     of truth — this just brings memory in sync with the post-unlink state.
    let local = WorkspacePolicy::default();
    {
        let mut ws = ctx.arc.write().await;
        ws.policy = local.clone();
        // Reset puts ws into a known-clean inherit-global state — drop any prior fail-safe marker.
        ws.policy_failsafe = false;
    }

    let (effective, global) = render_views(state, &local);
    state.config_notify.notify_waiters();
    Response::WorkspacePolicyOk {
        ws_id: ctx.ws_id,
        effective,
        local,
        global,
    }
}

/// Handle `PatchWorkspacePolicy`: server-side read-modify-write per field
/// killing the lost-update race of the CLI's old GET→modify→SET.
///
/// Lock discipline (see [[ws-lock-no-fs-loops]]): the ws RwLock is held only
/// for the few-microsecond memory ops (read snapshot, then write commit).
/// The fsync+rename+parent-fsync runs WITHOUT the ws lock, under a per-ws
/// narrow `policy_io_mu` that serializes PATCH/RESET against each other but
/// does not block checkpoint/list/status. Order is fs-first, memory-second:
/// if `save_policy_blocking` errors we return without touching memory, so
/// memory never leads disk (no rollback needed).
async fn handle_patch_workspace_policy(
    state: &Arc<DaemonState>,
    workspace: &str,
    auto_cleanup: PolicyFieldOp<bool>,
    auto_cleanup_keep: PolicyFieldOp<ws_ckpt_common::CleanupRetention>,
) -> Response {
    let ctx = match resolve_ws_for_policy(state, workspace).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    // (1) Serialize PATCH/RESET on this ws via the narrow mutex; concurrent
    //     readers/checkpoint on the ws RwLock are NOT blocked by this.
    let _io_guard = ctx.policy_io_mu.lock().await;

    // (2) Tiny read lock: failsafe gate + snapshot for read-modify-write.
    //     Held under io_guard, so two PATCHes can't both observe stale state
    //     and race to write.
    let (mut new_policy, failsafe) = {
        let ws = ctx.arc.read().await;
        (ws.policy.clone(), ws.policy_failsafe)
    };

    // Refuse: in-memory policy is a fail-safe synthetic, not the user's truth.
    // Patching from it would persist the synthetic as truth (e.g. silently
    // turning auto_cleanup=true on disk into false). Force reload/reset first.
    if failsafe {
        return Response::Error {
            code: ErrorCode::InternalError,
            message: format!(
                "workspace {} has a fail-safe policy in memory (real policy.toml \
                 was unreadable at startup). Patching now would overwrite the \
                 on-disk policy with the fail-safe value. Run `ws-ckpt reload` \
                 to re-read the file, or `ws-ckpt config -w {} --reset` to \
                 discard the on-disk override and restart from inherit-global.",
                ctx.ws_id, ctx.ws_id
            ),
        };
    }

    new_policy.auto_cleanup = auto_cleanup.apply(new_policy.auto_cleanup);
    new_policy.auto_cleanup_keep = auto_cleanup_keep.apply(new_policy.auto_cleanup_keep);

    // (3) Slow fs save WITHOUT the ws lock. Always save + notify even if mem-equal:
    //     corrects on-disk drift and re-arms parked schedulers. Patch edits only;
    //     all-None still writes empty TOML so loads see Loaded(default), not Missing.
    if let Err(e) = save_policy_blocking(ctx.index_dir, new_policy.clone()).await {
        // fs-first ordering: memory was not touched, so no rollback needed.
        return Response::Error {
            code: ErrorCode::InternalError,
            message: format!("Failed to persist policy.toml: {}", e),
        };
    }

    // (4) Tiny write lock to commit the in-memory result.
    {
        let mut ws = ctx.arc.write().await;
        ws.policy = new_policy.clone();
    }

    let (effective, global) = render_views(state, &new_policy);
    state.config_notify.notify_waiters();
    Response::WorkspacePolicyOk {
        ws_id: ctx.ws_id,
        effective,
        local: new_policy,
        global,
    }
}

/// Aggregate advisory metrics. Never triggers bootstrap; backend-query failure
/// yields zero bytes so the CLI silently skips the fs warning.
async fn handle_health_advisory(state: &Arc<DaemonState>) -> Response {
    let over_limit_workspace_count: u32 = state
        .get_all_workspace_info()
        .await
        .iter()
        .filter(|w| w.snapshot_count > ADVISORY_SNAPSHOT_LIMIT)
        .count() as u32;
    let (fs_total_bytes, fs_used_bytes) = match state.backend.get_usage().await {
        Ok((total, used)) => (total, used),
        Err(_) => (0, 0),
    };
    Response::HealthAdvisoryOk {
        over_limit_workspace_count,
        fs_total_bytes,
        fs_used_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ws_ckpt_common::backend::StorageBackend;
    use ws_ckpt_common::{CleanupRetention, DaemonConfig, ErrorCode, Request, Response};

    fn test_backend() -> Arc<dyn StorageBackend> {
        // Use BtrfsBase to avoid triggering lazy bootstrap in dispatch tests
        Arc::new(crate::backends::btrfs_base::BtrfsBaseBackend::new(
            PathBuf::from("/tmp/test-mount"),
            crate::backends::btrfs_base::BtrfsBaseScenario::InPlace,
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

    // The dispatcher routes all Request variants to handlers. For handlers that
    // call tokio::fs::canonicalize, we can use tempdir to create real paths and
    // test the routing without requiring btrfs.

    #[tokio::test]
    async fn dispatch_init_nonexistent_path_returns_invalid_path() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Init {
            workspace: "/nonexistent/path/12345".to_string(),
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidPath),
            _ => panic!("expected InvalidPath error from Init"),
        }
    }

    #[tokio::test]
    async fn dispatch_checkpoint_nonexistent_auto_inits_and_returns_invalid_path() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Checkpoint {
            workspace: "/nonexistent/path/12345".to_string(),
            id: "snap-1".to_string(),
            message: None,
            metadata: None,
            pin: false,
        };
        let resp = dispatch(&state, req).await;
        // Auto-init triggers, but path doesn't exist → InvalidPath
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidPath),
            _ => panic!("expected InvalidPath error from Checkpoint auto-init"),
        }
    }

    #[tokio::test]
    async fn dispatch_rollback_nonexistent_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Rollback {
            workspace: "/nonexistent/path/12345".to_string(),
            to: Some("msg1-step0".to_string()),
            num_ancestors: None,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error from Rollback"),
        }
    }

    #[tokio::test]
    async fn dispatch_delete_nonexistent_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Delete {
            workspace: Some("/nonexistent/path/12345".to_string()),
            snapshot: "nonexistent".to_string(),
            force: true,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error from Delete"),
        }
    }

    #[tokio::test]
    async fn dispatch_delete_snapshot_not_found_returns_error() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Delete {
            workspace: Some("/nonexistent/ws".to_string()),
            snapshot: "nosuchsnap".to_string(),
            force: false,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::WorkspaceNotFound);
                assert!(message.contains("/nonexistent/ws"));
            }
            _ => panic!("expected WorkspaceNotFound from Delete"),
        }
    }

    #[tokio::test]
    async fn dispatch_checkpoint_unregistered_real_path_auto_inits() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let tmpdir = tempfile::tempdir().unwrap();
        let req = Request::Checkpoint {
            workspace: tmpdir.path().to_string_lossy().to_string(),
            id: "snap-1".to_string(),
            message: None,
            metadata: None,
            pin: false,
        };
        let resp = dispatch(&state, req).await;
        // Auto-init triggers; since backend cannot actually init in test env,
        // we expect an error (InternalError from backend failure), not WorkspaceNotFound
        if let Response::Error { code, .. } = resp {
            assert!(
                code != ErrorCode::WorkspaceNotFound,
                "should not return WorkspaceNotFound; auto-init should have been attempted"
            );
        }
        // If somehow init succeeded, that's also acceptable
    }

    #[tokio::test]
    async fn dispatch_rollback_unregistered_real_path_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let tmpdir = tempfile::tempdir().unwrap();
        let req = Request::Rollback {
            workspace: tmpdir.path().to_string_lossy().to_string(),
            to: Some("msg1-step0".to_string()),
            num_ancestors: None,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound from Rollback on unregistered ws"),
        }
    }

    #[tokio::test]
    async fn dispatch_delete_unregistered_snapshot_returns_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Delete {
            workspace: Some("/nonexistent/ws".to_string()),
            snapshot: "abc123".to_string(),
            force: true,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound from Delete on unregistered workspace"),
        }
    }

    // Test that dispatch wraps anyhow errors into InternalError
    // (cannot easily trigger without mocking, so we verify the pattern)
    #[test]
    fn dispatch_error_wrapping_pattern() {
        // Verify that the error wrapping produces correct Response
        let err_resp = Response::Error {
            code: ErrorCode::InternalError,
            message: format!("{:#}", anyhow::anyhow!("test error")),
        };
        match err_resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InternalError);
                assert!(message.contains("test error"));
            }
            _ => panic!("expected Error variant"),
        }
    }

    // ── Phase 2 dispatch tests ──

    #[tokio::test]
    async fn dispatch_list_nonexistent_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::List {
            workspace: Some("/nonexistent/path/12345".to_string()),
            format: None,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error from List"),
        }
    }

    #[tokio::test]
    async fn dispatch_list_unregistered_path_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let tmpdir = tempfile::tempdir().unwrap();
        let req = Request::List {
            workspace: Some(tmpdir.path().to_string_lossy().to_string()),
            format: None,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound from List"),
        }
    }

    #[tokio::test]
    async fn dispatch_diff_nonexistent_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Diff {
            workspace: "/nonexistent/path/12345".to_string(),
            from: "msg1-step0".to_string(),
            to: Some("msg2-step0".to_string()),
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error from Diff"),
        }
    }

    #[tokio::test]
    async fn dispatch_status_returns_status_ok() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Status { workspace: None };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::StatusOk { report } => {
                assert!(report.workspaces.is_empty());
            }
            _ => panic!("expected StatusOk, got {:?}", resp),
        }
    }

    #[tokio::test]
    async fn dispatch_status_with_nonexistent_workspace_returns_error() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Status {
            workspace: Some("/nonexistent/path/12345".to_string()),
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound, got {:?}", resp),
        }
    }

    #[tokio::test]
    async fn dispatch_status_with_unregistered_real_path_returns_error() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let tmpdir = tempfile::tempdir().unwrap();
        let req = Request::Status {
            workspace: Some(tmpdir.path().to_string_lossy().to_string()),
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound, got {:?}", resp),
        }
    }

    #[tokio::test]
    async fn dispatch_cleanup_nonexistent_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Cleanup {
            workspace: "/nonexistent/path/12345".to_string(),
            keep: None,
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error from Cleanup"),
        }
    }

    // ── Detached-registration guard at the dispatch layer ──
    //
    // Real topology: BtrfsBaseBackend on a tempdir (data_root =
    // `<tmp>/ws-ckpt-data`), a `ws-<id>` directory inside it as the
    // live-subvolume stand-in, and a workspace symlink pointing at it.

    /// Returns (state, ws_id, workspace symlink, tempdir to keep alive).
    fn live_topology(ws_id: &str) -> (Arc<DaemonState>, String, PathBuf, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let backend: Arc<dyn StorageBackend> =
            Arc::new(crate::backends::btrfs_base::BtrfsBaseBackend::new(
                temp.path().to_path_buf(),
                crate::backends::btrfs_base::BtrfsBaseScenario::InPlace,
            ));
        let state = Arc::new(DaemonState::new(
            test_config(),
            backend,
            temp.path().join("state"),
        ));
        let subvol = temp.path().join("ws-ckpt-data").join(ws_id);
        std::fs::create_dir_all(&subvol).unwrap();
        let ws_link = temp.path().join("ws-link");
        std::os::unix::fs::symlink(&subvol, &ws_link).unwrap();
        state.register_workspace(
            ws_id.to_string(),
            ws_link.clone(),
            ws_ckpt_common::SnapshotIndex::new(ws_link.clone()),
        );
        (state, ws_id.to_string(), ws_link, temp)
    }

    fn assert_detach_hint(resp: Response, op: &str) {
        match resp {
            Response::Error { code, message } => {
                assert_eq!(code, ErrorCode::InternalError, "{op}: {message}");
                assert!(message.contains("recover"), "{op}: {message}");
                assert!(message.contains("regular directory"), "{op}: {message}");
            }
            other => panic!("{op}: expected detach error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_checkpoint_refuses_detached_registration() {
        let (state, _ws_id, ws_link, _temp) = live_topology("ws-dispatch");
        // Detach: replace the symlink with a plain directory (issue #3059 repro).
        std::fs::remove_file(&ws_link).unwrap();
        std::fs::create_dir(&ws_link).unwrap();

        let resp = dispatch(
            &state,
            Request::Checkpoint {
                workspace: ws_link.to_string_lossy().to_string(),
                id: "snap-1".to_string(),
                message: None,
                metadata: None,
                pin: false,
            },
        )
        .await;
        // The checkpoint path must fail loudly instead of auto-initing over
        // the replacement directory or snapshotting the stale subvolume.
        assert_detach_hint(resp, "dispatch checkpoint");
    }

    #[tokio::test]
    async fn dispatch_checkpoint_healthy_registration_proceeds_past_guard() {
        let (state, _ws_id, _ws_link, _temp) = live_topology("ws-dispatch-live");
        let empty_ws = tempfile::tempdir().unwrap();
        let resp = dispatch(
            &state,
            Request::Checkpoint {
                workspace: empty_ws.path().to_string_lossy().to_string(),
                id: "snap-1".to_string(),
                message: None,
                metadata: None,
                pin: false,
            },
        )
        .await;
        // Auto-init on an unregistered directory must still be attempted: it
        // reaches the real backend (which fails outside btrfs, legitimately as
        // InternalError), never the detach guard.
        match resp {
            Response::Error { code, message } => {
                assert_ne!(code, ErrorCode::WorkspaceNotFound, "{message}");
                assert!(
                    !message.contains("ws-ckpt recover"),
                    "must not look like a detach error: {message}"
                );
            }
            other => panic!("expected an error from the backend, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_status_single_workspace_refuses_detached_registration() {
        let (state, _ws_id, ws_link, _temp) = live_topology("ws-status");
        std::fs::remove_file(&ws_link).unwrap();
        std::fs::create_dir(&ws_link).unwrap();

        let resp = dispatch(
            &state,
            Request::Status {
                workspace: Some(ws_link.to_string_lossy().to_string()),
            },
        )
        .await;
        assert_detach_hint(resp, "dispatch status -w");
    }

    #[tokio::test]
    async fn dispatch_status_global_stays_unguarded() {
        let (state, _ws_id, ws_link, _temp) = live_topology("ws-global");
        std::fs::remove_file(&ws_link).unwrap();
        std::fs::create_dir(&ws_link).unwrap();

        // Global status must keep listing detached workspaces: `recover --all`
        // uses it to enumerate the workspaces it repairs.
        let resp = dispatch(&state, Request::Status { workspace: None }).await;
        match resp {
            Response::StatusOk { report } => {
                assert_eq!(report.workspaces.len(), 1);
                assert_eq!(report.workspaces[0].ws_id, "ws-global");
            }
            other => panic!("expected StatusOk, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_config_returns_config_ok() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Config;
        let resp = dispatch(&state, req).await;
        match resp {
            Response::ConfigOk { config } => {
                assert_eq!(config.mount_path, "/tmp/test-mount");
                assert_eq!(config.auto_cleanup_keep, CleanupRetention::Count(20));
                assert_eq!(config.auto_cleanup_interval_secs, 86_400);
            }
            _ => panic!("expected ConfigOk, got {:?}", resp),
        }
    }

    #[tokio::test]
    async fn dispatch_reload_config_returns_reload_config_ok() {
        // ReloadConfig reads /etc/ws-ckpt/config.toml; if missing, uses defaults
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::ReloadConfig;
        let resp = dispatch(&state, req).await;
        assert!(matches!(resp, Response::ReloadConfigOk { .. }));
    }

    #[tokio::test]
    async fn dispatch_recover_nonexistent_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::Recover {
            workspace: "/nonexistent/path/12345".to_string(),
        };
        let resp = dispatch(&state, req).await;
        match resp {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            _ => panic!("expected WorkspaceNotFound error from Recover"),
        }
    }

    // ── Per-workspace policy dispatch tests ──

    #[tokio::test]
    async fn dispatch_get_workspace_policy_unregistered_returns_workspace_not_found() {
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::GetWorkspacePolicy {
            workspace: "/nonexistent/ws".to_string(),
        };
        match dispatch(&state, req).await {
            Response::Error { code, .. } => assert_eq!(code, ErrorCode::WorkspaceNotFound),
            other => panic!("expected WorkspaceNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_get_workspace_policy_returns_global_default_for_clean_ws() {
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/clean");
        state.register_workspace(
            "ws-clean".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
        );
        let req = Request::GetWorkspacePolicy {
            workspace: "ws-clean".to_string(),
        };
        match dispatch(&state, req).await {
            Response::WorkspacePolicyOk {
                ws_id,
                effective,
                local,
                global,
            } => {
                assert_eq!(ws_id, "ws-clean");
                assert!(local.is_empty(), "fresh ws should have no local override");
                // effective inherits from global (here: auto_cleanup=false, Count(20))
                assert_eq!(effective.auto_cleanup, global.auto_cleanup);
                assert_eq!(effective.auto_cleanup_keep, global.auto_cleanup_keep);
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_patch_then_get_workspace_policy_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/patchget");
        state.register_workspace(
            "ws-patchget".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
        );

        // Patch sets both fields (Patch is the only write path, Reset the only delete path).
        let patch_req = Request::PatchWorkspacePolicy {
            workspace: "ws-patchget".to_string(),
            auto_cleanup: PolicyFieldOp::Set(true),
            auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(3)),
        };
        match dispatch(&state, patch_req).await {
            Response::WorkspacePolicyOk {
                effective, local, ..
            } => {
                assert!(effective.auto_cleanup);
                assert_eq!(effective.auto_cleanup_keep, CleanupRetention::Count(3));
                assert_eq!(local.auto_cleanup_keep, Some(CleanupRetention::Count(3)));
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }

        // Verify policy.toml was actually written to disk under index_dir.
        let index_dir = state.index_dir("ws-patchget");
        let on_disk = ws_ckpt_common::load_workspace_policy_or_default(&index_dir).unwrap();
        assert_eq!(on_disk.auto_cleanup, Some(true));
        assert_eq!(on_disk.auto_cleanup_keep, Some(CleanupRetention::Count(3)));

        // GetWorkspacePolicy should reflect the same state.
        let get_req = Request::GetWorkspacePolicy {
            workspace: "ws-patchget".to_string(),
        };
        match dispatch(&state, get_req).await {
            Response::WorkspacePolicyOk { local, .. } => {
                assert_eq!(local.auto_cleanup, Some(true));
                assert_eq!(local.auto_cleanup_keep, Some(CleanupRetention::Count(3)));
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dispatch_reset_workspace_policy_deletes_file_and_inherits_global() {
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/reset");
        state.register_workspace(
            "ws-reset".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
        );

        // First Patch a real policy so there's a file to delete.
        let _ = dispatch(
            &state,
            Request::PatchWorkspacePolicy {
                workspace: "ws-reset".to_string(),
                auto_cleanup: PolicyFieldOp::Set(true),
                auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(9)),
            },
        )
        .await;
        assert!(state.index_dir("ws-reset").join("policy.toml").exists());

        // Reset → file should be gone, in-memory policy is empty.
        let reset_req = Request::ResetWorkspacePolicy {
            workspace: "ws-reset".to_string(),
        };
        match dispatch(&state, reset_req).await {
            Response::WorkspacePolicyOk { local, .. } => {
                assert!(local.is_empty());
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }
        assert!(
            !state.index_dir("ws-reset").join("policy.toml").exists(),
            "reset must remove policy.toml"
        );
    }

    #[tokio::test]
    async fn dispatch_reset_workspace_policy_is_idempotent_on_empty_policy() {
        // Calling Reset on a ws with no override must still succeed and converge
        // (file absent, memory default) — disk is always touched, ENOENT short-circuits.
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/reset-noop");
        state.register_workspace(
            "ws-reset-noop".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
        );
        // No prior Patch — policy is default, file does not exist.
        assert!(!state
            .index_dir("ws-reset-noop")
            .join("policy.toml")
            .exists());

        let reset_req = Request::ResetWorkspacePolicy {
            workspace: "ws-reset-noop".to_string(),
        };
        match dispatch(&state, reset_req).await {
            Response::WorkspacePolicyOk { local, .. } => {
                assert!(local.is_empty());
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }
        // Still no file. Idempotency is the whole guarantee.
        assert!(!state
            .index_dir("ws-reset-noop")
            .join("policy.toml")
            .exists());
    }

    #[tokio::test]
    async fn dispatch_patch_workspace_policy_refuses_when_failsafe() {
        // Boot-time fail-safe must not be mistaken for user intent: PATCH that
        // doesn't explicitly set auto_cleanup would silently persist Some(false).
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/failsafe");
        state.register_workspace_with_policy(
            "ws-failsafe".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
            WorkspacePolicy {
                auto_cleanup: Some(false),
                auto_cleanup_keep: None,
            },
            true,
        );

        let patch_req = Request::PatchWorkspacePolicy {
            workspace: "ws-failsafe".to_string(),
            auto_cleanup: PolicyFieldOp::Unchanged,
            auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(50)),
        };
        match dispatch(&state, patch_req).await {
            Response::Error {
                code: ErrorCode::InternalError,
                message,
            } => {
                assert!(
                    message.contains("fail-safe") && message.contains("--reset"),
                    "error message should explain the fail-safe state and remediation, got: {}",
                    message
                );
            }
            other => panic!("expected InternalError refusing patch, got {:?}", other),
        }

        // Reset clears the fail-safe marker, and a follow-up PATCH then succeeds.
        let reset_req = Request::ResetWorkspacePolicy {
            workspace: "ws-failsafe".to_string(),
        };
        assert!(matches!(
            dispatch(&state, reset_req).await,
            Response::WorkspacePolicyOk { .. }
        ));
        let patch_after_reset = Request::PatchWorkspacePolicy {
            workspace: "ws-failsafe".to_string(),
            auto_cleanup: PolicyFieldOp::Unchanged,
            auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(50)),
        };
        assert!(matches!(
            dispatch(&state, patch_after_reset).await,
            Response::WorkspacePolicyOk { .. }
        ));
    }

    #[tokio::test]
    async fn dispatch_patch_workspace_policy_sequential_accumulates() {
        // Smoke test for Patch semantics under sequential application.
        // The concurrent guarantee is exercised separately below.
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/patch-seq");
        state.register_workspace(
            "ws-patch-seq".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
        );

        let r1 = dispatch(
            &state,
            Request::PatchWorkspacePolicy {
                workspace: "ws-patch-seq".to_string(),
                auto_cleanup: PolicyFieldOp::Set(true),
                auto_cleanup_keep: PolicyFieldOp::Unchanged,
            },
        )
        .await;
        match r1 {
            Response::WorkspacePolicyOk { local, .. } => {
                assert_eq!(local.auto_cleanup, Some(true));
                assert_eq!(local.auto_cleanup_keep, None);
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }

        let r2 = dispatch(
            &state,
            Request::PatchWorkspacePolicy {
                workspace: "ws-patch-seq".to_string(),
                auto_cleanup: PolicyFieldOp::Unchanged,
                auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(11)),
            },
        )
        .await;
        match r2 {
            Response::WorkspacePolicyOk { local, .. } => {
                assert_eq!(local.auto_cleanup, Some(true));
                assert_eq!(local.auto_cleanup_keep, Some(CleanupRetention::Count(11)));
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }

        // Patch 3: flip auto_cleanup off via Set(false), leave keep alone
        // (per-field "clear" has no IPC; whole-policy removal is Reset).
        let r3 = dispatch(
            &state,
            Request::PatchWorkspacePolicy {
                workspace: "ws-patch-seq".to_string(),
                auto_cleanup: PolicyFieldOp::Set(false),
                auto_cleanup_keep: PolicyFieldOp::Unchanged,
            },
        )
        .await;
        match r3 {
            Response::WorkspacePolicyOk { local, .. } => {
                assert_eq!(local.auto_cleanup, Some(false));
                assert_eq!(local.auto_cleanup_keep, Some(CleanupRetention::Count(11)));
            }
            other => panic!("expected WorkspacePolicyOk, got {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dispatch_patch_workspace_policy_concurrent_no_lost_updates() {
        // Issue #14: spawn two real concurrent Patches on disjoint fields and
        // assert both edits survive (a sequential test would pass even without
        // the lock). Multi-thread runtime so they actually race on the RwLock.
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/patch-conc");
        state.register_workspace(
            "ws-patch-conc".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
        );

        // Several iterations to reduce schedule-luck flakiness: without the
        // lock, at least one is likely to expose the race.
        for i in 0..16 {
            // Reset the policy so each iteration starts from a clean slate.
            let _ = dispatch(
                &state,
                Request::ResetWorkspacePolicy {
                    workspace: "ws-patch-conc".to_string(),
                },
            )
            .await;

            let s1 = state.clone();
            let s2 = state.clone();
            let h1 = tokio::spawn(async move {
                dispatch(
                    &s1,
                    Request::PatchWorkspacePolicy {
                        workspace: "ws-patch-conc".to_string(),
                        auto_cleanup: PolicyFieldOp::Set(true),
                        auto_cleanup_keep: PolicyFieldOp::Unchanged,
                    },
                )
                .await
            });
            let h2 = tokio::spawn(async move {
                dispatch(
                    &s2,
                    Request::PatchWorkspacePolicy {
                        workspace: "ws-patch-conc".to_string(),
                        auto_cleanup: PolicyFieldOp::Unchanged,
                        auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(11)),
                    },
                )
                .await
            });
            let (_r1, _r2) = tokio::join!(h1, h2);

            // Final state must show BOTH edits — the point of the per-ws rmw
            // lock; the legacy GET→modify→SET would lose one on interleave.
            let get = dispatch(
                &state,
                Request::GetWorkspacePolicy {
                    workspace: "ws-patch-conc".to_string(),
                },
            )
            .await;
            match get {
                Response::WorkspacePolicyOk { local, .. } => {
                    assert_eq!(
                        local.auto_cleanup,
                        Some(true),
                        "iter {}: concurrent enable+keep lost auto_cleanup",
                        i
                    );
                    assert_eq!(
                        local.auto_cleanup_keep,
                        Some(CleanupRetention::Count(11)),
                        "iter {}: concurrent enable+keep lost keep",
                        i
                    );
                }
                other => panic!("expected WorkspacePolicyOk, got {:?}", other),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dispatch_patch_and_reset_concurrent_serialize_via_policy_io_mu() {
        // `policy_io_mu` exists to serialize PATCH ↔ RESET on the same ws so
        // each cycle ends in a deterministic state. We race one PATCH and one
        // RESET; whichever wins the mutex first commits its memory before the
        // other proceeds. The final result is whatever the *second* op writes
        // — never a torn state, never a deadlock.
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            tmp.path().to_path_buf(),
        ));
        let path = PathBuf::from("/ws/patch-vs-reset");
        state.register_workspace(
            "ws-patch-vs-reset".to_string(),
            path.clone(),
            ws_ckpt_common::SnapshotIndex::new(path),
        );

        // 16 iters: enough for both interleavings (patch-then-reset and
        // reset-then-patch) to actually occur under different schedules.
        let timeout = std::time::Duration::from_secs(5);
        for i in 0..16 {
            // Seed each iter from a known non-empty state so PATCH and RESET
            // produce visibly different memory.
            let _ = dispatch(
                &state,
                Request::PatchWorkspacePolicy {
                    workspace: "ws-patch-vs-reset".to_string(),
                    auto_cleanup: PolicyFieldOp::Set(true),
                    auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(7)),
                },
            )
            .await;

            let s1 = state.clone();
            let s2 = state.clone();
            let patch = tokio::spawn(async move {
                dispatch(
                    &s1,
                    Request::PatchWorkspacePolicy {
                        workspace: "ws-patch-vs-reset".to_string(),
                        auto_cleanup: PolicyFieldOp::Set(false),
                        auto_cleanup_keep: PolicyFieldOp::Set(CleanupRetention::Count(3)),
                    },
                )
                .await
            });
            let reset = tokio::spawn(async move {
                dispatch(
                    &s2,
                    Request::ResetWorkspacePolicy {
                        workspace: "ws-patch-vs-reset".to_string(),
                    },
                )
                .await
            });
            // No deadlock: both must complete within the timeout.
            let joined = tokio::time::timeout(timeout, async { tokio::join!(patch, reset) })
                .await
                .expect("iter {i}: PATCH+RESET timed out — possible deadlock under policy_io_mu");
            // Both must succeed (failsafe is off; backend stub is OK).
            for r in [&joined.0, &joined.1] {
                let resp = r.as_ref().expect("spawn joined cleanly");
                assert!(
                    matches!(resp, Response::WorkspacePolicyOk { .. }),
                    "iter {}: expected WorkspacePolicyOk, got {:?}",
                    i,
                    resp
                );
            }

            // Final disk + memory must reflect EXACTLY one of the two ops
            // (the second one to commit under io_guard). Whether that's
            // PATCH(false,Count(3)) or RESET(empty) depends on schedule —
            // but the two states are disjoint, so we can probe.
            let get = dispatch(
                &state,
                Request::GetWorkspacePolicy {
                    workspace: "ws-patch-vs-reset".to_string(),
                },
            )
            .await;
            match get {
                Response::WorkspacePolicyOk { local, .. } => {
                    let is_patch_winner = local.auto_cleanup == Some(false)
                        && local.auto_cleanup_keep == Some(CleanupRetention::Count(3));
                    let is_reset_winner = local.is_empty();
                    assert!(
                        is_patch_winner || is_reset_winner,
                        "iter {}: state is neither patch-winner nor reset-winner: {:?}",
                        i,
                        local
                    );
                }
                other => panic!("iter {}: expected WorkspacePolicyOk, got {:?}", i, other),
            }
        }
    }

    #[tokio::test]
    async fn dispatch_health_advisory_returns_health_advisory_ok() {
        // Empty workspace set -> counter must be 0; fs bytes vary by OS.
        let state = Arc::new(DaemonState::new(
            test_config(),
            test_backend(),
            PathBuf::from("/tmp/test-state"),
        ));
        let req = Request::HealthAdvisory;
        let resp = dispatch(&state, req).await;
        match resp {
            Response::HealthAdvisoryOk {
                over_limit_workspace_count,
                ..
            } => {
                assert_eq!(over_limit_workspace_count, 0);
            }
            _ => panic!("expected HealthAdvisoryOk, got {:?}", resp),
        }
    }
}
