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

/// Atomically write and fsync a `SnapshotIndex` to `ws_dir/index.json`.
fn save_index_sync(ws_dir: &Path, index: &SnapshotIndex) -> anyhow::Result<()> {
    let content =
        serde_json::to_string_pretty(index).context("Failed to serialize SnapshotIndex")?;
    persist::atomic_write(ws_dir, INDEX_FILE, content.as_bytes(), None)
}

/// Outcome of scanning and migrating legacy v0 workspace indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyMigrationOutcome {
    /// One or more legacy workspace indexes were migrated and `state.json` was committed.
    Migrated,
    /// No migratable legacy indexes existed; safe to proceed as a fresh start.
    NothingToMigrate,
    /// Legacy migration encountered an I/O or persistence failure and left legacy
    /// indexes intact; callers must not commit an incomplete `state.json`.
    Deferred,
}

/// Migrate old position index.json files to the new state_dir/indexes/ directory.
///
/// When state.json does not exist (upgrade scenario), scan old position and migrate.
/// Returns a structured [`LegacyMigrationOutcome`] so callers can distinguish
/// "nothing to migrate" from a transient migration failure that must block
/// partial manifest commits.
pub fn migrate_legacy_indexes_outcome(
    backend: &dyn StorageBackend,
    state_dir: &Path,
) -> LegacyMigrationOutcome {
    let snapshots_root = backend.snapshots_root().to_path_buf();

    let read_dir = match fs::read_dir(&snapshots_root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return LegacyMigrationOutcome::NothingToMigrate;
        }
        Err(e) => {
            warn!(
                "Migration: failed to read snapshots directory {:?}: {}",
                snapshots_root, e
            );
            return LegacyMigrationOutcome::Deferred;
        }
    };

    let mut migrated_any = false;
    let mut retry_needed = false;
    let mut logged_migration_start = false;
    let mut workspace_entries: Vec<WorkspaceEntry> = Vec::new();
    let mut migrated_legacy: Vec<(String, std::path::PathBuf)> = Vec::new();
    let new_indexes_root = state_dir.join(INDEXES_DIR);

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(
                    "Migration: failed to read directory entry in {:?}: {}",
                    snapshots_root, e
                );
                retry_needed = true;
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                warn!("Migration: failed to inspect entry {:?}: {}", path, e);
                retry_needed = true;
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }

        let ws_id = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => continue,
        };

        let old_index_path = path.join(INDEX_FILE);
        let old_index_meta = match fs::symlink_metadata(&old_index_path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                warn!(
                    "Migration: failed to inspect old index path {:?}: {}",
                    old_index_path, e
                );
                retry_needed = true;
                continue;
            }
        };
        if old_index_meta.is_dir() || old_index_path.is_dir() {
            warn!(
                "Migration: skipping directory at old index path {:?}",
                old_index_path
            );
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
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && old_index_meta.file_type().is_symlink() =>
            {
                warn!(
                    "Migration: skipping dangling symlink at old index path {:?}",
                    old_index_path
                );
                continue;
            }
            Err(e) => {
                warn!(
                    "Migration: failed to read old index file {:?}: {}",
                    old_index_path, e
                );
                retry_needed = true;
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
            retry_needed = true;
            continue;
        }

        if let Err(e) = save_index_sync(&new_index_dir, &index) {
            warn!("Migration: failed to save index {:?}: {}", new_index_dir, e);
            retry_needed = true;
            continue;
        }

        // Defer removal until state.json is durably persisted so a mid-loop
        // crash or save failure never strands workspaces without legacy indexes.
        migrated_legacy.push((ws_id.clone(), old_index_path));
        workspace_entries.push(WorkspaceEntry {
            ws_id: ws_id.clone(),
            workspace_path: index.workspace_path.clone(),
            registered_at: chrono::Utc::now(),
            origin_backend: backend.backend_type(),
        });
        migrated_any = true;
    }

    if retry_needed {
        warn!(
            "Migration incomplete due to I/O failures; preserving legacy indexes and skipping state.json commit for retry"
        );
        return LegacyMigrationOutcome::Deferred;
    }

    if !migrated_any {
        return LegacyMigrationOutcome::NothingToMigrate;
    }

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
        warn!(
            "Migration: failed to save state.json: {:#}; keeping legacy indexes for retry",
            e
        );
        return LegacyMigrationOutcome::Deferred;
    }

    info!("Migration complete, state.json generated");
    for (ws_name, old_index_path) in migrated_legacy {
        if let Err(e) = fs::remove_file(&old_index_path) {
            warn!(
                "Migration: failed to remove old index file {:?}: {}",
                old_index_path, e
            );
        } else {
            info!("Legacy index migration completed for workspace {}", ws_name);
        }
    }

    LegacyMigrationOutcome::Migrated
}

