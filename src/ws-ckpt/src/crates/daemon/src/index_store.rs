use anyhow::Context;
use std::path::Path;
use ws_ckpt_common::{SnapshotIndex, INDEX_FILE};

/// Saves an index with file fsync and best-effort parent-directory fsync.
pub async fn save(ws_dir: &Path, index: &SnapshotIndex) -> anyhow::Result<()> {
    let content =
        serde_json::to_string_pretty(index).context("Failed to serialize SnapshotIndex")?;
    let ws_dir = ws_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // Every later index rewrite must preserve the file durability of any
        // guarded evidence already acknowledged to a caller.
        ws_ckpt_common::persist::atomic_write(&ws_dir, INDEX_FILE, content.as_bytes(), None)
    })
    .await
    .context("index writer task failed")??;
    Ok(())
}

/// Saves an index with strict file and rename durability before returning.
pub async fn save_durable(ws_dir: &Path, index: &SnapshotIndex) -> anyhow::Result<()> {
    let content =
        serde_json::to_string_pretty(index).context("Failed to serialize SnapshotIndex")?;
    let ws_dir = ws_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        ws_ckpt_common::persist::atomic_write_strict(&ws_dir, INDEX_FILE, content.as_bytes(), None)
    })
    .await
    .context("durable index writer task failed")??;
    Ok(())
}

/// Persist an index directory rename before publishing its lifecycle change.
pub async fn sync_parent(index_dir: &Path) -> anyhow::Result<()> {
    let parent = index_dir
        .parent()
        .context("index directory has no parent")?
        .to_path_buf();
    tokio::task::spawn_blocking(move || ws_ckpt_common::persist::fsync_dir(&parent))
        .await
        .context("index directory sync task failed")?
}

/// Load a SnapshotIndex from the index.json file on disk.
pub async fn load(ws_dir: &Path) -> anyhow::Result<SnapshotIndex> {
    let index_path = ws_dir.join(INDEX_FILE);
    let content = tokio::fs::read_to_string(&index_path)
        .await
        .with_context(|| format!("Failed to read {:?}", index_path))?;
    let index: SnapshotIndex = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {:?}", index_path))?;
    Ok(index)
}

/// Reconcile interrupted snapshot mutations without discarding protected records.
pub(crate) async fn reconcile_from_fs(
    ws_dir: &Path,
    index: &mut SnapshotIndex,
) -> anyhow::Result<bool> {
    let recovered = match tokio::fs::try_exists(ws_dir).await? {
        true => rebuild_from_fs(ws_dir, index.workspace_path.clone()).await?,
        false => SnapshotIndex::new(index.workspace_path.clone()),
    };
    let mut changed = false;
    let mut absent = std::collections::HashSet::new();
    for (id, meta) in &mut index.snapshots {
        // Unlike Path::exists, IO failures must not be treated as data loss.
        let missing = !tokio::fs::try_exists(ws_dir.join(id))
            .await
            .with_context(|| format!("inspect snapshot {id:?} in {ws_dir:?}"))?;
        changed |= meta.missing != missing;
        meta.missing = missing;
        if missing && !meta.pinned && !index.governed_evidence.contains_key(id) {
            tracing::warn!("Pruning missing snapshot {id} from {ws_dir:?}");
            absent.insert(id.clone());
        }
    }
    changed |= !absent.is_empty();
    index.prune_chain(&absent);
    index.snapshots.retain(|id, _| !absent.contains(id));
    for (id, meta) in recovered.snapshots {
        // A retained receipt must never identify an unverified orphan as the
        // original guarded checkpoint. Orphans require explicit deletion.
        if !index.snapshots.contains_key(&id) && !index.governed_evidence.contains_key(&id) {
            tracing::warn!("Recovering orphan snapshot {id} from {ws_dir:?}");
            index.recovered_orphans.insert(id.clone());
            index.snapshots.insert(id, meta);
            changed = true;
        }
    }
    Ok(changed)
}

