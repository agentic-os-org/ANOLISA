//! Startup recovery for Btrfs rollback temporary subvolumes.
//!
//! Ambiguous state stops backend bootstrap because ws-ckpt has no durable
//! rollback commit record with which to choose between two live candidates.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use nix::fcntl::{renameat2, RenameFlags};
use tokio::process::Command;
use tracing::{info, warn};
use ws_ckpt_common::SNAPSHOTS_DIR;

const ROLLBACK_TMP_SUFFIX: &str = ".rollback-tmp";

#[derive(Clone, Debug, PartialEq, Eq)]
enum RollbackPathState {
    Missing,
    Subvolume,
    Unsafe(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RollbackRecoveryOutcome {
    Noop,
    RestoredLive,
}

#[async_trait]
trait RollbackRecoveryOps: Sync {
    async fn inspect(&self, path: &Path) -> Result<RollbackPathState>;
    async fn rename_noreplace(&self, from: &Path, to: &Path) -> Result<()>;
}

struct BtrfsRollbackRecoveryOps;

#[async_trait]
impl RollbackRecoveryOps for BtrfsRollbackRecoveryOps {
    async fn inspect(&self, path: &Path) -> Result<RollbackPathState> {
        let metadata = match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RollbackPathState::Missing)
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to inspect {}", path.display()))
            }
        };

        if metadata.file_type().is_symlink() {
            return Ok(RollbackPathState::Unsafe("a symlink".to_string()));
        }
        if !metadata.is_dir() {
            return Ok(RollbackPathState::Unsafe(
                "not a directory or Btrfs subvolume".to_string(),
            ));
        }

        let output = Command::new("btrfs")
            .args(["subvolume", "show"])
            .arg(path)
            .output()
            .await
            .with_context(|| format!("failed to verify Btrfs subvolume {}", path.display()))?;
        if output.status.success() {
            return Ok(RollbackPathState::Subvolume);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(RollbackPathState::Unsafe(format!(
            "not a verified Btrfs subvolume (btrfs exited with {}: {})",
            output.status,
            stderr.trim()
        )))
    }

    async fn rename_noreplace(&self, from: &Path, to: &Path) -> Result<()> {
        rename_noreplace(from, to).await
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RollbackTmpName {
    NotCandidate,
    Workspace(OsString),
    Malformed,
}

fn classify_rollback_tmp_name(name: &OsStr) -> RollbackTmpName {
    let Some(ws_id) = name.as_bytes().strip_suffix(ROLLBACK_TMP_SUFFIX.as_bytes()) else {
        return RollbackTmpName::NotCandidate;
    };
    if ws_id.is_empty() || ws_id == b"." || ws_id == b".." {
        return RollbackTmpName::Malformed;
    }
    RollbackTmpName::Workspace(OsString::from_vec(ws_id.to_vec()))
}

async fn has_workspace_ownership_marker(data_root: &Path, ws_id: &OsStr) -> Result<bool> {
    let marker = data_root.join(SNAPSHOTS_DIR).join(ws_id);
    match tokio::fs::symlink_metadata(&marker).await {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| {
            format!(
                "failed to inspect rollback ownership marker {}",
                marker.display()
            )
        }),
    }
}

async fn recover_interrupted_rollback_with<O: RollbackRecoveryOps>(
    data_root: &Path,
    ws_id: &OsStr,
    ops: &O,
) -> Result<RollbackRecoveryOutcome> {
    let live_path = data_root.join(ws_id);
    let mut tmp_name = ws_id.to_os_string();
    tmp_name.push(ROLLBACK_TMP_SUFFIX);
    let tmp_path = data_root.join(tmp_name);

    let tmp_state = ops
        .inspect(&tmp_path)
        .await
        .with_context(|| format!("failed to inspect rollback temporary path {tmp_path:?}"))?;
    if tmp_state == RollbackPathState::Missing {
        return Ok(RollbackRecoveryOutcome::Noop);
    }

    let live_state = ops
        .inspect(&live_path)
        .await
        .with_context(|| format!("failed to inspect live workspace path {live_path:?}"))?;

    match (live_state, tmp_state) {
        (RollbackPathState::Missing, RollbackPathState::Subvolume) => {
            ops.rename_noreplace(&tmp_path, &live_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to restore interrupted rollback {tmp_path:?} -> {live_path:?}; \
                         temporary subvolume was retained"
                    )
                })?;
            info!(
                "Restored live workspace from interrupted rollback: {:?} -> {:?}",
                tmp_path, live_path
            );
            Ok(RollbackRecoveryOutcome::RestoredLive)
        }
        (live_state, tmp_state) => bail!(
            "ambiguous interrupted rollback for workspace {ws_id:?}: live path {live_path:?} is \
             {live_state:?}, temporary path {tmp_path:?} is {tmp_state:?}; refusing to rename or \
             delete either path"
        ),
    }
}

