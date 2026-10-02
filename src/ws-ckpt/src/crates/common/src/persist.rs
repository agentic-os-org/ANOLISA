//! Daemon status persistence module
//!
//! Define the disk persistence format `DaemonStateFile`
//! and the atomic loading/saving functions.
//! Persistent location: `/var/lib/ws-ckpt/state.json` (managed by systemd StateDirectory).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::BackendType;
use crate::STATE_FILE;

/// Schema version number, used for future compatible upgrade
pub const DAEMON_STATE_VERSION: u32 = 1;

/// Disk persistence format, written to state_dir/state.json
#[derive(Serialize, Deserialize, Debug, Clone)]
#[non_exhaustive]
pub struct DaemonStateFile {
    /// Schema version number, used for future compatible upgrade
    pub version: u32,
    /// Backend identity information
    pub backend: BackendIdentity,
    /// Backend working paths
    pub paths: BackendPaths,
    /// Registered workspace list (does not contain snapshot details)
    pub workspaces: Vec<WorkspaceEntry>,
}

impl DaemonStateFile {
    /// Construct a new DaemonStateFile.
    ///
    /// Because the struct is `#[non_exhaustive]`, external crates cannot use
    /// struct-literal syntax to build it. This constructor is the only
    /// stable way to create instances from outside this crate and keeps
    /// future field additions backward compatible.
    pub fn new(
        version: u32,
        backend: BackendIdentity,
        paths: BackendPaths,
        workspaces: Vec<WorkspaceEntry>,
    ) -> Self {
        Self {
            version,
            backend,
            paths,
            workspaces,
        }
    }
}

/// Backend identity
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BackendIdentity {
    /// Backend type (reusing existing BackendType enum)
    pub backend_type: BackendType,
    /// Selection method: "config-override" | "persisted" | "auto-detect"
    pub selection_method: String,
    /// Selection time
    pub selected_at: DateTime<Utc>,
}

/// Backend-specific paths and runtime state.
/// Each variant carries only the fields relevant to that backend type.
/// JSON format uses internally tagged enum: {"backend": "BtrfsLoop", "mount_path": "...", ...}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "backend")]
pub enum BackendPaths {
    /// BtrfsLoop: loop-mounted btrfs image
    BtrfsLoop {
        /// btrfs img mount point (= data_root for this backend)
        mount_path: PathBuf,
        /// backend data root directory
        data_root: PathBuf,
        /// snapshot subvolume parent directory
        snapshots_root: PathBuf,
        /// loop img state (populated after bootstrap; None during initial save)
        #[serde(default)]
        loop_img: Option<LoopImgState>,
    },
    /// BtrfsBase: native btrfs partition
    BtrfsBase {
        /// btrfs partition mount point
        mount_path: PathBuf,
        /// backend data root directory
        data_root: PathBuf,
        /// snapshot subvolume parent directory
        snapshots_root: PathBuf,
    },
    // Future: DmThin { pool_device: PathBuf, thin_id: u32, data_root: PathBuf, snapshots_root: PathBuf }
}

/// Img state for BtrfsLoop Mode
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LoopImgState {
    /// Image file path
    pub img_path: PathBuf,
    /// Last bootstrap actual size (bytes)
    pub img_size_bytes: u64,
    /// Last used loop device (for diagnostic purposes, re-assigned on each restart)
    pub last_loop_device: Option<String>,
}

/// Central workspace entry
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkspaceEntry {
    /// Workspace ID
    pub ws_id: String,
    /// User original path (symlink path)
    pub workspace_path: PathBuf,
    /// Registration time
    pub registered_at: DateTime<Utc>,
    /// Origin backend type, for detecting orphan workspaces after backend type change
    pub origin_backend: BackendType,
}

