//! Daemon startup path: load persisted state or perform fresh detection/migration.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tracing::{info, warn};

use ws_ckpt_common::backend::StorageBackend;
use ws_ckpt_common::migration::LegacyMigrationOutcome;
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

    resolve_fresh_with_backend(config, state_dir, detect_result.backend).await
}

async fn resolve_fresh_with_backend(
    config: &DaemonConfig,
    state_dir: &Path,
    backend: Arc<dyn StorageBackend>,
) -> anyhow::Result<Arc<DaemonState>> {
    // Attempt to migrate old position index (synchronous call)
    let state = match ws_ckpt_common::migration::migrate_legacy_indexes_outcome(
        backend.as_ref(),
        state_dir,
    ) {
        LegacyMigrationOutcome::Migrated => {
            // Migrated — reconstruct from state_dir
            let sf = persist::load_state(state_dir)?;
            if let Some(ref state_file) = sf {
                Arc::new(
                    DaemonState::rebuild_after_legacy_migration(
                        state_file,
                        config.clone(),
                        backend,
                        state_dir.to_path_buf(),
                    )
                    .await
                    .context("Failed to rebuild daemon state after legacy migration")?,
                )
            } else {
                Arc::new(
                    DaemonState::rebuild_from_disk(
                        config.clone(),
                        backend,
                        state_dir.to_path_buf(),
                    )
                    .await
                    .context("Failed to rebuild daemon state from disk after migration")?,
                )
            }
        }
        LegacyMigrationOutcome::NothingToMigrate => {
            // Fresh install or no old data — rebuild_from_disk
            Arc::new(
                DaemonState::rebuild_from_disk(config.clone(), backend, state_dir.to_path_buf())
                    .await
                    .context("Failed to rebuild daemon state from disk on fresh install")?,
            )
        }
        LegacyMigrationOutcome::Deferred => {
            anyhow::bail!(
                "Legacy workspace index migration was deferred due to I/O errors; \
                 refusing startup so an incomplete {:?} is not committed. \
                 Resolve the storage/state error and restart.",
                state_dir.join(ws_ckpt_common::STATE_FILE)
            );
        }
    };
    state.mark_bootstrapped();

    Ok(state)
}

#[cfg(test)]
mod native_btrfs_tests;

