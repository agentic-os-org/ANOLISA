//! Owned-path validation and nonblocking installation locks.

use crate::{require, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path},
};

pub(crate) fn absolute(path: &Path) -> Result<()> {
    require(
        path.is_absolute() && !path.components().any(|p| matches!(p, Component::ParentDir)),
        format!("expected absolute path without '..': {}", path.display()),
    )?;
    for parent in path.ancestors() {
        require(
            !parent.is_symlink(),
            format!("symlink in installation path: {}", parent.display()),
        )?;
    }
    Ok(())
}

pub(crate) fn relative(path: &str) -> Result<()> {
    let parts: Vec<_> = path.split('/').collect();
    require(
        matches!(parts.first(), Some(&"bin" | &"libexec" | &"share"))
            && parts
                .iter()
                .all(|p| !p.is_empty() && *p != "." && *p != ".."),
        format!("invalid payload path: {path}"),
    )
}

pub(crate) fn owned_directory(path: &Path) -> Result<()> {
    absolute(path)?;
    let metadata = path.metadata()?;
    // SAFETY: geteuid reads the current effective identity without side effects.
    require(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0,
        format!(
            "directory must be owned and not group/world writable: {}",
            path.display()
        ),
    )
}

pub(crate) fn parent(path: &Path) -> Result<()> {
    absolute(path)?;
    owned_directory(
        path.parent()
            .ok_or_else(|| crate::Error::Invalid("path has no parent".into()))?,
    )
}

pub(crate) fn create(path: &Path, mode: u32) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(mode)
        .open(path)?)
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    write_private_using(path, |file| file.write_all(bytes))
}

fn write_private_using(
    path: &Path,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| crate::Error::Invalid("path has no parent".into()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    write(file.as_file_mut())?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn lock(prefix: &Path) -> Result<File> {
    lock_using(prefix, |file, path| lock_file(file, path, libc::LOCK_EX))
}

fn lock_using(prefix: &Path, acquire: impl FnOnce(File, &Path) -> Result<File>) -> Result<File> {
    let path = prefix.join(".aw-install.lock");
    match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => acquire(file, &path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Acquire and validate privately; publish only after all setup can succeed.
            let temporary = tempfile::NamedTempFile::new_in(prefix)?;
            let held = acquire(temporary.as_file().try_clone()?, temporary.path())?;
            fs::hard_link(temporary.path(), &path)?;
            Ok(held)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn shared_lock(prefix: &Path) -> Result<File> {
    absolute(prefix)?;
    let path = prefix.join(".aw-install.lock");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    lock_file(file, &path, libc::LOCK_SH)
}

fn lock_file(file: File, path: &Path, operation: i32) -> Result<File> {
    // SAFETY: the file descriptor is owned and remains open throughout flock.
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let held = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    require(
        current.is_file() && held.ino() == current.ino() && held.dev() == current.dev(),
        "installation lock changed; retry with the current prefix",
    )?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_lock_acquisition_or_validation_leaves_no_published_lock() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
        fs::create_dir_all(&target).unwrap();
        for after_acquire in [false, true] {
            let directory = tempfile::tempdir_in(&target).unwrap();
            let result = lock_using(directory.path(), |file, path| {
                if after_acquire {
                    let _held = lock_file(file, path, libc::LOCK_EX)?;
                    Err(std::io::Error::from_raw_os_error(libc::EIO).into())
                } else {
                    // LOCK_NB without a lock operation makes the real flock fail.
                    lock_file(file, path, 0)
                }
            });
            assert!(
                matches!(result, Err(crate::Error::Io(error)) if error.raw_os_error() == Some(if after_acquire { libc::EIO } else { libc::EINVAL }))
            );
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
            let held = lock(directory.path()).unwrap();
            assert_eq!(
                held.metadata().unwrap().ino(),
                directory
                    .path()
                    .join(".aw-install.lock")
                    .metadata()
                    .unwrap()
                    .ino()
            );
        }
    }

    #[test]
    fn failed_private_write_leaves_no_config_and_allows_retry() {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/preview");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target).unwrap();
        let path = directory.path().join("aw.yaml");
        let result = write_private_using(&path, |file| {
            file.write_all(b"partial YAML")?;
            Err(std::io::Error::from_raw_os_error(libc::ENOSPC))
        });
        assert!(
            matches!(result, Err(crate::Error::Io(error)) if error.raw_os_error() == Some(libc::ENOSPC))
        );
        assert!(!path.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
        write_private(&path, b"complete YAML").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete YAML");
    }
}
