//! Bounded content capture binds scanning, hashing and snapshots to identical regular-file bytes.

use super::storage::{Directory, set_owner};
use crate::filesystem::READ_FLAGS;
use crate::scanner::{EntryKind, ScanTree};
use crate::{FileHashes, GuardError, check_deadline, io_error};
use rustix::fs::{AtFlags, FileType, Mode, openat, statat};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::Instant;

const MAX_BYTES: usize = 50 * 1024 * 1024;
const MAX_FILES: usize = 2000;

#[derive(PartialEq, Eq)]
struct ContentFile {
    bytes: Vec<u8>,
    mode: u32,
}

pub(crate) struct Content {
    files: BTreeMap<String, ContentFile>,
    directories: BTreeSet<String>,
    size: usize,
}

impl Content {
    pub fn capture(root: &Directory, strict: bool, deadline: Instant) -> Result<Self, GuardError> {
        let mut content = Self {
            files: BTreeMap::new(),
            directories: BTreeSet::new(),
            size: 0,
        };
        content.walk(root, "", 0, strict, deadline)?;
        Ok(content)
    }

    fn walk(
        &mut self,
        root: &Directory,
        prefix: &str,
        depth: usize,
        strict: bool,
        deadline: Instant,
    ) -> Result<(), GuardError> {
        if depth > 32 {
            return Err(GuardError::Invalid(
                "content exceeds directory depth 32".into(),
            ));
        }
        for name in root.names(deadline)? {
            check_deadline(deadline)?;
            if matches!(name.as_str(), ".git" | ".skill-meta") {
                if strict {
                    return Err(GuardError::Integrity(
                        "snapshot contains excluded metadata".into(),
                    ));
                }
                continue;
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let stat = statat(&root.file, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| io_error(root.path.join(&name), e))?;
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => {
                    if self.directories.len() >= 10_000 {
                        return Err(GuardError::Invalid(
                            "content exceeds 10000 directories".into(),
                        ));
                    }
                    self.directories.insert(path.clone());
                    self.walk(
                        &root.child(&name, false)?,
                        &path,
                        depth + 1,
                        strict,
                        deadline,
                    )?;
                }
                FileType::RegularFile => self.read_file(root, &name, path, &stat, deadline)?,
                _ if strict => {
                    return Err(GuardError::Integrity(
                        "snapshot contains a link or special file".into(),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn read_file(
        &mut self,
        root: &Directory,
        name: &str,
        path: String,
        stat: &rustix::fs::Stat,
        deadline: Instant,
    ) -> Result<(), GuardError> {
        if self.files.len() >= MAX_FILES
            || u64::try_from(stat.st_size).unwrap_or(u64::MAX)
                > u64::try_from(MAX_BYTES - self.size).unwrap_or(0)
        {
            return Err(GuardError::Invalid(
                "content exceeds 2000 files or 50 MiB".into(),
            ));
        }
        let mut file = File::from(
            openat(&root.file, name, READ_FLAGS, Mode::empty())
                .map_err(|e| io_error(root.path.join(name), e))?,
        );
        let before = file.metadata().map_err(|e| io_error(&path, e))?;
        if !before.is_file() || before.dev() != stat.st_dev || before.ino() != stat.st_ino {
            return Err(GuardError::Integrity(
                "content changed during capture".into(),
            ));
        }
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            check_deadline(deadline)?;
            let count = file.read(&mut buffer).map_err(|e| io_error(&path, e))?;
            if count == 0 {
                break;
            }
            if self.size + bytes.len() + count > MAX_BYTES {
                return Err(GuardError::Invalid("content grew beyond 50 MiB".into()));
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        let after = file.metadata().map_err(|e| io_error(&path, e))?;
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
            return Err(GuardError::Integrity(
                "content changed during capture".into(),
            ));
        }
        self.size += bytes.len();
        // A root-owned snapshot must never acquire the source's setuid/setgid bits.
        self.files.insert(
            path,
            ContentFile {
                bytes,
                mode: 0o644 | (before.mode() & 0o111),
            },
        );
        Ok(())
    }

    pub fn hashes(&self) -> FileHashes {
        self.files
            .iter()
            .map(|(path, file)| (path.clone(), crate::integrity::digest(&file.bytes)))
            .collect()
    }

    pub fn write(&self, output: &Directory, deadline: Instant) -> Result<(), GuardError> {
        self.write_owned(output, deadline, None)
    }

    pub fn write_owned(
        &self,
        output: &Directory,
        deadline: Instant,
        owner: Option<u32>,
    ) -> Result<(), GuardError> {
        for path in &self.directories {
            check_deadline(deadline)?;
            let mut directory = Directory {
                file: output
                    .file
                    .try_clone()
                    .map_err(|e| io_error(&output.path, e))?,
                path: output.path.clone(),
            };
            for part in path.split('/') {
                directory = directory.child(part, true)?;
            }
        }
        for (path, file) in &self.files {
            check_deadline(deadline)?;
            let mut directory = Directory {
                file: output
                    .file
                    .try_clone()
                    .map_err(|e| io_error(&output.path, e))?,
                path: output.path.clone(),
            };
            let mut parts = path.split('/').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_some() {
                    directory = directory.child(part, true)?;
                } else {
                    let created = directory.write_new(part, &file.bytes, file.mode)?;
                    if let Some(uid) = owner {
                        set_owner(&created, uid, &directory.path.join(part))?;
                    }
                    directory.sync()?;
                }
            }
        }
        if let Some(uid) = owner {
            // Transfer directories from leaves to root, after their entries have been created.
            for path in self.directories.iter().rev() {
                check_deadline(deadline)?;
                let mut directory = Directory {
                    file: output
                        .file
                        .try_clone()
                        .map_err(|e| io_error(&output.path, e))?,
                    path: output.path.clone(),
                };
                for part in path.split('/') {
                    directory = directory.child(part, false)?;
                }
                set_owner(&directory.file, uid, &directory.path)?;
            }
            set_owner(&output.file, uid, &output.path)?;
        }
        output.sync()
    }

    pub fn scan_tree(
        &self,
        state_dir: &Path,
        original: &ScanTree,
        deadline: Instant,
    ) -> Result<(tempfile::TempDir, ScanTree), GuardError> {
        let stage = tempfile::Builder::new()
            .prefix(".scan-")
            .tempdir_in(state_dir)
            .map_err(|e| io_error(state_dir, e))?;
        self.write(&Directory::open(stage.path())?, deadline)?;
        let mut tree = ScanTree::open(stage.path(), deadline)?;
        tree.entries.extend(
            original
                .entries
                .iter()
                .filter(|e| e.kind != EntryKind::File)
                .cloned(),
        );
        Ok((stage, tree))
    }

    pub fn unchanged(
        &self,
        root: &Directory,
        original: &ScanTree,
        deadline: Instant,
    ) -> Result<(), GuardError> {
        let current = Self::capture(root, false, deadline)?;
        let new_tree = ScanTree::open(&root.path, deadline)?;
        let links = |tree: &ScanTree| -> BTreeMap<String, EntryKind> {
            tree.entries
                .iter()
                .filter(|e| e.kind != EntryKind::File)
                .map(|e| (e.path.clone(), e.kind.clone()))
                .collect()
        };
        let rebound = Directory::open(&root.path)?;
        let before = root.file.metadata().map_err(|e| io_error(&root.path, e))?;
        let after = rebound
            .file
            .metadata()
            .map_err(|e| io_error(&root.path, e))?;
        if self.files != current.files
            || self.directories != current.directories
            || links(original) != links(&new_tree)
            || (before.dev(), before.ino()) != (after.dev(), after.ino())
        {
            return Err(GuardError::Integrity(
                "Skill changed while scanning; retry with stable content".into(),
            ));
        }
        Ok(())
    }
}
