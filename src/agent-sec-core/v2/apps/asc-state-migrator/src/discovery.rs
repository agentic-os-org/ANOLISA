//! Trusted source discovery.
//!
//! A v1 installation leaves state in up to four places (issue #6605): an
//! explicit `AGENT_SEC_DATA_DIR`, `/var/log/agent-sec` (tier 1, root only),
//! `$HOME/.agent-sec-core` (tier 2) and `/tmp/agent-sec-<uid>` (tier 3). The
//! migrator discovers tier 2 and tier 3 directories by scanning, accepts
//! explicit directories from the administrator, and never treats the
//! destination directory itself as a source: tier 1 already is the system
//! store the daemon owns.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// How a source directory entered the candidate list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceOrigin {
    /// Passed with `--source`.
    Explicit,
    /// Found as `<root>/<user>/.agent-sec-core`.
    Homes(PathBuf),
    /// Found as `<root>/agent-sec-<uid>`.
    Tmp(PathBuf),
}

/// A candidate source directory with its verified owner.
#[derive(Debug, Clone)]
pub struct DiscoveredSource {
    /// The source directory.
    pub dir: PathBuf,
    /// How the directory was found.
    pub origin: SourceOrigin,
    /// Owner the imported rows will carry.
    pub owner_uid: u32,
    /// Whether the owner came from `--map-owner` rather than directory stat.
    pub admin_mapped: bool,
    /// The directory's own `uid`, for evidence.
    pub dir_uid: u32,
}

/// A directory that was considered and rejected, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedSource {
    /// The rejected directory.
    pub dir: PathBuf,
    /// Human-readable rejection reason.
    pub reason: String,
}

/// Parsed `--map-owner DIR=UID` entries.
#[derive(Debug, Clone, Default)]
pub struct OwnerMap {
    entries: BTreeMap<PathBuf, u32>,
}

impl OwnerMap {
    /// Parses every `DIR=UID` entry; invalid entries are reported, not parsed.
    ///
    /// # Errors
    ///
    /// Returns the first malformed entry verbatim.
    pub fn parse(specs: &[String]) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for spec in specs {
            let Some((dir, uid)) = spec.rsplit_once('=') else {
                return Err(format!("--map-owner expects DIR=UID, got '{spec}'"));
            };
            let dir = PathBuf::from(dir.trim());
            if dir.as_os_str().is_empty() {
                return Err(format!("--map-owner expects DIR=UID, got '{spec}'"));
            }
            let uid: u32 = uid
                .trim()
                .parse()
                .map_err(|_| format!("--map-owner uid must be numeric, got '{spec}'"))?;
            entries.insert(dir, uid);
        }
        Ok(Self { entries })
    }

    /// Returns the mapped owner for `dir`, matching by canonical path first
    /// and by literal path second.
    #[must_use]
    pub fn owner_of(&self, dir: &Path) -> Option<u32> {
        if let Ok(canonical) = fs::canonicalize(dir) {
            if let Some(uid) = self.entries.get(&canonical) {
                return Some(*uid);
            }
        }
        self.entries.get(dir).copied()
    }
}

/// Inputs of one discovery pass.
pub struct DiscoveryOptions {
    /// Explicit `--source` directories.
    pub explicit: Vec<PathBuf>,
    /// Admin owner mappings.
    pub owner_map: OwnerMap,
    /// Scan `<root>/*/.agent-sec-core` when `Some`.
    pub homes_root: Option<PathBuf>,
    /// Scan `<root>/agent-sec-*` when `Some`.
    pub tmp_root: Option<PathBuf>,
    /// The destination directory; sources inside it are already system-owned.
    pub destination_dir: PathBuf,
}

/// The result of a discovery pass.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Candidates that passed the structural checks.
    pub sources: Vec<DiscoveredSource>,
    /// Directories rejected during discovery, with reasons.
    pub rejected: Vec<RejectedSource>,
    /// Directories skipped because they are the destination itself.
    pub system_owned: Vec<PathBuf>,
}

/// Runs discovery over every configured origin.
///
/// Structural checks only: the directory must exist, not be a symlink, be a
/// directory and be readable. Stream-file validation happens in
/// [`crate::source`]. Sources discovered by scanning are rejected softly
/// (recorded, not fatal); explicit sources surface their rejection to the
/// caller through the same list, and the orchestrator decides severity.
pub fn discover(options: &DiscoveryOptions) -> Discovery {
    let mut discovery = Discovery::default();
    let destination = normalize(&options.destination_dir);

    let mut candidates: Vec<(PathBuf, SourceOrigin)> = Vec::new();
    for dir in &options.explicit {
        candidates.push((dir.clone(), SourceOrigin::Explicit));
    }
    if let Some(root) = &options.homes_root {
        scan_home_root(root, &mut candidates);
    }
    if let Some(root) = &options.tmp_root {
        scan_tmp_root(root, &mut candidates);
    }

    for (dir, origin) in candidates {
        let normalized = normalize(&dir);
        if normalized == destination {
            discovery.system_owned.push(dir);
            continue;
        }
        match classify(&dir, &origin, &options.owner_map) {
            Ok(source) => discovery.sources.push(source),
            Err(reason) => discovery.rejected.push(RejectedSource { dir, reason }),
        }
    }
    discovery
}