/// Load DaemonStateFile from state_dir/state.json.
///
/// - File does not exist: returns `Ok(None)`
/// - File exists but format error: returns `Err`
pub fn load_state(state_dir: &Path) -> Result<Option<DaemonStateFile>> {
    let path = state_dir.join(STATE_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => {
            let state: DaemonStateFile = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse state file: {}", path.display()))?;
            Ok(Some(state))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read state file: {}", path.display())),
    }
}

/// Atomically write `bytes` to `<dir>/<name>` via tmp+fsync+rename+parent-
/// fsync. Optionally create the temp file with `mode` (Unix only).
///
/// Crash semantics: after Ok the target holds the new content — never a
/// half-written file. The parent-dir fsync is best-effort: it runs *after*
/// the rename already succeeded, so propagating its error would
/// leave the file durably on disk while memory rejects the update, causing
/// memory-vs-disk drift. We log it at `warn!` instead.
///
/// `mode` is applied both via `OpenOptions::mode()` at create time AND via
/// an explicit `fchmod` on the open handle. The fchmod is required because
/// `create(true).truncate(true)` reuses the inode of a stale `<name>.tmp`
/// left by a crashed prior write, inheriting its old (possibly 0o644)
/// permissions — `OpenOptions::mode()` only fires on a fresh create.
/// Ignored on non-Unix.
pub fn atomic_write(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    #[cfg_attr(not(unix), allow(unused_variables))] mode: Option<u32>,
) -> Result<()> {
    let target = atomic_write_and_rename(dir, name, bytes, mode)?;

    // Best-effort: a fsync_dir failure doesn't roll back the rename, and
    // propagating it as Err would cause the memory-vs-disk drift described
    // in the docstring above.
    if let Err(e) = fsync_dir(dir) {
        tracing::warn!(
            "atomic_write: rename of {} succeeded but parent dir fsync failed: {:#} \
             (data is in the right place; durability across a crash is best-effort on this fs)",
            target.display(),
            e
        );
    }
    Ok(())
}

/// Atomically writes a file and requires both content and directory-entry durability.
///
/// Unlike [`atomic_write`], a parent-directory fsync failure is returned to the
/// caller. Because the rename precedes that fsync, the target may already contain
/// the new bytes when this function returns an error.
pub fn atomic_write_strict(dir: &Path, name: &str, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    atomic_write_strict_with_dir_sync(dir, name, bytes, mode, fsync_dir)
}

fn atomic_write_strict_with_dir_sync<F>(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    mode: Option<u32>,
    sync_dir: F,
) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    atomic_write_and_rename(dir, name, bytes, mode)?;
    sync_dir(dir)
}

fn atomic_write_and_rename(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    #[cfg_attr(not(unix), allow(unused_variables))] mode: Option<u32>,
) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create directory: {}", dir.display()))?;

    let target = dir.join(name);
    let tmp = dir.join(format!("{}.tmp", name));

    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(m);
        }
        let mut file = opts
            .open(&tmp)
            .with_context(|| format!("Failed to create temp file: {}", tmp.display()))?;
        // fchmod even if file pre-existed: OpenOptions::mode() only applies on create.
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(m))
                .with_context(|| format!("Failed to chmod temp file: {}", tmp.display()))?;
        }
        file.write_all(bytes)
            .with_context(|| format!("Failed to write temp file: {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to fsync temp file: {}", tmp.display()))?;
    }

    fs::rename(&tmp, &target).with_context(|| {
        format!(
            "Failed to atomically rename: {} -> {}",
            tmp.display(),
            target.display()
        )
    })?;
    Ok(target)
}

/// fsync a directory so a `rename` / `unlink` inside it is durable across a
/// crash. Surfaces the error so the caller can decide (best-effort).
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let f = fs::File::open(dir)
        .with_context(|| format!("Failed to open directory {} for fsync", dir.display()))?;
    f.sync_all()
        .with_context(|| format!("Failed to sync_all on directory {}", dir.display()))?;
    Ok(())
}

