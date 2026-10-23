//! Daemon startup path: load persisted state or perform fresh detection/migration.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tracing::{info, warn};

use ws_ckpt_common::persist;
use ws_ckpt_common::DaemonConfig;

use crate::backend_detect;
use crate::state::DaemonState;

/// Resolve the daemon state at startup.
///
/// Restores state.json or performs first-install detection when it is absent.
/// Unreadable state must not silently select a different storage root.
pub(crate) async fn resolve_state(
    config: &DaemonConfig,
    state_dir: &Path,
) -> anyhow::Result<Arc<DaemonState>> {
    let persisted = persist::load_state(state_dir).with_context(|| {
        format!("Failed to load {:?}; refusing fresh backend detection. Repair or explicitly reset the state file before restarting", state_dir.join(ws_ckpt_common::STATE_FILE))
    })?;

    // 2. Determine startup path according to state.json existence
    let state: Arc<DaemonState> = if let Some(ref state_file) = persisted {
        resolve_from_persisted(config, state_dir, state_file).await?
    } else {
        resolve_fresh(config, state_dir).await?
    };

    Ok(state)
}

/// Restore daemon state from an existing state.json.
async fn resolve_from_persisted(
    config: &DaemonConfig,
    state_dir: &Path,
    state_file: &ws_ckpt_common::persist::DaemonStateFile,
) -> anyhow::Result<Arc<DaemonState>> {
    // Determine the final backend type: config override vs persisted
    let (effective_backend_type, selection_method) = if let Some(config_type) =
        config.parse_backend_type()
    {
        if config_type != state_file.backend.backend_type {
            warn!(
                "Config overrides persisted backend_type: {:?} -> {:?}. Previous backend snapshots \
                 are not adopted; startup reconciliation can prune unpinned index records under {:?}. \
                 Switching back does not recover pruned index records.",
                state_file.backend.backend_type, config_type, state_dir
            );
        }
        (config_type, "config-override")
    } else {
        // config is "auto" → keep the record in state.json
        (state_file.backend.backend_type, "persisted")
    };

    info!(
        "Restoring from persisted state (backend={:?})",
        effective_backend_type
    );

    let persisted_btrfs_root = if effective_backend_type
        == ws_ckpt_common::backend::BackendType::BtrfsBase
        && effective_backend_type == state_file.backend.backend_type
    {
        match &state_file.paths {
            persist::BackendPaths::BtrfsBase { data_root, .. } => Some(data_root.as_path()),
            _ => anyhow::bail!("Persisted BtrfsBase identity has incompatible backend paths; refusing storage selection"),
        }
    } else {
        None
    };
    let backend = backend_detect::create_backend_from_persisted(
        effective_backend_type,
        config,
        persisted_btrfs_root,
    )
    .await
    .with_context(|| {
        format!(
            "Failed to create {:?} backend. \
                 To change backend type, edit [backend] type in /etc/ws-ckpt/config.toml. \
                 To reset all state, remove {:?}.",
            effective_backend_type,
            state_dir.join(ws_ckpt_common::STATE_FILE)
        )
    })?;

    // BtrfsLoop restore invariant: img data must pre-exist somewhere. Either the
    // canonical FHS path or the pre-FHS legacy path counts — backend creation has
    // already attempted migration / fallback, so this only catches the truly
    // catastrophic "both files gone but state.json still claims data" case.
    if backend.backend_type() == ws_ckpt_common::backend::BackendType::BtrfsLoop
        && state_file.backend.backend_type == ws_ckpt_common::backend::BackendType::BtrfsLoop
    {
        let target = std::path::Path::new(ws_ckpt_common::BTRFS_IMG_PATH);
        let legacy = std::path::Path::new(ws_ckpt_common::LEGACY_BTRFS_IMG_PATH);
        if !target.exists() && !legacy.exists() {
            anyhow::bail!(
                "Backend image file not found at {:?} (and no legacy file at {:?}). \
                 Persisted state expects a BtrfsLoop backend but the image is missing — \
                 all snapshot data may be lost. \
                 To change backend type, edit [backend] type in /etc/ws-ckpt/config.toml. \
                 To reset all state, remove {:?}.",
                target,
                legacy,
                state_dir.join(ws_ckpt_common::STATE_FILE)
            );
        }
    }
    if let Some(previous_root) = persisted_btrfs_root
        .filter(|root| config.btrfs_mount.is_some() && *root != backend.data_root())
    {
        warn!(
            previous_root = ?previous_root,
            new_root = ?backend.data_root(),
            state_dir = ?state_dir,
            "Configured btrfs_mount changes the storage root. Previous snapshots become invisible; \
             startup reconciliation can prune unpinned index records from the state directory. \
             Pointing back to the previous root does not recover pruned index records."
        );
    }
    backend
        .bootstrap(config)
        .await
        .context("Failed to bootstrap backend during state recovery")?;

    let state = Arc::new(
        DaemonState::rebuild_from_persisted(
            state_file,
            config.clone(),
            backend,
            state_dir.to_path_buf(),
            selection_method,
        )
        .await
        .context("Failed to rebuild daemon state from persisted state.json")?,
    );
    state.mark_bootstrapped();
    Ok(state)
}

/// Fresh start: detect backend, bootstrap, optionally migrate legacy indexes.
async fn resolve_fresh(
    config: &DaemonConfig,
    state_dir: &Path,
) -> anyhow::Result<Arc<DaemonState>> {
    info!("No persisted state file found, starting in fresh install or migration mode");

    // Detect and create backend
    let detect_result = backend_detect::detect_and_create_backend(config)
        .await
        .context("Failed to detect and create storage backend")?;
    info!(
        "Backend selected: {} (method: {})",
        detect_result.backend.backend_type(),
        detect_result.method
    );

    detect_result
        .backend
        .bootstrap(config)
        .await
        .context("Failed to bootstrap backend on fresh install")?;

    // Attempt to migrate old position index (synchronous call)
    let backend_ref = &detect_result.backend;
    let migrated =
        ws_ckpt_common::migration::migrate_legacy_indexes(backend_ref.as_ref(), state_dir);

    let state = if migrated {
        // Migrated — reconstruct from state_dir
        let sf = persist::load_state(state_dir)?;
        if let Some(ref state_file) = sf {
            Arc::new(
                DaemonState::rebuild_from_persisted(
                    state_file,
                    config.clone(),
                    detect_result.backend,
                    state_dir.to_path_buf(),
                    "auto-detect",
                )
                .await
                .context("Failed to rebuild daemon state after legacy migration")?,
            )
        } else {
            Arc::new(
                DaemonState::rebuild_from_disk(
                    config.clone(),
                    detect_result.backend,
                    state_dir.to_path_buf(),
                )
                .await
                .context("Failed to rebuild daemon state from disk after migration")?,
            )
        }
    } else {
        // Fresh install or no old data — rebuild_from_disk
        Arc::new(
            DaemonState::rebuild_from_disk(
                config.clone(),
                detect_result.backend,
                state_dir.to_path_buf(),
            )
            .await
            .context("Failed to rebuild daemon state from disk on fresh install")?,
        )
    };
    state.mark_bootstrapped();

    Ok(state)
}

#[cfg(test)]
mod native_btrfs_tests;

#[cfg(test)]
mod native_btrfs_config_tests;
