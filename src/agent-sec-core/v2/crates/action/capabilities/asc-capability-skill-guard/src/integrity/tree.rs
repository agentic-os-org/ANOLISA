//! Descriptor-relative hashing skips source symlinks and rejects unsafe snapshot entries.

use super::FileHashes;
use crate::filesystem::{READ_FLAGS, open_directory};
use crate::{GuardError, io_error};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat};
use sha2::{Digest as _, Sha256};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read as _;
use std::os::unix::{ffi::OsStringExt as _, fs::MetadataExt as _};
use std::path::Path;

/// Hashes regular files using V1 exclusions; strict snapshots reject excluded and special entries.
///
/// `root` is the already resolved physical I/O directory, not the canonical Skill identity.
///
/// # Errors
/// Reports unsafe snapshots, non-UTF-8 paths, traversal/read failures and files changed while read.
pub fn hash_tree(root: &Path, strict_snapshot: bool) -> Result<FileHashes, GuardError> {
    let directory = open_directory(root)?;
    let mut hashes = FileHashes::new();
    walk(&directory, root, "", strict_snapshot, &mut hashes)?;
    Ok(hashes)
}

fn walk(
    directory: &File,
    root: &Path,
    prefix: &str,
    strict: bool,
    hashes: &mut FileHashes,
) -> Result<(), GuardError> {
    let entries = Dir::read_from(directory).map_err(|e| io_error(root, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_error(root, e))?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let name = OsString::from_vec(name.to_vec());
        let text = name
            .to_str()
            .ok_or_else(|| GuardError::Integrity("non-UTF-8 content path".into()))?;
        if matches!(text, ".git" | ".skill-meta") {
            if strict {
                return Err(GuardError::Integrity(
                    "snapshot contains excluded metadata".into(),
                ));
            }
            continue;
        }
        let rel = if prefix.is_empty() {
            text.into()
        } else {
            format!("{prefix}/{text}")
        };
        let stat = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| io_error(root.join(&rel), e))?;
        let kind = FileType::from_raw_mode(stat.st_mode);
        if !matches!(kind, FileType::RegularFile | FileType::Directory) {
            if strict {
                return Err(GuardError::Integrity(format!(
                    "snapshot contains a link or special file: {rel}"
                )));
            }
            continue;
        }
        let flags = if kind == FileType::Directory {
            READ_FLAGS | OFlags::DIRECTORY
        } else {
            READ_FLAGS
        };
        let mut file = File::from(
            openat(directory, &name, flags, Mode::empty())
                .map_err(|e| io_error(root.join(&rel), e))?,
        );
        let before = file.metadata().map_err(|e| io_error(root.join(&rel), e))?;
        if before.ino() != stat.st_ino || before.dev() != stat.st_dev {
            return Err(GuardError::Integrity(format!(
                "content changed while opening: {rel}"
            )));
        }
        if kind == FileType::Directory {
            walk(&file, root, &rel, strict, hashes)?;
        } else {
            if !before.is_file() {
                return Err(GuardError::Integrity(
                    "content is not a regular file".into(),
                ));
            }
            let mut sha = Sha256::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let count = file
                    .read(&mut buffer)
                    .map_err(|e| io_error(root.join(&rel), e))?;
                if count == 0 {
                    break;
                }
                sha.update(&buffer[..count]);
            }
            let after = file.metadata().map_err(|e| io_error(root.join(&rel), e))?;
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
                return Err(GuardError::Integrity(format!(
                    "content changed while hashing: {rel}"
                )));
            }
            hashes.insert(rel, format!("sha256:{:x}", sha.finalize()));
        }
    }
    Ok(())
}