fn scan_home_root(root: &Path, candidates: &mut Vec<(PathBuf, SourceOrigin)>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path().join(".agent-sec-core");
        if dir.is_dir() {
            candidates.push((dir, SourceOrigin::Homes(root.to_path_buf())));
        }
    }
}

fn scan_tmp_root(root: &Path, candidates: &mut Vec<(PathBuf, SourceOrigin)>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(suffix) = name.strip_prefix("agent-sec-") else {
            continue;
        };
        if suffix.parse::<u32>().is_err() {
            continue;
        }
        let dir = entry.path();
        if dir.is_dir() {
            candidates.push((dir, SourceOrigin::Tmp(root.to_path_buf())));
        }
    }
}

/// Structural classification of one candidate directory.
fn classify(
    dir: &Path,
    origin: &SourceOrigin,
    owner_map: &OwnerMap,
) -> Result<DiscoveredSource, String> {
    let symlink_meta = fs::symlink_metadata(dir).map_err(|err| format!("cannot stat: {err}"))?;
    if symlink_meta.file_type().is_symlink() {
        return Err("is a symlink — refusing to use".to_owned());
    }
    if !symlink_meta.is_dir() {
        return Err("is not a directory".to_owned());
    }
    let dir_uid = symlink_meta.uid();

    // The tmp tier encodes the owner in its name; a mismatch with the real
    // directory owner means the directory cannot be trusted to belong to that
    // user.
    if let SourceOrigin::Tmp(_) = origin {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if let Some(suffix) = name.strip_prefix("agent-sec-") {
            if let Ok(named_uid) = suffix.parse::<u32>() {
                if named_uid != dir_uid {
                    return Err(format!(
                        "tmp directory names uid {named_uid} but is owned by uid {dir_uid}"
                    ));
                }
            }
        }
    }

    if rustix::fs::access(dir, rustix::fs::Access::READ_OK).is_err() {
        return Err("is not readable".to_owned());
    }

    let (owner_uid, admin_mapped) = match owner_map.owner_of(dir) {
        Some(mapped) => (mapped, true),
        None => (dir_uid, false),
    };

    Ok(DiscoveredSource {
        dir: dir.to_path_buf(),
        origin: origin.clone(),
        owner_uid,
        admin_mapped,
        dir_uid,
    })
}

fn normalize(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_map_parses_and_matches_literals_and_canonical_paths() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("old-user");
        fs::create_dir_all(&dir).unwrap();
        let map =
            OwnerMap::parse(&[format!("{}=1001", dir.display()), "/abs/x=7".to_owned()]).unwrap();
        assert_eq!(map.owner_of(&dir), Some(1001));
        assert_eq!(map.owner_of(Path::new("/abs/x")), Some(7));
        assert_eq!(map.owner_of(Path::new("/abs/y")), None);
    }

    #[test]
    fn owner_map_rejects_malformed_entries() {
        assert!(OwnerMap::parse(&["no-equals-sign".to_owned()]).is_err());
        assert!(OwnerMap::parse(&["/dir=notanumber".to_owned()]).is_err());
        assert!(OwnerMap::parse(&["=1001".to_owned()]).is_err());
    }

    #[test]
    fn home_scan_requires_the_agent_dir_inside_each_user_directory() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("alice/.agent-sec-core")).unwrap();
        fs::create_dir_all(temp.path().join("bob")).unwrap();
        let mut candidates = Vec::new();
        scan_home_root(temp.path(), &mut candidates);
        assert_eq!(candidates.len(), 1);
        assert!(
            candidates[0].0.ends_with(".agent-sec-core"),
            "only directories containing the v1 fallback dir are candidates"
        );
    }

    #[test]
    fn tmp_scan_requires_a_numeric_uid_suffix() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("agent-sec-1000")).unwrap();
        fs::create_dir_all(temp.path().join("agent-sec-notanumber")).unwrap();
        fs::create_dir_all(temp.path().join("unrelated")).unwrap();
        let mut candidates = Vec::new();
        scan_tmp_root(temp.path(), &mut candidates);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].0.ends_with("agent-sec-1000"));
    }

    #[test]
    fn a_symlinked_candidate_directory_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir_all(&real).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, temp.path().join("link")).unwrap();
        let err = classify(
            &temp.path().join("link"),
            &SourceOrigin::Explicit,
            &OwnerMap::default(),
        )
        .expect_err("symlink must be rejected");
        assert!(err.contains("symlink"));
    }
}
