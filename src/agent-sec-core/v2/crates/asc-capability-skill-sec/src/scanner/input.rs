//! Read-only descriptor-relative inventory shared by the directory scanner adapters.

use crate::filesystem::{READ_FLAGS, open_directory};
use crate::{SkillSecError, check_deadline, io_error};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat};
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) const SKIP: &[&str] = &[
    ".git",
    ".skill-meta",
    ".pytest_cache",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
];
pub(crate) const MAX_FILES: usize = 2000;
pub(crate) const MAX_BYTES: u64 = 50 * 1024 * 1024;
pub(crate) const MAX_DEPTH: usize = 32;
// Bound name buffers as well as files; links and empty directories consume memory too.
const MAX_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Link { target: String },
    Special,
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    device: u64,
    inode: u64,
}

pub(crate) struct ScanTree {
    directory: File,
    pub entries: Vec<Entry>,
    pub errors: Vec<serde_json::Value>,
    pub root: PathBuf,
    visited: usize,
    file_count: usize,
    total_bytes: u64,
}

impl ScanTree {
    pub fn open(root: &Path, deadline: Instant) -> Result<Self, SkillSecError> {
        check_deadline(deadline)?;
        let tree = Self::from_directory(open_directory(root)?, root, deadline)?;
        // Signing and content comparisons must never consume a truncated inventory.
        if !tree.errors.is_empty() {
            return Err(SkillSecError::Scanner(
                "Skill exceeds directory scan limits".into(),
            ));
        }
        Ok(tree)
    }

    // Analyze needs the coverage errors, using the descriptor on which it checked SKILL.md.
    pub(super) fn from_directory(
        directory: File,
        root: &Path,
        deadline: Instant,
    ) -> Result<Self, SkillSecError> {
        let mut tree = Self {
            directory,
            entries: Vec::new(),
            errors: Vec::new(),
            root: root.into(),
            visited: 0,
            file_count: 0,
            total_bytes: 0,
        };
        let directory = tree.directory.try_clone().map_err(|e| io_error(root, e))?;
        tree.walk(&directory, "", 0, deadline)?;
        Ok(tree)
    }

