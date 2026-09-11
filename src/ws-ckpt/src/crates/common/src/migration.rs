//! Legacy index migration: move workspace index.json files from the old
//! in-snapshot layout to state_dir/indexes/ and generate state.json.

use std::fs;
use std::path::Path;

use anyhow::Context;
use tracing::{info, warn};

use crate::backend::StorageBackend;
use crate::persist::{
    self, BackendIdentity, BackendPaths, DaemonStateFile, WorkspaceEntry, DAEMON_STATE_VERSION,
};
use crate::{SnapshotIndex, INDEXES_DIR, INDEX_FILE};

/// Atomically write a `SnapshotIndex` to `ws_dir/index.json` via tmp+rename.
fn save_index_sync(ws_dir: &Path, index: &SnapshotIndex) -> anyhow::Result<()> {
    let index_path = ws_dir.join(INDEX_FILE);
    let tmp_path = ws_dir.join(format!("{}.tmp", INDEX_FILE));
    let content =
        serde_json::to_string_pretty(index).context("Failed to serialize SnapshotIndex")?;
    std::fs::write(&tmp_path, &content)
        .with_context(|| format!("Failed to write {:?}", tmp_path))?;
    std::fs::rename(&tmp_path, &index_path)
        .with_context(|| format!("Failed to rename {:?} -> {:?}", tmp_path, index_path))?;
    Ok(())
}