/// Atomically write to state.json (write-tmp + fsync + rename + parent fsync).
pub fn save_state(state_dir: &Path, state: &DaemonStateFile) -> Result<()> {
    let content =
        serde_json::to_string_pretty(state).context("Failed to serialize DaemonStateFile")?;
    atomic_write(state_dir, STATE_FILE, content.as_bytes(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;

    /// Construct a complete test DaemonStateFile
    fn sample_state() -> DaemonStateFile {
        DaemonStateFile {
            version: DAEMON_STATE_VERSION,
            backend: BackendIdentity {
                backend_type: BackendType::BtrfsLoop,
                selection_method: "auto-detect".to_string(),
                selected_at: Utc::now(),
            },
            paths: BackendPaths::BtrfsLoop {
                mount_path: PathBuf::from("/mnt/btrfs-workspace"),
                data_root: PathBuf::from("/mnt/btrfs-workspace"),
                snapshots_root: PathBuf::from("/mnt/btrfs-workspace/snapshots"),
                loop_img: Some(LoopImgState {
                    img_path: PathBuf::from("/var/lib/ws-ckpt/btrfs-data.img"),
                    img_size_bytes: 30 * 1024 * 1024 * 1024,
                    last_loop_device: Some("/dev/loop0".to_string()),
                }),
            },
            workspaces: vec![WorkspaceEntry {
                ws_id: "ws-a3f2b1".to_string(),
                workspace_path: PathBuf::from("/home/user/project"),
                registered_at: Utc::now(),
                origin_backend: BackendType::BtrfsLoop,
            }],
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let state = sample_state();
        save_state(dir.path(), &state).unwrap();
        let loaded = load_state(dir.path())
            .unwrap()
            .expect("Should be able to load state");
        assert_eq!(loaded.version, DAEMON_STATE_VERSION);
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].ws_id, "ws-a3f2b1");
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_state(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_invalid_json_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        fs::write(&path, "not valid json {{{").unwrap();
        let result = load_state(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn save_empty_workspaces_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonStateFile {
            version: DAEMON_STATE_VERSION,
            backend: BackendIdentity {
                backend_type: BackendType::BtrfsBase,
                selection_method: "config".to_string(),
                selected_at: Utc::now(),
            },
            paths: BackendPaths::BtrfsBase {
                mount_path: PathBuf::from("/mnt/btrfs"),
                data_root: PathBuf::from("/mnt/btrfs"),
                snapshots_root: PathBuf::from("/mnt/btrfs/snapshots"),
            },
            workspaces: vec![],
        };
        save_state(dir.path(), &state).unwrap();
        let loaded = load_state(dir.path())
            .unwrap()
            .expect("Should be able to load state");
        assert_eq!(loaded.version, DAEMON_STATE_VERSION);
        assert!(loaded.workspaces.is_empty());
        match &loaded.paths {
            BackendPaths::BtrfsBase { .. } => {} // OK, no loop_img field
            _ => panic!("Expected BtrfsBase variant"),
        }
    }

    #[test]
    fn atomic_write_no_tmp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let state = sample_state();
        save_state(dir.path(), &state).unwrap();
        // Should not exist .tmp file after successful write
        let tmp_path = dir.path().join(format!("{}.tmp", STATE_FILE));
        assert!(!tmp_path.exists());
    }

    #[test]
    fn atomic_write_strict_persists_content_without_tmp_residue() {
        let dir = tempfile::tempdir().unwrap();
        atomic_write_strict(dir.path(), "evidence.json", b"durable", None).unwrap();

        assert_eq!(
            fs::read(dir.path().join("evidence.json")).unwrap(),
            b"durable"
        );
        assert!(!dir.path().join("evidence.json.tmp").exists());
    }

    #[test]
    fn atomic_write_strict_propagates_parent_fsync_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = atomic_write_strict_with_dir_sync(
            dir.path(),
            "evidence.json",
            b"renamed",
            None,
            |_| anyhow::bail!("injected parent fsync failure"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected parent fsync failure"));
        assert_eq!(
            fs::read(dir.path().join("evidence.json")).unwrap(),
            b"renamed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_chmods_over_stale_tmp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let name = "policy.toml";
        // Simulate a prior crashed write: stale .tmp on disk with 0o644.
        let stale_tmp = dir.path().join(format!("{}.tmp", name));
        fs::write(&stale_tmp, b"old garbage").unwrap();
        fs::set_permissions(&stale_tmp, fs::Permissions::from_mode(0o644)).unwrap();

        atomic_write(dir.path(), name, b"new content", Some(0o600)).unwrap();

        let final_mode = fs::metadata(dir.path().join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            final_mode, 0o600,
            "stale-tmp inode reuse must not leak 0o644"
        );
    }
}