/// Migrate old position index.json files to the new state_dir/indexes/ directory.
///
/// When state.json does not exist (upgrade scenario), scan old position and migrate.
/// Returns `true` if migration completed and committed `state.json`.
pub fn migrate_legacy_indexes(backend: &dyn StorageBackend, state_dir: &Path) -> bool {
    migrate_legacy_indexes_outcome(backend, state_dir) == LegacyMigrationOutcome::Migrated
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
            recovered_orphans: Default::default(),
        }
    }

    fn write_old_index(snapshots_root: &Path, ws_id: &str, index: &SnapshotIndex) {
        let ws_dir = snapshots_root.join(ws_id);
        fs::create_dir_all(&ws_dir).unwrap();
        let content = serde_json::to_string_pretty(index).unwrap();
        fs::write(ws_dir.join(crate::INDEX_FILE), content).unwrap();
    }

    #[test]
    fn migrate_empty_dir_returns_nothing_to_migrate() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert_eq!(
            migrate_legacy_indexes_outcome(&backend, state_dir.path()),
            LegacyMigrationOutcome::NothingToMigrate
        );
        assert!(!migrate_legacy_indexes(&backend, state_dir.path()));
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
    }

    #[test]
    fn migrate_missing_snapshots_root_returns_nothing_to_migrate() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let backend = MockBackend {
            data_root: temp.path().to_path_buf(),
            snapshots_root: temp.path().join("does-not-exist"),
        };
        assert_eq!(
            migrate_legacy_indexes_outcome(&backend, state_dir.path()),
            LegacyMigrationOutcome::NothingToMigrate
        );
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_snapshots_root_returns_deferred() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let snap_dir = temp.path().join("snapshots");
        let state_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(&snap_dir).unwrap();
        let orig_mode = fs::metadata(&snap_dir).unwrap().permissions();
        fs::set_permissions(&snap_dir, fs::Permissions::from_mode(0o000)).unwrap();

        // Skip under root/CAP_DAC_OVERRIDE where 0o000 remains readable.
        if fs::read_dir(&snap_dir).is_ok() {
            fs::set_permissions(&snap_dir, orig_mode).unwrap();
            return;
        }

        let backend = MockBackend {
            data_root: temp.path().to_path_buf(),
            snapshots_root: snap_dir.clone(),
        };
        let outcome = migrate_legacy_indexes_outcome(&backend, state_dir.path());
        fs::set_permissions(&snap_dir, orig_mode).unwrap();

        assert_eq!(outcome, LegacyMigrationOutcome::Deferred);
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

    #[test]
    fn failed_state_save_keeps_legacy_index_for_retry() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        write_old_index(snap_dir.path(), "ws-a", &make_index("/a"));

        let blocker = state_dir.path().join(format!("{}.tmp", crate::STATE_FILE));
        fs::create_dir(&blocker).unwrap();

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert_eq!(
            migrate_legacy_indexes_outcome(&backend, state_dir.path()),
            LegacyMigrationOutcome::Deferred
        );
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
        let legacy = snap_dir.path().join("ws-a").join(crate::INDEX_FILE);
        assert!(
            legacy.exists(),
            "legacy index must survive until state.json is committed"
        );

        fs::remove_dir(&blocker).unwrap();
        assert_eq!(
            migrate_legacy_indexes_outcome(&backend, state_dir.path()),
            LegacyMigrationOutcome::Migrated
        );
        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        assert_eq!(sf.workspaces.len(), 1);
        assert_eq!(sf.workspaces[0].ws_id, "ws-a");
        assert!(!legacy.exists());
    }

    #[test]
    fn partial_index_write_failure_does_not_commit_partial_state() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        write_old_index(snap_dir.path(), "ws-a", &make_index("/a"));
        write_old_index(snap_dir.path(), "ws-b", &make_index("/b"));

        let indexes_dir = state_dir.path().join(crate::INDEXES_DIR);
        fs::create_dir_all(&indexes_dir).unwrap();
        let blocker = indexes_dir.join("ws-b");
        fs::write(&blocker, "not-a-dir").unwrap();

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert_eq!(
            migrate_legacy_indexes_outcome(&backend, state_dir.path()),
            LegacyMigrationOutcome::Deferred
        );
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
        let legacy_a = snap_dir.path().join("ws-a").join(crate::INDEX_FILE);
        let legacy_b = snap_dir.path().join("ws-b").join(crate::INDEX_FILE);
        assert!(legacy_a.exists());
        assert!(legacy_b.exists());

        fs::remove_file(&blocker).unwrap();
        assert_eq!(
            migrate_legacy_indexes_outcome(&backend, state_dir.path()),
            LegacyMigrationOutcome::Migrated
        );
        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        let mut ws_ids: Vec<_> = sf.workspaces.iter().map(|w| w.ws_id.as_str()).collect();
        ws_ids.sort();
        assert_eq!(ws_ids, vec!["ws-a", "ws-b"]);
        assert!(!legacy_a.exists());
        assert!(!legacy_b.exists());
    }

    #[test]
    fn malformed_json_alongside_io_failure_defers_and_recovers_on_retry() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();

        let bad_dir = snap_dir.path().join("ws-bad");
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(bad_dir.join(crate::INDEX_FILE), "not json {{{").unwrap();

        write_old_index(snap_dir.path(), "ws-a", &make_index("/a"));
        write_old_index(snap_dir.path(), "ws-b", &make_index("/b"));

        let indexes_dir = state_dir.path().join(crate::INDEXES_DIR);
        fs::create_dir_all(&indexes_dir).unwrap();
        let blocker = indexes_dir.join("ws-b");
        fs::write(&blocker, "not-a-dir").unwrap();

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(!migrate_legacy_indexes(&backend, state_dir.path()));
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
        let legacy_a = snap_dir.path().join("ws-a").join(crate::INDEX_FILE);
        let legacy_b = snap_dir.path().join("ws-b").join(crate::INDEX_FILE);
        let legacy_bad = bad_dir.join(crate::INDEX_FILE);
        assert!(legacy_a.exists());
        assert!(legacy_b.exists());
        assert!(legacy_bad.exists());

        // Even if ws-a's legacy index is updated before retry, the next
        // migration pass must atomically refresh state_dir/indexes/ws-a/.
        let mut updated_a = make_index("/a");
        updated_a.snapshots.insert(
            "snap-updated".to_string(),
            SnapshotMeta {
                created_at: chrono::Utc::now(),
                message: Some("updated".to_string()),
                metadata: None,
                pinned: false,
                missing: false,
                parent_id: None,
                child_ids: Vec::new(),
            },
        );
        write_old_index(snap_dir.path(), "ws-a", &updated_a);

        fs::remove_file(&blocker).unwrap();
        assert!(migrate_legacy_indexes(&backend, state_dir.path()));
        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        let mut ws_ids: Vec<_> = sf.workspaces.iter().map(|w| w.ws_id.as_str()).collect();
        ws_ids.sort();
        assert_eq!(ws_ids, vec!["ws-a", "ws-b"]);
        assert!(!legacy_a.exists());
        assert!(!legacy_b.exists());
        assert!(legacy_bad.exists(), "malformed legacy file stays untouched");

        let migrated_a_path = indexes_dir.join("ws-a").join(crate::INDEX_FILE);
        let migrated_a: SnapshotIndex =
            serde_json::from_str(&fs::read_to_string(&migrated_a_path).unwrap()).unwrap();
        assert!(migrated_a.snapshots.contains_key("snap-updated"));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_legacy_index_does_not_wedge_valid_workspaces() {
        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();

        let dangling_dir = snap_dir.path().join("ws-dangling");
        fs::create_dir_all(&dangling_dir).unwrap();
        let dangling_link = dangling_dir.join(crate::INDEX_FILE);
        std::os::unix::fs::symlink(snap_dir.path().join("missing/index.json"), &dangling_link)
            .unwrap();

        write_old_index(snap_dir.path(), "ws-good", &make_index("/good"));

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        assert!(migrate_legacy_indexes(&backend, state_dir.path()));
        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        assert_eq!(sf.workspaces.len(), 1);
        assert_eq!(sf.workspaces[0].ws_id, "ws-good");
        assert!(!snap_dir
            .path()
            .join("ws-good")
            .join(crate::INDEX_FILE)
            .exists());
        assert!(fs::symlink_metadata(&dangling_link).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_workspace_dir_defers_commit_until_readable() {
        use std::os::unix::fs::PermissionsExt;

        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        write_old_index(snap_dir.path(), "ws-a", &make_index("/a"));
        write_old_index(snap_dir.path(), "ws-perm", &make_index("/perm"));

        let perm_dir = snap_dir.path().join("ws-perm");
        let perm_index = perm_dir.join(crate::INDEX_FILE);
        let orig_mode = fs::metadata(&perm_dir).unwrap().permissions();
        fs::set_permissions(&perm_dir, fs::Permissions::from_mode(0o000)).unwrap();

        // Skip under root/CAP_DAC_OVERRIDE where 0o000 remains searchable.
        if fs::symlink_metadata(&perm_index).is_ok() {
            fs::set_permissions(&perm_dir, orig_mode).unwrap();
            return;
        }

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        let migrated_while_locked = migrate_legacy_indexes(&backend, state_dir.path());
        fs::set_permissions(&perm_dir, orig_mode).unwrap();

        assert!(!migrated_while_locked);
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
        assert!(snap_dir
            .path()
            .join("ws-a")
            .join(crate::INDEX_FILE)
            .exists());
        assert!(perm_index.exists());

        assert!(migrate_legacy_indexes(&backend, state_dir.path()));
        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        let mut ws_ids: Vec<_> = sf.workspaces.iter().map(|w| w.ws_id.as_str()).collect();
        ws_ids.sort();
        assert_eq!(ws_ids, vec!["ws-a", "ws-perm"]);
        assert!(!snap_dir
            .path()
            .join("ws-a")
            .join(crate::INDEX_FILE)
            .exists());
        assert!(!perm_index.exists());
    }

    #[cfg(unix)]
    #[test]
    fn single_workspace_read_failure_preserves_legacy_index_and_retries() {
        use std::os::unix::fs::PermissionsExt;

        let snap_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        write_old_index(snap_dir.path(), "ws-only", &make_index("/only"));

        let legacy = snap_dir.path().join("ws-only").join(crate::INDEX_FILE);
        let orig_mode = fs::metadata(&legacy).unwrap().permissions();
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o000)).unwrap();

        if fs::read_to_string(&legacy).is_ok() {
            fs::set_permissions(&legacy, orig_mode).unwrap();
            return;
        }

        let backend = MockBackend {
            data_root: snap_dir.path().to_path_buf(),
            snapshots_root: snap_dir.path().to_path_buf(),
        };
        let migrated_while_unreadable = migrate_legacy_indexes(&backend, state_dir.path());
        fs::set_permissions(&legacy, orig_mode).unwrap();

        assert!(!migrated_while_unreadable);
        assert!(!state_dir.path().join(crate::STATE_FILE).exists());
        assert!(legacy.exists());

        assert!(migrate_legacy_indexes(&backend, state_dir.path()));
        let sf = persist::load_state(state_dir.path()).unwrap().unwrap();
        assert_eq!(sf.workspaces.len(), 1);
        assert_eq!(sf.workspaces[0].ws_id, "ws-only");
        assert!(!legacy.exists());
    }
}