/// Resolves all interrupted rollbacks before workspace state is rebuilt.
///
/// Any owned but ambiguous entry fails the whole backend bootstrap. This is a
/// deliberate coarse fail-closed boundary until per-workspace quarantine and
/// durable rollback commit evidence exist.
pub(crate) async fn recover_interrupted_rollbacks(data_root: &Path) -> Result<()> {
    recover_interrupted_rollbacks_with(data_root, &BtrfsRollbackRecoveryOps).await
}

async fn recover_interrupted_rollbacks_with<O: RollbackRecoveryOps>(
    data_root: &Path,
    ops: &O,
) -> Result<()> {
    let mut entries = match tokio::fs::read_dir(data_root).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("failed to scan backend data root {data_root:?}"))
        }
    };

    while let Some(entry) = entries
        .next_entry()
        .await
        .with_context(|| format!("failed to scan backend data root {data_root:?}"))?
    {
        let name = entry.file_name();
        match classify_rollback_tmp_name(&name) {
            RollbackTmpName::NotCandidate => {}
            RollbackTmpName::Workspace(ws_id) => {
                if has_workspace_ownership_marker(data_root, &ws_id).await? {
                    recover_interrupted_rollback_with(data_root, &ws_id, ops).await?;
                } else {
                    warn!(
                        "Leaving unowned rollback temporary entry untouched: {:?}",
                        entry.path()
                    );
                }
            }
            RollbackTmpName::Malformed => warn!(
                "Leaving malformed rollback temporary entry untouched: {:?}",
                entry.path()
            ),
        }
    }

    Ok(())
}

async fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    if !from.is_absolute() || !to.is_absolute() {
        bail!("rollback recovery rename requires absolute paths: from={from:?}, to={to:?}");
    }

    let from = from.to_path_buf();
    let to = to.to_path_buf();
    tokio::task::spawn_blocking(move || {
        renameat2(
            None,
            from.as_path(),
            None,
            to.as_path(),
            RenameFlags::RENAME_NOREPLACE,
        )
        .with_context(|| format!("failed to rename {from:?} to {to:?} without replacement"))
    })
    .await
    .context("rollback recovery rename task failed")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeRollbackOps {
        states: Mutex<HashMap<PathBuf, RollbackPathState>>,
        calls: Mutex<Vec<String>>,
        fail_rename: bool,
    }

    impl FakeRollbackOps {
        fn with_states(states: impl IntoIterator<Item = (PathBuf, RollbackPathState)>) -> Self {
            Self {
                states: Mutex::new(states.into_iter().collect()),
                ..Self::default()
            }
        }

        fn state(&self, path: &Path) -> RollbackPathState {
            self.states
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .unwrap_or(RollbackPathState::Missing)
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl RollbackRecoveryOps for FakeRollbackOps {
        async fn inspect(&self, path: &Path) -> Result<RollbackPathState> {
            Ok(self.state(path))
        }

        async fn rename_noreplace(&self, from: &Path, to: &Path) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("rename:{}->{}", from.display(), to.display()));
            if self.fail_rename {
                bail!("injected rename failure");
            }

            let mut states = self.states.lock().unwrap();
            if states.contains_key(to) {
                bail!("destination exists");
            }
            let state = states
                .remove(from)
                .ok_or_else(|| anyhow::anyhow!("source missing"))?;
            states.insert(to.to_path_buf(), state);
            Ok(())
        }
    }

    fn rollback_paths(root: &Path) -> (PathBuf, PathBuf) {
        (root.join("ws-abc123"), root.join("ws-abc123.rollback-tmp"))
    }

    #[test]
    fn rollback_tmp_name_accepts_current_and_legacy_workspace_ids() {
        for name in [
            "ws-abc123.rollback-tmp",
            "ws-012def-2.rollback-tmp",
            "ws-abc.rollback-tmp",
            "legacy-project.rollback-tmp",
        ] {
            assert!(matches!(
                classify_rollback_tmp_name(std::ffi::OsStr::new(name)),
                RollbackTmpName::Workspace(_)
            ));
        }
    }

    #[test]
    fn rollback_tmp_name_rejects_unsafe_and_unrelated_names() {
        for name in [".rollback-tmp", "..rollback-tmp", "...rollback-tmp"] {
            assert_eq!(
                classify_rollback_tmp_name(std::ffi::OsStr::new(name)),
                RollbackTmpName::Malformed,
                "{name}"
            );
        }
        assert_eq!(
            classify_rollback_tmp_name(std::ffi::OsStr::new("ws-abc123")),
            RollbackTmpName::NotCandidate
        );
        let non_utf8 = std::ffi::OsString::from_vec(
            [b"ws-abc12\xff".as_slice(), ROLLBACK_TMP_SUFFIX.as_bytes()].concat(),
        );
        assert_eq!(
            classify_rollback_tmp_name(&non_utf8),
            RollbackTmpName::Workspace(std::ffi::OsString::from_vec(b"ws-abc12\xff".to_vec()))
        );
    }

    #[tokio::test]
    async fn interrupted_rollback_restores_tmp_when_live_is_missing() {
        let root = Path::new("/backend");
        let (live, tmp) = rollback_paths(root);
        let ops = FakeRollbackOps::with_states([(tmp.clone(), RollbackPathState::Subvolume)]);

        let outcome = recover_interrupted_rollback_with(root, OsStr::new("ws-abc123"), &ops)
            .await
            .unwrap();

        assert_eq!(outcome, RollbackRecoveryOutcome::RestoredLive);
        assert_eq!(ops.state(&live), RollbackPathState::Subvolume);
        assert_eq!(ops.state(&tmp), RollbackPathState::Missing);
        assert_eq!(
            ops.calls(),
            vec![format!("rename:{}->{}", tmp.display(), live.display())]
        );
    }

    #[tokio::test]
    async fn both_valid_subvolumes_fail_closed_without_commit_evidence() {
        let root = Path::new("/backend");
        let (live, tmp) = rollback_paths(root);
        let ops = FakeRollbackOps::with_states([
            (live.clone(), RollbackPathState::Subvolume),
            (tmp.clone(), RollbackPathState::Subvolume),
        ]);

        let error = recover_interrupted_rollback_with(root, OsStr::new("ws-abc123"), &ops)
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("ambiguous interrupted rollback"));
        assert_eq!(ops.state(&live), RollbackPathState::Subvolume);
        assert_eq!(ops.state(&tmp), RollbackPathState::Subvolume);
        assert!(ops.calls().is_empty(), "neither subvolume may be deleted");
    }

    #[tokio::test]
    async fn recovery_is_idempotent_after_restore() {
        let root = Path::new("/backend");
        let (_, tmp) = rollback_paths(root);
        let restore_ops = FakeRollbackOps::with_states([(tmp, RollbackPathState::Subvolume)]);
        recover_interrupted_rollback_with(root, OsStr::new("ws-abc123"), &restore_ops)
            .await
            .unwrap();
        assert_eq!(
            recover_interrupted_rollback_with(root, OsStr::new("ws-abc123"), &restore_ops)
                .await
                .unwrap(),
            RollbackRecoveryOutcome::Noop
        );
    }

    #[tokio::test]
    async fn ambiguous_rollback_states_fail_closed() {
        let root = Path::new("/backend");
        let (live, tmp) = rollback_paths(root);
        let unsafe_states = [
            (
                RollbackPathState::Unsafe("a symlink".to_string()),
                RollbackPathState::Subvolume,
            ),
            (
                RollbackPathState::Unsafe("not a subvolume".to_string()),
                RollbackPathState::Subvolume,
            ),
            (
                RollbackPathState::Missing,
                RollbackPathState::Unsafe("a symlink".to_string()),
            ),
            (
                RollbackPathState::Subvolume,
                RollbackPathState::Unsafe("not a subvolume".to_string()),
            ),
        ];

        for (live_state, tmp_state) in unsafe_states {
            let ops = FakeRollbackOps::with_states([
                (live.clone(), live_state.clone()),
                (tmp.clone(), tmp_state.clone()),
            ]);
            let error = recover_interrupted_rollback_with(root, OsStr::new("ws-abc123"), &ops)
                .await
                .unwrap_err();

            assert!(format!("{error:#}").contains("refusing to rename or delete"));
            assert_eq!(ops.state(&live), live_state);
            assert_eq!(ops.state(&tmp), tmp_state);
            assert!(ops.calls().is_empty());
        }
    }

    #[tokio::test]
    async fn recovery_rename_failure_preserves_retryable_state() {
        let root = Path::new("/backend");
        let (live, tmp) = rollback_paths(root);
        let rename_ops = FakeRollbackOps {
            fail_rename: true,
            ..FakeRollbackOps::with_states([(tmp.clone(), RollbackPathState::Subvolume)])
        };

        assert!(
            recover_interrupted_rollback_with(root, OsStr::new("ws-abc123"), &rename_ops)
                .await
                .is_err()
        );
        assert_eq!(rename_ops.state(&live), RollbackPathState::Missing);
        assert_eq!(rename_ops.state(&tmp), RollbackPathState::Subvolume);
    }

    #[tokio::test]
    async fn rename_noreplace_never_overwrites_a_concurrent_live_path() {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("from");
        let to = root.path().join("to");
        tokio::fs::write(&from, b"preserved").await.unwrap();
        tokio::fs::write(&to, b"concurrent").await.unwrap();

        assert!(rename_noreplace(&from, &to).await.is_err());
        assert_eq!(tokio::fs::read(&from).await.unwrap(), b"preserved");
        assert_eq!(tokio::fs::read(&to).await.unwrap(), b"concurrent");
    }

    #[tokio::test]
    async fn rename_noreplace_rejects_relative_paths() {
        let error = rename_noreplace(Path::new("from"), Path::new("to"))
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("requires absolute paths"));
    }

    #[tokio::test]
    async fn scanner_preserves_malformed_and_non_subvolume_entries() {
        let root = tempfile::tempdir().unwrap();
        let malformed = root.path().join("foreign.rollback-tmp");
        let regular_dir = root.path().join("ws-abc123.rollback-tmp");
        tokio::fs::create_dir(&malformed).await.unwrap();
        tokio::fs::create_dir(&regular_dir).await.unwrap();
        tokio::fs::create_dir_all(root.path().join(SNAPSHOTS_DIR).join("ws-abc123"))
            .await
            .unwrap();

        let ops = FakeRollbackOps::with_states([(
            regular_dir.clone(),
            RollbackPathState::Unsafe("not a subvolume".to_string()),
        )]);
        let error = recover_interrupted_rollbacks_with(root.path(), &ops)
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("ambiguous interrupted rollback"));
        assert!(malformed.exists(), "malformed candidate must be preserved");
        assert!(
            regular_dir.exists(),
            "ordinary directory must never be remove_dir_all'ed"
        );
    }

    #[tokio::test]
    async fn unowned_entry_is_preserved_without_blocking_startup() {
        let root = tempfile::tempdir().unwrap();
        let malformed = root.path().join("foreign.rollback-tmp");
        tokio::fs::create_dir(&malformed).await.unwrap();

        recover_interrupted_rollbacks_with(root.path(), &FakeRollbackOps::default())
            .await
            .unwrap();

        assert!(malformed.exists());
    }

    #[tokio::test]
    async fn scanner_restores_owned_legacy_workspace_id() {
        let root = tempfile::tempdir().unwrap();
        let ws_id = OsStr::new("ws-abc");
        let live = root.path().join(ws_id);
        let tmp = root.path().join("ws-abc.rollback-tmp");
        tokio::fs::create_dir(&tmp).await.unwrap();
        tokio::fs::create_dir_all(root.path().join(SNAPSHOTS_DIR).join(ws_id))
            .await
            .unwrap();
        let ops = FakeRollbackOps::with_states([(tmp.clone(), RollbackPathState::Subvolume)]);

        recover_interrupted_rollbacks_with(root.path(), &ops)
            .await
            .unwrap();

        assert_eq!(ops.state(&live), RollbackPathState::Subvolume);
        assert_eq!(ops.state(&tmp), RollbackPathState::Missing);
        assert_eq!(
            ops.calls(),
            vec![format!("rename:{}->{}", tmp.display(), live.display())]
        );
    }

    #[tokio::test]
    async fn real_path_inspection_rejects_symlink_without_following_it() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let candidate = root.path().join("ws-abc123.rollback-tmp");
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::symlink(&target, &candidate).await.unwrap();

        let state = BtrfsRollbackRecoveryOps.inspect(&candidate).await.unwrap();
        assert_eq!(state, RollbackPathState::Unsafe("a symlink".to_string()));
        assert!(
            tokio::fs::symlink_metadata(&candidate)
                .await
                .unwrap()
                .file_type()
                .is_symlink(),
            "symlink candidate must be preserved"
        );
    }
}
