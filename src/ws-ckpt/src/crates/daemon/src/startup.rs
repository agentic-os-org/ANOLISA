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
/// Loads state.json (with parse-failure downgrade to fresh start), then either
/// restores from persisted state or performs fresh backend detection + optional
/// legacy index migration.
pub(crate) async fn resolve_state(
    config: &DaemonConfig,
    state_dir: &Path,
) -> anyhow::Result<Arc<DaemonState>> {
    // 1. Attempt to load state.json (parse failure downgrades to fresh start,
    //    to avoid corrupt file causing daemon infinite restart)
    let persisted = match persist::load_state(state_dir) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Failed to load state.json, treating as fresh start: {:#}. \
                 To recover, fix or remove {:?}.",
                e,
                state_dir.join(ws_ckpt_common::STATE_FILE)
            );
            None
        }
    };

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
    let (effective_backend_type, selection_method) =
        if let Some(config_type) = config.parse_backend_type() {
            if config_type != state_file.backend.backend_type {
                warn!(
                "Config overrides persisted backend_type: {:?} -> {:?} (config is authoritative)",
                state_file.backend.backend_type, config_type
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

    let backend = backend_detect::create_backend(effective_backend_type, config)
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

    // BtrfsBase restore invariant: the daemon re-detects "first btrfs mount
    // in /proc/mounts" on every start; a foreign partition must never
    // silently become the data root, because the reconcile inside
    // rebuild_from_persisted would prune the persisted indexes against it.
    // Same fail-fast contract as the BtrfsLoop image check below.
    verify_persisted_btrfs_base_root(
        state_file,
        backend.as_ref(),
        &state_dir.join(ws_ckpt_common::STATE_FILE),
    )?;

    // BtrfsLoop restore invariant: img data must pre-exist somewhere. Either the
    // canonical FHS path or the pre-FHS legacy path counts — backend creation has
    // already attempted migration / fallback, so this only catches the truly
    // catastrophic "both files gone but state.json still claims data" case.
    if backend.backend_type() == ws_ckpt_common::backend::BackendType::BtrfsLoop {
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

/// Fail fast when the freshly detected BtrfsBase data root does not match
/// the one recorded in state.json.
///
/// BtrfsBase picks "first writable btrfs mount in /proc/mounts" on every
/// start (`find_available_btrfs_partition`), so a newly attached btrfs
/// disk — or a temporarily unmounted real one — silently rebases the
/// daemon onto a foreign partition. Bootstrap then CREATES
/// `<foreign>/ws-ckpt-data`, and the startup reconcile prunes every
/// unpinned snapshot record from the persisted indexes (state lives under
/// /var/lib/ws-ckpt, partition-independent), destroying messages/lineage
/// that the real partition still holds. This is the BtrfsBase counterpart
/// of the BtrfsLoop image invariant: persisted paths are an identity
/// contract, not a suggestion.
fn verify_persisted_btrfs_base_root(
    state_file: &ws_ckpt_common::persist::DaemonStateFile,
    backend: &dyn ws_ckpt_common::backend::StorageBackend,
    state_json_path: &Path,
) -> anyhow::Result<()> {
    use ws_ckpt_common::backend::BackendType;
    use ws_ckpt_common::persist::BackendPaths;
    // Only guard the persisted-type == effective-type case: a config
    // override to btrfs-loop is a deliberate migration with its own
    // invariant below.
    if state_file.backend.backend_type != BackendType::BtrfsBase
        || backend.backend_type() != BackendType::BtrfsBase
    {
        return Ok(());
    }
    let BackendPaths::BtrfsBase {
        data_root: persisted,
        ..
    } = &state_file.paths
    else {
        return Ok(()); // persisted loop paths with a base backend: override case
    };
    let detected = backend.data_root();

    // Tolerate textual differences that resolve to the same directory
    // (symlinked mount points) before refusing.
    let same_root = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        // One side unresolvable: fall back to the textual comparison so a
        // missing persisted root is still reported as a mismatch below.
        _ => a == b,
    };
    if same_root(persisted, detected) {
        return Ok(());
    }
    let persisted_exists = persisted.exists();
    anyhow::bail!(
        "Persisted BtrfsBase data root {:?} does not match the detected \
         btrfs partition's data root {:?} (persisted root {} on disk). \
         Auto-cleanup reconciliation would prune the persisted snapshot \
         indexes against the wrong partition. Mount the recorded btrfs \
         partition (or pin the right one and edit [backend] type in \
         /etc/ws-ckpt/config.toml), or remove {:?} to reset all state.",
        persisted,
        detected,
        if persisted_exists {
            "still exists"
        } else {
            "is missing"
        },
        state_json_path
    );
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
mod tests {
    use super::*;
    use crate::backends::btrfs_base::{BtrfsBaseBackend, BtrfsBaseScenario};
    use std::path::PathBuf;
    use ws_ckpt_common::persist::{BackendIdentity, BackendPaths, DaemonStateFile};

    /// A persisted BtrfsBase state file whose data root lives under
    /// `mount` (the shape write_manifest persists on every save).
    fn base_state_file(mount: &Path) -> DaemonStateFile {
        let data_root = mount.join("ws-ckpt-data");
        DaemonStateFile::new(
            ws_ckpt_common::persist::DAEMON_STATE_VERSION,
            BackendIdentity {
                backend_type: ws_ckpt_common::backend::BackendType::BtrfsBase,
                selection_method: "auto-detect".to_string(),
                selected_at: chrono::Utc::now(),
            },
            BackendPaths::BtrfsBase {
                mount_path: mount.to_path_buf(),
                snapshots_root: data_root.join("snapshots"),
                data_root,
            },
            vec![],
        )
    }

    fn state_json(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join("state.json")
    }

    #[test]
    fn restart_refuses_a_foreign_btrfs_base_partition() {
        let real = tempfile::tempdir().unwrap();
        let persisted = base_state_file(real.path());
        // The recorded root exists and holds snapshot data.
        let snap = real.path().join("ws-ckpt-data/snapshots/ws-1/snap-a");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(snap.join("canary"), "real data").unwrap();

        let foreign = tempfile::tempdir().unwrap();
        let backend =
            BtrfsBaseBackend::new(foreign.path().to_path_buf(), BtrfsBaseScenario::CrossDisk);
        let tmp = tempfile::tempdir().unwrap();

        let err = verify_persisted_btrfs_base_root(&persisted, &backend, &state_json(&tmp))
            .expect_err("a foreign partition must refuse to start");
        let msg = format!("{err:#}");
        assert!(msg.contains("does not match"), "{msg}");
        assert!(msg.contains("state.json"), "remediation must name state.json: {msg}");
        // Fail-safe: nothing was created or pruned on either side.
        assert!(snap.join("canary").exists());
        assert!(!foreign.path().join("ws-ckpt-data").exists());
    }

    #[test]
    fn restart_accepts_a_matching_btrfs_base_partition() {
        let real = tempfile::tempdir().unwrap();
        let persisted = base_state_file(real.path());
        let backend =
            BtrfsBaseBackend::new(real.path().to_path_buf(), BtrfsBaseScenario::InPlace);
        let tmp = tempfile::tempdir().unwrap();
        verify_persisted_btrfs_base_root(&persisted, &backend, &state_json(&tmp))
            .expect("the recorded partition must pass");
    }

    #[test]
    fn restart_refuses_when_the_persisted_data_root_is_missing() {
        // The recorded partition is temporarily unmounted: the persisted
        // root does not exist, detection picked another mount. This is the
        // "reboot while A is unmounted" shape — still a refusal, with the
        // "is missing" wording.
        let real = tempfile::tempdir().unwrap();
        let persisted = base_state_file(real.path());
        // NOTE: real's tempdir exists but ws-ckpt-data under it does not.
        let foreign = tempfile::tempdir().unwrap();
        let backend =
            BtrfsBaseBackend::new(foreign.path().to_path_buf(), BtrfsBaseScenario::CrossDisk);
        let tmp = tempfile::tempdir().unwrap();
        let err = verify_persisted_btrfs_base_root(&persisted, &backend, &state_json(&tmp))
            .expect_err("a missing recorded root must refuse");
        assert!(format!("{err:#}").contains("is missing"));
    }

    #[test]
    fn btrfs_loop_and_override_states_bypass_the_guard() {
        let tmp = tempfile::tempdir().unwrap();
        // Persisted BtrfsLoop paths: the loop invariant owns this case.
        let loop_file = DaemonStateFile::new(
            ws_ckpt_common::persist::DAEMON_STATE_VERSION,
            BackendIdentity {
                backend_type: ws_ckpt_common::backend::BackendType::BtrfsLoop,
                selection_method: "auto-detect".to_string(),
                selected_at: chrono::Utc::now(),
            },
            BackendPaths::BtrfsLoop {
                mount_path: tmp.path().join("mnt"),
                data_root: tmp.path().join("mnt/ws-ckpt-data"),
                snapshots_root: tmp.path().join("mnt/ws-ckpt-data/snapshots"),
                loop_img: None,
            },
            vec![],
        );
        let foreign = tempfile::tempdir().unwrap();
        let base_backend =
            BtrfsBaseBackend::new(foreign.path().to_path_buf(), BtrfsBaseScenario::CrossDisk);
        verify_persisted_btrfs_base_root(&loop_file, &base_backend, &state_json(&tmp))
            .expect("loop-persisted state is not this guard's scope");

        // Persisted BtrfsBase + effective BtrfsLoop (config override): a
        // deliberate migration, guarded by the loop invariant instead.
        let base_file = base_state_file(foreign.path());
        let loop_backend_mount = tempfile::tempdir().unwrap();
        let loop_backend = crate::backends::btrfs_loop::BtrfsLoopBackend::new(
            loop_backend_mount.path().to_path_buf(),
            loop_backend_mount.path().join("image"),
        );
        verify_persisted_btrfs_base_root(&base_file, &loop_backend, &state_json(&tmp))
            .expect("an override to btrfs-loop is a deliberate migration");
    }

    /// The amplifier the guard closes: reconcile pointed at a foreign (empty)
    /// snapshot bucket prunes unpinned records and would persist the pruned
    /// index. Passes on main — it documents why the guard must exist.
    #[tokio::test]
    async fn foreign_root_reconcile_prunes_unpinned_records() {
        let real = tempfile::tempdir().unwrap();
        let ws_snap = real.path().join("ws-ckpt-data/snapshots/ws-amp");
        let mut index = ws_ckpt_common::SnapshotIndex::new(ws_snap.clone());
        let now = chrono::Utc::now();
        for (id, pinned, off) in [
            ("snap-pinned", true, 30i64),
            ("snap-old", false, 20),
            ("snap-new", false, 10),
        ] {
            let meta_dir = ws_snap.join(id);
            std::fs::create_dir_all(&meta_dir).unwrap();
            index.snapshots.insert(
                id.to_string(),
                ws_ckpt_common::SnapshotMeta {
                    message: Some(id.to_string()),
                    metadata: None,
                    pinned,
                    created_at: now - chrono::Duration::seconds(off),
                    missing: false,
                    parent_id: None,
                    child_ids: Vec::new(),
                },
            );
        }

        let foreign = tempfile::tempdir().unwrap();
        let foreign_ws = foreign.path().join("ws-ckpt-data/snapshots/ws-amp");
        std::fs::create_dir_all(&foreign_ws).unwrap();

        let changed = crate::index_store::reconcile_from_fs(&foreign_ws, &mut index)
            .await
            .expect("reconcile over the foreign root");
        assert!(changed, "the prune must be reported so the caller persists it");
        assert!(
            !index.snapshots.contains_key("snap-old")
                && !index.snapshots.contains_key("snap-new"),
            "unpinned records are pruned against the foreign root"
        );
        let pinned = index.snapshots.get("snap-pinned").expect("pinned survives");
        assert!(pinned.missing, "the pinned record is flagged missing");
        // The real partition still holds everything.
        assert!(ws_snap.join("snap-old").exists());
    }
}