#[cfg(test)]
mod native_btrfs_config_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use ws_ckpt_common::backend::{
        BackendType, EnvironmentStatus, GcResult, SnapshotDeleteOutcome,
    };
    use ws_ckpt_common::{
        DiffEntry, SnapshotIndex, SnapshotMeta, WorkspaceInfo, INDEXES_DIR, INDEX_FILE, STATE_FILE,
    };

    struct TestBackend {
        data_root: PathBuf,
        snapshots_root: PathBuf,
    }

    #[async_trait::async_trait]
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
            unimplemented!()
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
        async fn recover_workspace(&self, _: &str, _: &str) -> anyhow::Result<Vec<String>> {
            unimplemented!()
        }
        async fn diff(&self, _: &str, _: &str, _: Option<&str>) -> anyhow::Result<Vec<DiffEntry>> {
            unimplemented!()
        }
        async fn cleanup_snapshots(
            &self,
            _: &str,
            _: &[String],
        ) -> anyhow::Result<Vec<(String, SnapshotDeleteOutcome)>> {
            unimplemented!()
        }
        async fn fork(&self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn gc_generations(&self, _: &str) -> anyhow::Result<GcResult> {
            unimplemented!()
        }
        async fn check_environment(&self) -> anyhow::Result<EnvironmentStatus> {
            unimplemented!()
        }
        async fn get_usage(&self) -> anyhow::Result<(u64, u64)> {
            Ok((1 << 40, 1 << 30))
        }
    }

    fn write_legacy_workspace(
        snapshots_root: &Path,
        ws_id: &str,
        workspace_path: &Path,
        snap_id: &str,
    ) {
        fs::create_dir_all(workspace_path).unwrap();
        let snap_dir = snapshots_root.join(ws_id);
        fs::create_dir_all(snap_dir.join(snap_id)).unwrap();
        let mut snapshots = HashMap::new();
        snapshots.insert(
            snap_id.to_string(),
            SnapshotMeta {
                created_at: chrono::Utc::now(),
                message: Some("legacy".to_string()),
                metadata: None,
                pinned: false,
                missing: false,
                parent_id: None,
                child_ids: Vec::new(),
            },
        );
        let index = SnapshotIndex {
            workspace_path: workspace_path.to_path_buf(),
            snapshots,
            head: None,
            governed_evidence: HashMap::new(),
            guarded_rollbacks: HashMap::new(),
            recovered_orphans: Default::default(),
        };
        fs::write(
            snap_dir.join(INDEX_FILE),
            serde_json::to_string_pretty(&index).unwrap(),
        )
        .unwrap();
    }

    async fn simulate_daemon_boot(
        config: &DaemonConfig,
        state_dir: &Path,
        backend: Arc<dyn StorageBackend>,
    ) -> anyhow::Result<Arc<DaemonState>> {
        let persisted = persist::load_state(state_dir)?;
        let state = if let Some(ref state_file) = persisted {
            let state = Arc::new(
                DaemonState::rebuild_from_persisted(
                    state_file,
                    config.clone(),
                    backend,
                    state_dir.to_path_buf(),
                    "persisted",
                )
                .await?,
            );
            state.mark_bootstrapped();
            state
        } else {
            resolve_fresh_with_backend(config, state_dir, backend).await?
        };
        // Mirrors daemon::lib startup sequence before serving IPC requests.
        state.save_manifest().await?;
        Ok(state)
    }

    #[tokio::test]
    async fn deferred_legacy_migration_blocks_partial_manifest_and_recovers_on_restart() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let snapshots_root = data_root.join("snapshots");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&snapshots_root).unwrap();
        fs::create_dir_all(state_dir.join(INDEXES_DIR)).unwrap();

        write_legacy_workspace(
            &snapshots_root,
            "ws-a",
            &temp.path().join("repo-a"),
            "snap-a",
        );
        write_legacy_workspace(
            &snapshots_root,
            "ws-b",
            &temp.path().join("repo-b"),
            "snap-b",
        );

        let blocker = state_dir.join(INDEXES_DIR).join("ws-b");
        fs::write(&blocker, "not-a-dir").unwrap();

        let backend: Arc<dyn StorageBackend> = Arc::new(TestBackend {
            data_root: data_root.clone(),
            snapshots_root: snapshots_root.clone(),
        });
        let config = DaemonConfig {
            mount_path: data_root.clone(),
            ..DaemonConfig::default()
        };

        let first_boot = simulate_daemon_boot(&config, &state_dir, Arc::clone(&backend)).await;
        assert!(
            first_boot.is_err(),
            "startup must fail closed instead of persisting partial state.json"
        );
        assert!(!state_dir.join(STATE_FILE).exists());
        let legacy_a = snapshots_root.join("ws-a").join(INDEX_FILE);
        let legacy_b = snapshots_root.join("ws-b").join(INDEX_FILE);
        assert!(legacy_a.exists());
        assert!(legacy_b.exists());

        fs::remove_file(&blocker).unwrap();

        let recovered = simulate_daemon_boot(&config, &state_dir, Arc::clone(&backend))
            .await
            .expect("startup after clearing blocker must migrate all workspaces");
        assert!(state_dir.join(STATE_FILE).exists());
        assert!(!legacy_a.exists());
        assert!(!legacy_b.exists());

        let mut infos = recovered.get_all_workspace_info().await;
        infos.sort_by(|a, b| a.ws_id.cmp(&b.ws_id));
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].ws_id, "ws-a");
        assert_eq!(infos[0].snapshot_count, 1);
        assert_eq!(infos[1].ws_id, "ws-b");
        assert_eq!(infos[1].snapshot_count, 1);

        let sf = persist::load_state(&state_dir).unwrap().unwrap();
        let mut persisted_ids: Vec<_> = sf.workspaces.iter().map(|w| w.ws_id.as_str()).collect();
        persisted_ids.sort();
        assert_eq!(persisted_ids, vec!["ws-a", "ws-b"]);

        let restarted = simulate_daemon_boot(&config, &state_dir, Arc::clone(&backend))
            .await
            .expect("subsequent persisted restart must retain all migrated workspaces");
        let mut restarted_infos = restarted.get_all_workspace_info().await;
        restarted_infos.sort_by(|a, b| a.ws_id.cmp(&b.ws_id));
        assert_eq!(restarted_infos.len(), 2);
        assert_eq!(restarted_infos[0].ws_id, "ws-a");
        assert_eq!(restarted_infos[0].snapshot_count, 1);
        assert_eq!(restarted_infos[1].ws_id, "ws-b");
        assert_eq!(restarted_infos[1].snapshot_count, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_legacy_index_blocks_partial_manifest_and_recovers_on_restart() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let snapshots_root = data_root.join("snapshots");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&snapshots_root).unwrap();
        fs::create_dir_all(state_dir.join(INDEXES_DIR)).unwrap();

        write_legacy_workspace(
            &snapshots_root,
            "ws-a",
            &temp.path().join("repo-a"),
            "snap-a",
        );
        write_legacy_workspace(
            &snapshots_root,
            "ws-b",
            &temp.path().join("repo-b"),
            "snap-b",
        );

        let legacy_a = snapshots_root.join("ws-a").join(INDEX_FILE);
        let legacy_b = snapshots_root.join("ws-b").join(INDEX_FILE);
        let orig_mode = fs::metadata(&legacy_b).unwrap().permissions();
        fs::set_permissions(&legacy_b, fs::Permissions::from_mode(0o000)).unwrap();

        // Skip under root/CAP_DAC_OVERRIDE where 0o000 files remain readable.
        if fs::read_to_string(&legacy_b).is_ok() {
            fs::set_permissions(&legacy_b, orig_mode).unwrap();
            return;
        }

        let backend: Arc<dyn StorageBackend> = Arc::new(TestBackend {
            data_root: data_root.clone(),
            snapshots_root: snapshots_root.clone(),
        });
        let config = DaemonConfig {
            mount_path: data_root.clone(),
            ..DaemonConfig::default()
        };

        let first_boot = simulate_daemon_boot(&config, &state_dir, Arc::clone(&backend)).await;
        fs::set_permissions(&legacy_b, orig_mode).unwrap();

        assert!(
            first_boot.is_err(),
            "transient read failure on one legacy index must block startup manifest commit"
        );
        assert!(!state_dir.join(STATE_FILE).exists());
        assert!(legacy_a.exists());
        assert!(legacy_b.exists());

        let recovered = simulate_daemon_boot(&config, &state_dir, Arc::clone(&backend))
            .await
            .expect("restart after read error clears must migrate both workspaces");
        assert!(state_dir.join(STATE_FILE).exists());
        assert!(!legacy_a.exists());
        assert!(!legacy_b.exists());

        let restarted = simulate_daemon_boot(&config, &state_dir, Arc::clone(&backend))
            .await
            .expect("persisted restart must retain migrated workspaces");
        drop(recovered);
        let mut restarted_infos = restarted.get_all_workspace_info().await;
        restarted_infos.sort_by(|a, b| a.ws_id.cmp(&b.ws_id));
        assert_eq!(restarted_infos.len(), 2);
        assert_eq!(restarted_infos[0].ws_id, "ws-a");
        assert_eq!(restarted_infos[0].snapshot_count, 1);
        assert_eq!(restarted_infos[1].ws_id, "ws-b");
        assert_eq!(restarted_infos[1].snapshot_count, 1);
    }

    #[tokio::test]
    async fn fresh_install_without_legacy_indexes_saves_empty_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let snapshots_root = data_root.join("snapshots");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&snapshots_root).unwrap();
        fs::create_dir_all(state_dir.join(INDEXES_DIR)).unwrap();

        let backend: Arc<dyn StorageBackend> = Arc::new(TestBackend {
            data_root: data_root.clone(),
            snapshots_root,
        });
        let config = DaemonConfig {
            mount_path: data_root,
            ..DaemonConfig::default()
        };

        let state = simulate_daemon_boot(&config, &state_dir, backend)
            .await
            .expect("empty fresh install must succeed");
        assert!(state.get_all_workspace_info().await.is_empty());
        let sf = persist::load_state(&state_dir).unwrap().unwrap();
        assert!(sf.workspaces.is_empty());
    }
}
