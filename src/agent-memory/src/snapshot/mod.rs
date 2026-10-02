//! Phase 6.3: snapshot subsystem.
//!
//! Snapshots are point-in-time copies of the mount root, stored under
//! `<mount>/.anolisa/snapshots/<id>.tar.gz`. We deliberately keep this
//! module backend-agnostic: today we only ship a tar.gz writer, but
//! `detect_btrfs()` is in place so future Btrfs subvol snapshots can
//! slot in transparently.

pub mod tar;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ns::MountPoint;

/// Subdirectory under `.anolisa/` where archives live.
pub const SNAPSHOTS_DIR: &str = "snapshots";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    /// Stable id derived from filename (no extension).
    pub id: String,
    /// Optional human label provided at creation time (== id when omitted).
    pub name: String,
    /// Creation time (RFC3339 UTC).
    pub created_at: String,
    /// Bytes on disk.
    pub size: u64,
    /// File backend (`tar.gz` today; reserved for `btrfs` later).
    pub backend: String,
}

/// Filesystem detect: returns true if `path` lives on a CoW-capable FS
/// that supports cheap subvolume snapshots. Reserved for a future Btrfs
/// subvol backend; current implementations all return false → tar.gz
/// path.
pub fn detect_btrfs(_path: &Path) -> bool {
    false
}

pub fn snapshots_dir(mount: &MountPoint) -> PathBuf {
    mount.meta_dir.join(SNAPSHOTS_DIR)
}

/// Build a fresh snapshot id of the form `snap_<ULID>`.
pub fn new_snapshot_id() -> String {
    format!("snap_{}", ulid::Ulid::new())
}

/// Create a snapshot using whatever backend best suits the mount.
/// `name` is optional; when None the id doubles as the display name.
pub fn create(mount: &MountPoint, name: Option<&str>) -> Result<SnapshotInfo> {
    let id = new_snapshot_id();
    let display = name.unwrap_or(&id).to_string();

    if detect_btrfs(&mount.root) {
        // Reserved for future Btrfs subvol snapshot backend.
        // Falls through to tar for now.
    }
    tar::create_tarball(mount, &id, &display)
}

pub fn list(mount: &MountPoint) -> Result<Vec<SnapshotInfo>> {
    tar::list_tarballs(mount)
}

pub fn restore(mount: &MountPoint, id: &str) -> Result<()> {
    tar::restore_tarball(mount, id)
}
