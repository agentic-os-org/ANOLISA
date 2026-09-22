//! Backup planning skeleton.
//!
//! At P1-A this module only **plans** where backups would be stored —
//! actual filesystem copies, checksums, and restore are wired in later
//! milestones together with `Transaction` (launch spec §8.2 / §8.3).
//!
//! The plan maps every input `src` path to a stable location under
//! `<backup_root>/<id>/<safe-basename>-<path-hash>`. The stored name keeps
//! a readable basename but keys uniqueness on the full source path, so
//! paths that flatten to the same separator-replaced string cannot collide.
//! All entries remain namespaced under the backup id directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BACKUP_BASENAME_MAX_CHARS: usize = 96;

/// A planned (or completed) backup set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSet {
    /// Stable backup id (timestamp- or uuid-based).
    pub id: String,
    /// ISO8601 UTC timestamp when the plan was created.
    pub created_at: String,
    /// One entry per source path captured by this set.
    pub entries: Vec<BackupEntry>,
}

/// A single backup mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    /// Original on-disk path the operator wants to protect.
    pub src: PathBuf,
    /// Where the copy would live under the backup root.
    pub stored_at: PathBuf,
    /// Recorded checksum once the copy completes (planning phase: None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl BackupSet {
    /// Build a plan that maps every `paths` entry to a target under
    /// `<backup_root>/<id>/`.
    pub fn plan(id: String, paths: Vec<PathBuf>, backup_root: &Path) -> Self {
        let root = backup_root.join(&id);
        let entries = paths
            .into_iter()
            .map(|src| {
                let stored_at = root.join(flatten_src(&src));
                BackupEntry {
                    src,
                    stored_at,
                    sha256: None,
                }
            })
            .collect();
        Self {
            id,
            created_at: chrono::Utc::now().to_rfc3339(),
            entries,
        }
    }
}

/// Convert a source path to `<safe-basename>-<path-hash>` so it can live
/// alongside other captured files in the backup root without path collisions.
fn flatten_src(path: &Path) -> String {
    let basename = path
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("root");
    format!(
        "{}-{}",
        sanitize_filename_part(basename),
        hash_path_hex(path)
    )
}

fn sanitize_filename_part(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.len() >= BACKUP_BASENAME_MAX_CHARS {
            break;
        }
        let safe = if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            ch
        } else {
            '_'
        };
        out.push(safe);
    }
    if !out.chars().any(|ch| ch.is_ascii_alphanumeric()) || out == "." || out == ".." {
        return "path".to_string();
    }
    out
}

fn hash_path_hex(path: &Path) -> String {
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    {
        hasher.update(path.to_string_lossy().as_bytes());
    }
    to_lower_hex(&hasher.finalize())
}

fn to_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_emits_one_entry_per_path() {
        let backup_root = PathBuf::from("/var/lib/anolisa/backups");
        let plan = BackupSet::plan(
            "op-20260601-001".to_string(),
            vec![
                PathBuf::from("/etc/openclaw/config.json"),
                PathBuf::from("/etc/anolisa/features.toml"),
            ],
            &backup_root,
        );
        assert_eq!(plan.id, "op-20260601-001");
        assert_eq!(plan.entries.len(), 2);
        assert!(plan.entries.iter().all(|e| e.sha256.is_none()));
    }

    #[test]
    fn plan_uses_id_namespaced_stored_at() {
        let backup_root = PathBuf::from("/var/lib/anolisa/backups");
        let plan = BackupSet::plan(
            "op-1".to_string(),
            vec![PathBuf::from("/etc/openclaw/config.json")],
            &backup_root,
        );
        let entry = &plan.entries[0];
        assert_eq!(entry.stored_at.parent().unwrap(), backup_root.join("op-1"));
        assert!(
            entry
                .stored_at
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config.json-")
        );
        assert_eq!(entry.src, PathBuf::from("/etc/openclaw/config.json"));
    }

    #[test]
    fn plan_handles_relative_and_root_paths() {
        let backup_root = PathBuf::from("/tmp/backups");
        let plan = BackupSet::plan(
            "op-2".to_string(),
            vec![PathBuf::from("relative/file"), PathBuf::from("/")],
            &backup_root,
        );
        assert_eq!(
            plan.entries[0].stored_at.parent().unwrap(),
            backup_root.join("op-2")
        );
        assert!(
            plan.entries[0]
                .stored_at
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("file-")
        );
        assert_eq!(
            plan.entries[1].stored_at.parent().unwrap(),
            backup_root.join("op-2")
        );
        assert!(
            plan.entries[1]
                .stored_at
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("root-")
        );
    }

    #[test]
    fn plan_does_not_collide_paths_that_separator_flattening_collided() {
        let backup_root = PathBuf::from("/tmp/backups");
        let plan = BackupSet::plan(
            "op-3".to_string(),
            vec![PathBuf::from("/a/b__c"), PathBuf::from("/a/b/c")],
            &backup_root,
        );

        assert_ne!(plan.entries[0].stored_at, plan.entries[1].stored_at);
        assert_eq!(
            plan.entries[0].stored_at.parent().unwrap(),
            backup_root.join("op-3")
        );
        assert_eq!(
            plan.entries[1].stored_at.parent().unwrap(),
            backup_root.join("op-3")
        );
    }
}
