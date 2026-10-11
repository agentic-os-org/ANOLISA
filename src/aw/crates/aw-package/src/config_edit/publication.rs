//! Locked snapshot comparison and same-directory replacement of existing files.

use crate::{filesystem as paths, require, Result};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};

pub(super) struct Snapshot {
    pub(super) bytes: Vec<u8>,
    identity: Identity,
}

#[derive(PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified: (i64, i64),
}

impl From<&Metadata> for Identity {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        }
    }
}

pub(super) fn read(path: &Path) -> Result<Snapshot> {
    let mut file = open(path)?;
    snapshot(path, &mut file)
}

fn open(path: &Path) -> Result<File> {
    paths::parent(path)?;
    // O_NONBLOCK prevents a substituted FIFO from blocking before metadata checks.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid reads the effective identity without side effects.
    require(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o7000 == 0
            && metadata.nlink() == 1,
        "configuration must be an owned regular file without writable peers or hard links",
    )?;
    Ok(file)
}

fn snapshot(path: &Path, file: &mut File) -> Result<Snapshot> {
    let identity = Identity::from(&file.metadata()?);
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(aw_config::MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() <= aw_config::MAX_DOCUMENT_BYTES,
        "configuration exceeds the document byte limit",
    )?;
    require(
        identity == Identity::from(&file.metadata()?)
            && identity == Identity::from(&fs::symlink_metadata(path)?),
        "configuration changed while reading; reopen the current file",
    )?;
    Ok(Snapshot { bytes, identity })
}

pub(super) fn publish(
    path: &Path,
    original: &Snapshot,
    bytes: Option<&[u8]>,
) -> Result<Option<Snapshot>> {
    publish_using(path, original, bytes, |file, bytes| {
        file.write_all(bytes)?;
        file.sync_all()
    })
}

fn publish_using(
    path: &Path,
    original: &Snapshot,
    bytes: Option<&[u8]>,
    write: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
) -> Result<Option<Snapshot>> {
    let mut held = open(path)?;
    // SAFETY: held owns a live descriptor; its lock lasts through replacement.
    if unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    unchanged(path, &mut held, original)?;
    let Some(bytes) = bytes else { return Ok(None) };
    let parent = path
        .parent()
        .ok_or_else(|| crate::Error::Invalid("configuration path has no parent".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(original.identity.mode & 0o777))?;
    write(temporary.as_file_mut(), bytes)?;
    unchanged(path, &mut held, original)?;
    // Prepare the return state before the commit point: no error may follow rename.
    let identity = Identity::from(&temporary.as_file().metadata()?);
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(Some(Snapshot {
        bytes: bytes.into(),
        identity,
    }))
}

fn unchanged(path: &Path, held: &mut File, original: &Snapshot) -> Result<()> {
    let current = snapshot(path, held)?;
    require(
        current.identity == original.identity && current.bytes == original.bytes,
        "configuration changed since opening; reopen the current file",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_staging_preserves_original_and_removes_temporary_file() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("aw.yaml");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let original = read(&path).unwrap();
        let result = publish_using(&path, &original, Some(b"new document"), |file, _| {
            file.write_all(b"partial")?;
            Err(std::io::Error::from_raw_os_error(libc::ENOSPC))
        });
        assert!(
            matches!(result, Err(crate::Error::Io(error)) if error.raw_os_error() == Some(libc::ENOSPC))
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(read(&path).unwrap().identity == original.identity);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn noncooperating_replacement_during_staging_is_detected() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("aw.yaml");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let original = read(&path).unwrap();
        let result = publish_using(&path, &original, Some(b"our edits"), |file, bytes| {
            file.write_all(bytes)?;
            let replacement = directory.path().join("other.yaml");
            fs::write(&replacement, b"another writer").unwrap();
            fs::rename(replacement, &path).unwrap();
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"another writer");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
