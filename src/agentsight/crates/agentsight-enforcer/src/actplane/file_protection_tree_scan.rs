//! Apply-time walk for directory-tree read protection.
//!
//! Tree reads are classified by the accessed name, so a protected inode that
//! also has a name outside the tree stays readable through that name. Apply
//! rejects trees that already hold multiply-linked regular files; the BPF link
//! guard keeps bound processes from creating new outside names afterwards.
//! The same walk records every in-scope file identity, so existing mappings
//! made through bind aliases or symlinked spellings are found by inode
//! rather than only by their lexical `/proc/<pid>/maps` path.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use agentsight_enforcement_protocol::FileProtectionExclusionKind;

use super::file_protection_policy::NormalizedFileProtectionTarget;
use super::file_protection_resolver::HeldPath;
use super::inode_policy::userspace_dev_to_kernel;
use crate::BackendError;

/// Upper bound on visited entries so apply latency stays bounded under the lifecycle lock.
pub(super) const MAX_SCANNED_ENTRIES: usize = 100_000;

/// Walks a read-protected tree and returns the kernel `(dev, ino)` identity of
/// every regular file in its scope.
///
/// The walk starts from the held root, so it observes the target process mount
/// namespace and descends into submounts below the root. Symlinks are never
/// followed and subtree exclusions are skipped; an exact exclusion only exempts
/// its own entry.
///
/// # Errors
///
/// Returns [`BackendError::CompileFailure`] naming the first multiply-linked
/// file, an unreadable directory, or a tree larger than [`MAX_SCANNED_ENTRIES`].
pub(super) fn scan_read_tree(
    target: &NormalizedFileProtectionTarget,
    root: &HeldPath,
) -> Result<HashSet<(u32, u64)>, BackendError> {
    let base = PathBuf::from(format!("/proc/self/fd/{}", root.handle().as_raw_fd()));
    scan(target, &base, MAX_SCANNED_ENTRIES)
}

fn scan(
    target: &NormalizedFileProtectionTarget,
    base: &Path,
    limit: usize,
) -> Result<HashSet<(u32, u64)>, BackendError> {
    let mut files = HashSet::new();
    let mut pending = vec![(base.to_path_buf(), PathBuf::from(&target.path))];
    let mut visited = 0usize;
    while let Some((directory, authored)) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            // A directory removed during the walk holds nothing left to alias.
            Err(error) if error.kind() == io::ErrorKind::NotFound && directory != base => continue,
            Err(error) => return Err(unverifiable(&target.path, &authored, &error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| unverifiable(&target.path, &authored, &error))?;
            visited += 1;
            if visited > limit {
                return Err(BackendError::CompileFailure(format!(
                    "directory-tree read protection for {:?} visits more than {limit} entries while checking hard links; narrow the target or exclude large subtrees",
                    target.path
                )));
            }
            let name = entry.file_name();
            let entry_authored = authored.join(&name);
            let (exact_excluded, subtree_excluded) = excluded(target, &entry_authored);
            if subtree_excluded {
                continue;
            }
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(unverifiable(&target.path, &entry_authored, &error)),
            };
            if metadata.is_dir() {
                pending.push((entry.path(), entry_authored));
            } else if metadata.is_file() && !exact_excluded {
                if metadata.nlink() > 1 {
                    return Err(BackendError::CompileFailure(format!(
                        "directory-tree read protection for {:?} cannot cover {:?}: the regular file has {} hard links, and a link outside the protected scope could bypass the protection; remove the extra links or exclude the entry",
                        target.path,
                        entry_authored,
                        metadata.nlink()
                    )));
                }
                files.insert((userspace_dev_to_kernel(metadata.dev()), metadata.ino()));
            }
        }
    }
    Ok(files)
}

fn excluded(target: &NormalizedFileProtectionTarget, path: &Path) -> (bool, bool) {
    let mut exact = false;
    for exclusion in &target.exclusions {
        let excluded = Path::new(&exclusion.path);
        match exclusion.kind {
            FileProtectionExclusionKind::DirectorySubtree if path.starts_with(excluded) => {
                return (true, true);
            }
            FileProtectionExclusionKind::ExactEntry if path == excluded => exact = true,
            _ => {}
        }
    }
    (exact, false)
}

fn unverifiable(root: &str, path: &Path, error: &io::Error) -> BackendError {
    BackendError::CompileFailure(format!(
        "directory-tree read protection for {root:?} cannot verify hard links under {path:?}: {error}"
    ))
}

#[cfg(test)]
#[path = "file_protection_tree_scan_tests.rs"]
mod tests;
