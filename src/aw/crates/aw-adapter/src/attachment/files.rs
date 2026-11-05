//! Preserve the latest native settings and retain a displaced concurrent write.

use crate::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
    {
        return Err(format!("unsafe native file: {}", path.display()).into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("native file exceeds 1 MiB".into());
    }
    Ok(bytes)
}

pub(super) fn create(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("native settings have no parent")?;
    let mut next = tempfile::NamedTempFile::new_in(parent)?;
    next.write_all(bytes)?;
    next.as_file().sync_all()?;
    next.persist_noclobber(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub(super) fn update(
    path: &Path,
    change: impl FnOnce(&mut serde_json::Value) -> Result<()>,
) -> Result<()> {
    let original = read(path)?;
    let mut value: serde_json::Value = serde_json::from_slice(&original)?;
    let previous = value.clone();
    change(&mut value)?;
    if read(path)? != original {
        return Err("native settings changed before publication".into());
    }
    if value == previous {
        return Ok(());
    }
    let parent = path.parent().ok_or("native settings have no parent")?;
    let mut next = tempfile::NamedTempFile::new_in(parent)?;
    next.write_all(&serde_json::to_vec(&value)?)?;
    next.as_file().sync_all()?;
    if read(path)? != original {
        return Err("native settings changed before publication".into());
    }
    let source = std::ffi::CString::new(next.path().as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // Exchange retains the actual displaced inode, including racing native writes.
    // SAFETY: both terminated path buffers remain live during renameat2.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    File::open(parent)?.sync_all()?;
    if read(next.path())? != original {
        let (_, retained) = next.keep().map_err(|error| error.error)?;
        return Err(format!(
            "concurrent native settings write retained at {}; review before retrying",
            retained.display()
        )
        .into());
    }
    // Drop removes only the displaced snapshot, never the newly published file.
    Ok(())
}

pub(super) fn directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
        || path.canonicalize()? != path
    {
        return Err(format!(
            "native directory must be canonical, same-user and not group/world writable: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn settings_updates_preserve_native_data_and_reject_symlinks_and_racing_edits() {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/attachment-tests");
        fs::create_dir_all(&parent).unwrap();
        let directory = tempfile::tempdir_in(parent).unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"{\"native\":true}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        update(&path, |value| {
            value["added"] = serde_json::json!(1);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&read(&path).unwrap()).unwrap(),
            serde_json::json!({"native":true,"added":1})
        );
        assert!(update(&path, |_| {
            fs::write(&path, b"{\"concurrent\":true}")?;
            Ok(())
        })
        .is_err());
        assert_eq!(read(&path).unwrap(), b"{\"concurrent\":true}");
        let alias = directory.path().join("alias");
        symlink(&path, &alias).unwrap();
        assert!(update(&alias, |_| Ok(())).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read(&path).is_err());
    }
}
