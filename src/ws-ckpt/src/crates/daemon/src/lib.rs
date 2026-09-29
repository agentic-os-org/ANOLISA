pub mod backend_detect;
pub mod backends;
pub mod dispatcher;
pub mod fs_watcher;
mod guarded_checkpoint;
pub mod index_store;
pub mod listener;
mod lockfile;
pub mod ops_log;
mod recover_preview;
pub mod scheduler;
#[cfg(target_os = "linux")]
pub mod seccomp;
pub mod snapshot_mgr;
mod startup;
pub mod state;
mod util;
pub mod workspace_mgr;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use ws_ckpt_common::{DaemonConfig, DEFAULT_STATE_DIR, INDEXES_DIR, LOCKFILE_NAME};

pub async fn run_daemon(config: DaemonConfig) -> anyhow::Result<()> {
    // 0. Require root privileges
    if !nix::unistd::geteuid().is_root() {
        anyhow::bail!(
            "ws-ckpt daemon must be run as root (mount, losetup, btrfs commands require root privileges)"
        );
    }

    // 1. Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&config.log_level))
        .init();

    info!("ws-ckpt daemon starting...");

    // 2. Create state_dir（fixed to DEFAULT_STATE_DIR）
    let state_dir = PathBuf::from(DEFAULT_STATE_DIR);
    tokio::fs::create_dir_all(&state_dir)
        .await
        .with_context(|| format!("Failed to create state directory: {:?}", state_dir))?;
    tokio::fs::create_dir_all(state_dir.join(INDEXES_DIR))
        .await
        .with_context(|| {
            format!(
                "Failed to create indexes directory: {:?}",
                state_dir.join(INDEXES_DIR)
            )
        })?;

    // 3. Lockfile crash detection
    let lockfile_path = state_dir.join(LOCKFILE_NAME);
    let lockfile_holder = lockfile::acquire(&lockfile_path)?;

    // 4. Resolve startup state (load state.json, create backend, bootstrap, rebuild)
    let state = startup::resolve_state(&config, &state_dir).await?;

    // 6. Save initial state
    if let Err(e) = state.save_manifest().await {
        warn!("Failed to save initial state.json: {:#}", e);
    }

    // 7. Re-establish symlinks lost during daemon restart
    util::ensure_symlinks(&state).await;

    // 8. Apply seccomp-bpf syscall filter (after bootstrap, before listener)
    #[cfg(target_os = "linux")]
    if let Err(e) = seccomp::apply_seccomp_filter() {
        tracing::warn!(
            "Failed to apply seccomp filter: {:#}. Continuing without syscall filtering.",
            e
        );
    }

    // 9. Start background scheduler
    scheduler::start_scheduler(state.clone());

    // 10. Create cancellation token
    let cancel = CancellationToken::new();

    // 11. Register signal handlers
    let mut sigterm = signal(SignalKind::terminate())?;

    // SIGHUP no-op handler
    match signal(SignalKind::hangup()) {
        Ok(mut sighup) => {
            tokio::spawn(async move {
                loop {
                    sighup.recv().await;
                    tracing::info!(
                        "Received SIGHUP; no-op (use `systemctl reload ws-ckpt` or \
                         `ws-ckpt reload` to reload config)"
                    );
                }
            });
        }
        Err(e) => {
            tracing::warn!("Failed to install SIGHUP handler: {}", e);
        }
    }

    // 12. Spawn listener
    let listener_cancel = cancel.clone();
    let listener_state = Arc::clone(&state);
    let listener_handle =
        tokio::spawn(async move { listener::run_listener(listener_state, listener_cancel).await });

    // 13. Wait for shutdown signal
    tokio::select! {
        _ = sigterm.recv() => {
            info!("Received SIGTERM, shutting down...");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT (Ctrl+C), shutting down...");
        }
    }

    cancel.cancel();

    // 14. Wait for listener to finish
    if let Err(e) = listener_handle.await {
        tracing::error!("Listener task panicked: {}", e);
    }

    // 15. Flush all workspace index.json files
    info!("Flushing workspace indexes...");
    let all_ws = state.all_workspaces();
    for ws in &all_ws {
        let ws_guard = ws.read().await;
        let ws_dir = state.index_dir(&ws_guard.ws_id);
        if let Err(e) = tokio::fs::create_dir_all(&ws_dir).await {
            tracing::error!("Failed to create index directory {:?}: {}", ws_dir, e);
            continue;
        }
        if let Err(e) = index_store::save(&ws_dir, &ws_guard.index).await {
            tracing::error!("Failed to save index for {}: {:#}", ws_guard.ws_id, e);
        }
    }

    // 16. Save final state
    if let Err(e) = state.save_manifest().await {
        tracing::error!("Failed to save final state.json: {:#}", e);
    }

    // 17. Remove lockfile (clean exit marker)
    drop(lockfile_holder);
    let _ = std::fs::remove_file(&lockfile_path);

    info!("daemon shutdown complete");
    Ok(())
}