    fn walk(
        &mut self,
        directory: &File,
        prefix: &str,
        depth: usize,
        deadline: Instant,
    ) -> Result<(), SkillSecError> {
        check_deadline(deadline)?;
        if depth > MAX_DEPTH {
            self.errors
                .push(serde_json::json!({"code":"directory-depth-limit",
                "message":format!("Skill directory depth exceeds {MAX_DEPTH}."),"file":prefix,
                "metadata":{"max_directory_depth":MAX_DEPTH,"truncated":true}}));
            return Ok(());
        }
        let mut names = Vec::new();
        for item in Dir::read_from(directory).map_err(|e| io_error(prefix, e))? {
            check_deadline(deadline)?;
            let item = item.map_err(|e| io_error(prefix, e))?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| SkillSecError::Invalid("non-UTF-8 Skill path".into()))?;
            if !matches!(name, "." | "..") {
                if self.visited == MAX_ENTRIES {
                    self.errors.push(serde_json::json!({"code":"entry-count-limit",
                        "message":format!("Skill traversal exceeds {MAX_ENTRIES} directory entries."),
                        "metadata":{"max_entries":MAX_ENTRIES,"entry_count":self.visited + 1,
                            "truncated":true}}));
                    return Ok(());
                }
                self.visited += 1;
                names.push(name.to_owned());
            }
        }
        names.sort();
        let mut subdirectories = Vec::new();
        for name in names {
            check_deadline(deadline)?;
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let stat = statat(directory, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| io_error(&path, e))?;
            let kind = match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => {
                    if SKIP.contains(&name.as_str()) {
                        continue;
                    }
                    subdirectories.push((name, path));
                    continue;
                }
                FileType::RegularFile => {
                    if !self.count_file(u64::try_from(stat.st_size).unwrap_or(u64::MAX)) {
                        return Ok(());
                    }
                    EntryKind::File
                }
                FileType::Symlink => {
                    // Only classify the target; never read bytes through the link or expose its path.
                    let target = match self.root.join(&path).canonicalize() {
                        Ok(target) if target.starts_with(&self.root) => "inside-root",
                        Ok(_) => "outside-root",
                        Err(_) => "unresolved",
                    };
                    EntryKind::Link {
                        target: target.into(),
                    }
                }
                _ => EntryKind::Special,
            };
            self.entries.push(Entry {
                path,
                kind,
                size: u64::try_from(stat.st_size).unwrap_or(0),
                device: stat.st_dev,
                inode: stat.st_ino,
            });
        }
        for (name, path) in subdirectories {
            let child = File::from(
                openat(
                    directory,
                    name.as_str(),
                    READ_FLAGS | OFlags::DIRECTORY,
                    Mode::empty(),
                )
                .map_err(|e| io_error(&path, e))?,
            );
            self.walk(&child, &path, depth + 1, deadline)?;
            if !self.errors.is_empty() {
                return Ok(());
            }
        }
        Ok(())
    }

    fn count_file(&mut self, size: u64) -> bool {
        self.file_count += 1;
        self.total_bytes = self.total_bytes.saturating_add(size);
        if self.file_count > MAX_FILES {
            self.errors
                .push(serde_json::json!({"code":"file-count-limit",
                "message":format!("Skill contains more than {MAX_FILES} regular files."),
                "metadata":{"max_files":MAX_FILES,"file_count":self.file_count,"truncated":true}}));
            return false;
        }
        if self.total_bytes > MAX_BYTES {
            self.errors.push(serde_json::json!({"code":"total-size-limit",
                "message":format!("Skill content exceeds {MAX_BYTES} bytes."),
                "metadata":{"max_total_bytes":MAX_BYTES,"total_bytes":self.total_bytes,"truncated":true}}));
            return false;
        }
        true
    }

    fn open_entry(&self, entry: &Entry) -> Result<File, SkillSecError> {
        let mut parent = self
            .directory
            .try_clone()
            .map_err(|e| io_error(&entry.path, e))?;
        let mut parts = entry.path.split('/').peekable();
        while let Some(part) = parts.next() {
            let flags = if parts.peek().is_some() {
                READ_FLAGS | OFlags::DIRECTORY
            } else {
                READ_FLAGS
            };
            parent = File::from(
                openat(&parent, part, flags, Mode::empty())
                    .map_err(|e| io_error(&entry.path, e))?,
            );
        }
        let before = parent.metadata().map_err(|e| io_error(&entry.path, e))?;
        if !before.is_file() || before.dev() != entry.device || before.ino() != entry.inode {
            return Err(SkillSecError::Integrity(
                "Skill entry changed before scan".into(),
            ));
        }
        Ok(parent)
    }

    pub fn read_prefix(&self, entry: &Entry, deadline: Instant) -> Result<Vec<u8>, SkillSecError> {
        check_deadline(deadline)?;
        let mut bytes = Vec::new();
        self.open_entry(entry)?
            .take(256)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&entry.path, e))?;
        Ok(bytes)
    }

    pub fn read(
        &self,
        entry: &Entry,
        limit: u64,
        deadline: Instant,
    ) -> Result<Vec<u8>, SkillSecError> {
        check_deadline(deadline)?;
        if entry.kind != EntryKind::File || entry.size > limit {
            return Err(SkillSecError::Scanner(
                "file is not readable within the scan limit".into(),
            ));
        }
        let mut parent = self.open_entry(entry)?;
        let before = parent.metadata().map_err(|e| io_error(&entry.path, e))?;
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            check_deadline(deadline)?;
            let count = parent
                .read(&mut buffer)
                .map_err(|e| io_error(&entry.path, e))?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
                return Err(SkillSecError::Scanner(
                    "file exceeded scan limit while reading".into(),
                ));
            }
        }
        let after = parent.metadata().map_err(|e| io_error(&entry.path, e))?;
        if (
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) {
            return Err(SkillSecError::Integrity(
                "Skill entry changed during scan".into(),
            ));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn file_inventory_stops_at_limit_and_strict_open_rejects_partial_content() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        for index in 0..MAX_FILES + 100 {
            fs::write(root.join(format!("{index:05}.txt")), "").unwrap();
        }
        let tree =
            ScanTree::from_directory(open_directory(&root).unwrap(), &root, deadline()).unwrap();
        assert_eq!(tree.errors[0]["code"], "file-count-limit");
        assert_eq!(tree.entries.len(), MAX_FILES);
        assert_eq!(tree.file_count, MAX_FILES + 1);
        assert!(matches!(
            ScanTree::open(&root, deadline()),
            Err(SkillSecError::Scanner(_))
        ));
    }

    #[test]
    fn entry_budget_stops_across_directories_and_counts_links_and_empty_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        fs::write(root.join("SKILL.md"), "name: fixture").unwrap();
        for branch in 0..4 {
            let directory = root.join(format!("branch-{branch}"));
            fs::create_dir(&directory).unwrap();
            for index in 0..6000 {
                let entry = directory.join(format!("{index:05}"));
                if index % 2 == 0 {
                    fs::create_dir(entry).unwrap();
                } else {
                    symlink("../SKILL.md", entry).unwrap();
                }
            }
        }
        let tree =
            ScanTree::from_directory(open_directory(&root).unwrap(), &root, deadline()).unwrap();
        assert_eq!(tree.errors.len(), 1);
        assert_eq!(tree.errors[0]["code"], "entry-count-limit");
        assert_eq!(tree.visited, MAX_ENTRIES);
        assert_eq!(tree.file_count, 1);
        // The last directory is rejected while collecting names, before classifying its entries.
        assert!(
            !tree
                .entries
                .iter()
                .any(|entry| entry.path.starts_with("branch-3/"))
        );
        assert!(matches!(
            ScanTree::open(&root, deadline()),
            Err(SkillSecError::Scanner(_))
        ));
        let result = crate::scanner::analyze(&root, deadline()).unwrap();
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.data["coverage_complete"], false);
        assert_eq!(result.data["errors"][0]["code"], "entry-count-limit");
    }
}