/// Rebuild a SnapshotIndex from the filesystem directory structure.
/// Directory names are snapshot IDs, including hidden names and `index.json`.
pub async fn rebuild_from_fs(
    ws_dir: &Path,
    workspace_path: std::path::PathBuf,
) -> anyhow::Result<SnapshotIndex> {
    use ws_ckpt_common::SnapshotMeta;
    let mut index = SnapshotIndex::new(workspace_path);
    let mut entries = tokio::fs::read_dir(ws_dir)
        .await
        .with_context(|| format!("Failed to read directory {:?}", ws_dir))?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        // Metadata files are not directories; their names are also valid IDs.
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        // Rebuild with minimal metadata (message lost)
        let meta = SnapshotMeta {
            message: None,
            metadata: None,
            pinned: true,
            created_at: chrono::Utc::now(),
            missing: false,
            parent_id: None,
            child_ids: vec![],
        };
        index.recovered_orphans.insert(name.clone());
        index.snapshots.insert(name, meta);
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use ws_ckpt_common::{SnapshotIndex, SnapshotMeta};

    #[tokio::test]
    async fn reconcile_rejects_scan_errors_and_prunes_an_absent_head() {
        let dir = tempdir().unwrap();
        let snapshots = dir.path().join("snapshots");
        let mut index = SnapshotIndex::new(PathBuf::from("/ws"));
        index.head = Some("gone".into());
        index.snapshots.insert(
            "gone".into(),
            SnapshotMeta {
                message: None,
                metadata: None,
                pinned: false,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![ws_ckpt_common::LIVE_CHILD.into()],
            },
        );
        std::fs::write(&snapshots, "not a directory").unwrap();
        assert!(reconcile_from_fs(&snapshots, &mut index).await.is_err());
        assert!(index.snapshots.contains_key("gone"));
        std::fs::remove_file(&snapshots).unwrap();
        assert!(reconcile_from_fs(&snapshots, &mut index).await.unwrap());
        assert!(index.snapshots.is_empty());
        assert!(index.head.is_none());
        assert!(!reconcile_from_fs(&snapshots, &mut index).await.unwrap());
    }

    #[tokio::test]
    async fn save_and_load_round_trip() {
        let dir = tempdir().unwrap();
        let mut index = SnapshotIndex::new(PathBuf::from("/tmp/test-ws"));
        index.snapshots.insert(
            "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            SnapshotMeta {
                message: Some("initial snapshot".to_string()),
                metadata: Some(serde_json::json!({"event": "init"})),
                pinned: true,
                created_at: chrono::Utc::now(),
                missing: false,
                parent_id: None,
                child_ids: vec![],
            },
        );

        // Save
        save(dir.path(), &index).await.expect("save failed");

        // Load
        let loaded = load(dir.path()).await.expect("load failed");
        assert_eq!(loaded.workspace_path, index.workspace_path);
        assert_eq!(loaded.snapshots.len(), 1);
        assert!(loaded
            .snapshots
            .contains_key("abcdef1234567890abcdef1234567890abcdef12"));
        let meta = &loaded.snapshots["abcdef1234567890abcdef1234567890abcdef12"];
        assert_eq!(meta.message.as_deref(), Some("initial snapshot"));
        assert!(meta.pinned);
    }

    #[tokio::test]
    async fn save_atomicity_no_tmp_residue() {
        // After save, index.json should exist but index.json.tmp should NOT
        let dir = tempdir().unwrap();
        let index = SnapshotIndex::new(PathBuf::from("/ws"));
        save(dir.path(), &index).await.expect("save failed");

        assert!(dir.path().join(INDEX_FILE).exists());
        assert!(
            !dir.path().join(format!("{}.tmp", INDEX_FILE)).exists(),
            "tmp file should not remain after save"
        );
    }

    #[tokio::test]
    async fn load_nonexistent_file_returns_error() {
        let dir = tempdir().unwrap();
        let result = load(dir.path()).await;
        assert!(result.is_err(), "loading from empty dir should fail");
    }

    #[tokio::test]
    async fn rebuild_from_fs_finds_all_snapshot_dirs() {
        let dir = tempdir().unwrap();
        // Create directories with various naming patterns
        std::fs::create_dir(dir.path().join("abcdef1234567890abcdef1234567890abcdef12")).unwrap();
        std::fs::create_dir(dir.path().join("1111111111111111111111111111111111111111")).unwrap();
        std::fs::create_dir(dir.path().join("msg1-step0")).unwrap();
        std::fs::create_dir(dir.path().join("my-snapshot")).unwrap();

        let index = rebuild_from_fs(dir.path(), PathBuf::from("/ws"))
            .await
            .expect("rebuild_from_fs failed");

        assert_eq!(index.snapshots.len(), 4);
        assert!(index
            .snapshots
            .contains_key("abcdef1234567890abcdef1234567890abcdef12"));
        assert!(index
            .snapshots
            .contains_key("1111111111111111111111111111111111111111"));
        assert!(index.snapshots.contains_key("msg1-step0"));
        assert!(index.snapshots.contains_key("my-snapshot"));
    }

    #[tokio::test]
    async fn rebuild_from_fs_includes_hidden_ids_but_ignores_files() {
        let dir = tempdir().unwrap();
        // Create matching and non-matching entries
        std::fs::create_dir(dir.path().join("abcdef1234567890abcdef1234567890abcdef12")).unwrap();
        std::fs::create_dir(dir.path().join("msg1-step0")).unwrap();
        std::fs::create_dir(dir.path().join("my-snapshot")).unwrap();
        // Hidden names are valid snapshot IDs
        std::fs::create_dir(dir.path().join(".hidden")).unwrap();
        // Regular file should be ignored
        std::fs::write(dir.path().join("index.json"), "{}").unwrap();

        let index = rebuild_from_fs(dir.path(), PathBuf::from("/ws"))
            .await
            .expect("rebuild_from_fs failed");

        assert_eq!(index.snapshots.len(), 4);
        assert!(index.snapshots.contains_key(".hidden"));
        assert!(index
            .snapshots
            .contains_key("abcdef1234567890abcdef1234567890abcdef12"));
        assert!(index.snapshots.contains_key("msg1-step0"));
        assert!(index.snapshots.contains_key("my-snapshot"));
    }
}