/// Migrate old position index.json files to the new state_dir/indexes/ directory.
///
/// When state.json does not exist (upgrade scenario), scan old position and migrate.
/// Returns true if a migration occurred.
pub fn migrate_legacy_indexes(backend: &dyn StorageBackend, state_dir: &Path) -> bool {
    let snapshots_root = backend.snapshots_root().to_path_buf();

    let read_dir = match fs::read_dir(&snapshots_root) {
        Ok(rd) => rd,
        Err(_) => return false,
    };

    let mut migrated_any = false;
    let mut logged_migration_start = false;
    let mut workspace_entries: Vec<WorkspaceEntry> = Vec::new();
    let new_indexes_root = state_dir.join(INDEXES_DIR);

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }

        let ws_id = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => continue,
        };

        let old_index_path = path.join(INDEX_FILE);
        if !old_index_path.exists() {
            continue;
        }

        // Log migration start on first workspace that needs migration
        if !logged_migration_start {
            info!(
                "Migrating from v0 layout: moving indexes from {:?} to {:?}",
                snapshots_root, new_indexes_root
            );
            logged_migration_start = true;
        }

        // Read old index
        let content = match fs::read_to_string(&old_index_path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Migration: failed to read old index file {:?}: {}",
                    old_index_path, e
                );
                continue;
            }
        };

        let index: SnapshotIndex = match serde_json::from_str(&content) {
            Ok(idx) => idx,
            Err(e) => {
                warn!(
                    "Migration: failed to parse old index file {:?}: {}",
                    old_index_path, e
                );
                continue;
            }
        };

        // Create new position directory and write it
        let new_index_dir = state_dir.join(INDEXES_DIR).join(&ws_id);
        if let Err(e) = fs::create_dir_all(&new_index_dir) {
            warn!(
                "Migration: failed to create index directory {:?}: {}",
                new_index_dir, e
            );
            continue;
        }

        if let Err(e) = save_index_sync(&new_index_dir, &index) {
            warn!("Migration: failed to save index {:?}: {}", new_index_dir, e);
            continue;
        }

        // Remove old position file (move semantics)
        if let Err(e) = fs::remove_file(&old_index_path) {
            warn!(
                "Migration: failed to remove old index file {:?}: {}",
                old_index_path, e
            );
        }

        info!("Legacy index migration completed for workspace {}", ws_id);

        workspace_entries.push(WorkspaceEntry {
            ws_id: ws_id.clone(),
            workspace_path: index.workspace_path.clone(),
            registered_at: chrono::Utc::now(),
            origin_backend: backend.backend_type(),
        });
        migrated_any = true;
    }

    if migrated_any {
        // Construct DaemonStateFile and save it
        let state_file = DaemonStateFile {
            version: DAEMON_STATE_VERSION,
            backend: BackendIdentity {
                backend_type: backend.backend_type(),
                selection_method: "auto-detect".to_string(),
                selected_at: chrono::Utc::now(),
            },
            paths: match backend.backend_type() {
                crate::backend::BackendType::BtrfsLoop => BackendPaths::BtrfsLoop {
                    mount_path: backend.data_root().to_path_buf(),
                    data_root: backend.data_root().to_path_buf(),
                    snapshots_root: backend.snapshots_root().to_path_buf(),
                    loop_img: None,
                },
                crate::backend::BackendType::BtrfsBase => BackendPaths::BtrfsBase {
                    mount_path: backend.data_root().to_path_buf(),
                    data_root: backend.data_root().to_path_buf(),
                    snapshots_root: backend.snapshots_root().to_path_buf(),
                },
            },
            workspaces: workspace_entries,
        };
        if let Err(e) = persist::save_state(state_dir, &state_file) {
            warn!("Migration: failed to save state.json: {:#}", e);
        } else {
            info!("Migration complete, state.json generated");
        }
    }

    migrated_any
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use crate::backend::{BackendType, EnvironmentStatus, GcResult, StorageBackend};
    use crate::{DiffEntry, SnapshotIndex, SnapshotMeta, WorkspaceInfo};

    struct MockBackend {
        data_root: PathBuf,
        snapshots_root: PathBuf,
    }

    #[async_trait::async_trait]
    impl StorageBackend for MockBackend {
        fn backend_type(&self) -> BackendType {
            BackendType::BtrfsBase
        }
        fn data_root(&self) -> &std::path::Path {
            &self.data_root
        }
        fn snapshots_root(&self) -> &std::path::Path {
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
        async fn recover_workspace(&self, _: &str, _: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn diff(&self, _: &str, _: &str, _: Option<&str>) -> anyhow::Result<Vec<DiffEntry>> {
            unimplemented!()
        }
        async fn cleanup_snapshots(
            &self,
            _: &str,
            _: &[String],
        ) -> anyhow::Result<Vec<(String, crate::backend::SnapshotDeleteOutcome)>> {
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
            unimplemented!()
        }
    }

    fn make_index(workspace_path: &str) -> SnapshotIndex {
        SnapshotIndex {
            workspace_path: PathBuf::from(workspace_path),
            snapshots: HashMap::new(),
            head: None,
            governed_evidence: HashMap::new(),
            guarded_rollbacks: HashMap::new(),
        }
    }

    fn write_old_index(snapshots_root: &Path, ws_id: &str, index: &SnapshotIndex) {
        let ws_dir = snapshots_root.join(ws_id);
        fs::create_dir_all(&ws_dir).unwrap();
        let content = serde_json::to_string_pretty(index).unwrap();
        fs::write(ws_dir.join(crate::INDEX_FILE), content).unwrap();
    }

    #[test]
    fn migrate_empty_dir_returns_false() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(!migrate_legacy_indexes(&backend, state_dir.path()));
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
    }

    #[test]
    fn migrate_single_workspace() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let index = make_index("/home/user/project");
        write_old_index(snap_dir.path(), "ws-abc", &index);

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(migrate_legacy_indexes(&backend, state_dir.path()));

        // New index exists
        let new_path = state_dir
            .path()
            .join(crate::INDEXES_DIR)
            .join("ws-abc")
            .join(crate::INDEX_FILE);
        assert!(new_path.exists());
        let loaded: SnapshotIndex =
            serde_json::from_str(&fs::read_to_string(&new_path).unwrap()).unwrap();
        assert_eq!(loaded.workspace_path, PathBuf::from("/home/user/project"));

        // Old index removed
        assert!(!snap_dir
            .path()
            .join("ws-abc")
            .join(crate::INDEX_FILE)
            .exists());

        // state.json written
        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        assert_eq!(sf.workspaces.len(), 1);
        assert_eq!(sf.workspaces[0].ws_id, "ws-abc");
    }

    #[test]
    fn migrate_multiple_workspaces() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        write_old_index(snap_dir.path(), "ws-a", &make_index("/a"));
        write_old_index(snap_dir.path(), "ws-b", &make_index("/b"));

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(migrate_legacy_indexes(&backend, state_dir.path()));

        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        assert_eq!(sf.workspaces.len(), 2);
    }

    #[test]
    fn migrate_skips_non_dir() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        fs::write(snap_dir.path().join("regular-file"), "not a dir").unwrap();

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(!migrate_legacy_indexes(&backend, state_dir.path()));
    }

    #[test]
    fn migrate_skips_dir_without_index() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        fs::create_dir(snap_dir.path().join("ws-empty")).unwrap();

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(!migrate_legacy_indexes(&backend, state_dir.path()));
    }

    #[test]
    fn migrate_handles_invalid_json() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();

        // Bad JSON workspace
        let bad_dir = snap_dir.path().join("ws-bad");
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(bad_dir.join(crate::INDEX_FILE), "not json {{{").unwrap();

        // Good workspace
        write_old_index(snap_dir.path(), "ws-good", &make_index("/good"));

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(migrate_legacy_indexes(&backend, state_dir.path()));

        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        assert_eq!(sf.workspaces.len(), 1);
        assert_eq!(sf.workspaces[0].ws_id, "ws-good");
    }

    #[test]
    fn save_index_sync_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = make_index("/test");
        index.snapshots.insert(
            "snap1".to_string(),
            SnapshotMeta {
                created_at: chrono::Utc::now(),
                message: Some("test".to_string()),
                metadata: None,
                pinned: false,
                missing: false,
                parent_id: None,
                child_ids: Vec::new(),
            },
        );
        save_index_sync(dir.path(), &index).unwrap();

        let content = fs::read_to_string(dir.path().join(crate::INDEX_FILE)).unwrap();
        let loaded: SnapshotIndex = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.workspace_path, index.workspace_path);
        assert!(loaded.snapshots.contains_key("snap1"));
    }
}
