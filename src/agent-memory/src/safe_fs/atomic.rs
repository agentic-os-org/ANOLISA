//! Atomic whole-file replacement for foreground memory edits.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use nix::fcntl::OFlag;
use nix::sys::stat::Mode;
use nix::unistd::{Gid, Uid, fchown};

use crate::error::{MemoryError, Result};

use super::open_in_root;

/// Write a complete replacement before publishing it with a same-directory rename.
///
/// Existing files must be writable regular files; their owner, group, mode and
/// extended attributes are preserved, except set-ID bits and file capabilities
/// are cleared like an ordinary unprivileged content write. Other hard links and open descriptors
/// retain the old inode. The parent directory must be writable, and Linux procfs
/// must expose `/proc/self/fd`. New files use the normal 0644/umask permissions.
///
/// # Errors
/// Returns an error without replacing the destination if preparation, writing,
/// metadata copying or publication fails. A directory-sync error after the
/// rename explicitly reports that publication already occurred.
pub fn write_atomic(root: BorrowedFd<'_>, rel: &Path, content: &[u8]) -> Result<u64> {
    let name = rel.file_name().ok_or_else(|| {
        MemoryError::InvalidArgument(format!("not a file path: {}", rel.display()))
    })?;
    let parent_path = rel
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = open_in_root(
        root,
        parent_path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY,
        Mode::empty(),
    )?;
    let leaf = Path::new(name);
    // Opening without truncation preserves the existing write-permission gate.
    // NONBLOCK lets us reject a FIFO without waiting for a reader.
    let existing = match open_in_root(
        parent.as_fd(),
        leaf,
        OFlag::O_WRONLY | OFlag::O_NONBLOCK,
        Mode::empty(),
    ) {
        Ok(file) => {
            if !file.metadata()?.is_file() {
                return Err(MemoryError::InvalidArgument(format!(
                    "not a regular file: {}",
                    rel.display()
                )));
            }
            Some(file)
        }
        Err(MemoryError::NotFound(_)) => None,
        Err(error) => return Err(error),
    };

    // The held directory descriptor anchors creation, cleanup and rename even
    // if its original pathname changes. The random name is created exclusively.
    let anchored_parent = Path::new("/proc/self/fd").join(parent.as_raw_fd().to_string());
    let mut staged = tempfile::Builder::new()
        .prefix(".memory-write-")
        .suffix(".tmp")
        .permissions(std::fs::Permissions::from_mode(0o644))
        .tempfile_in(&anchored_parent)?;
    let created_permissions = staged.as_file().metadata()?.permissions();
    staged
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    staged.write_all(content)?;
    if let Some(existing) = existing {
        copy_metadata(&existing, staged.as_file())?;
    } else {
        staged.as_file().set_permissions(created_permissions)?;
    }
    staged.as_file().sync_all()?;
    staged
        .persist(anchored_parent.join(name))
        .map_err(|error| MemoryError::Io(error.error))?;
    parent.sync_all().map_err(|error| {
        MemoryError::Other(format!(
            "replacement published for {}, but directory sync failed: {error}",
            rel.display()
        ))
    })?;
    Ok(content.len() as u64)
}

fn copy_metadata(source: &File, destination: &File) -> Result<()> {
    let meta = source.metadata()?;
    let current = destination.metadata()?;
    if meta.uid() != current.uid() || meta.gid() != current.gid() {
        fchown(
            destination.as_raw_fd(),
            Some(Uid::from_raw(meta.uid())),
            Some(Gid::from_raw(meta.gid())),
        )
        .map_err(|error| MemoryError::Other(format!("preserve replacement ownership: {error}")))?;
    }
    destination.set_permissions(std::fs::Permissions::from_mode(meta.mode() & !0o6000))?;
    // ACLs and other extended attributes belong to the existing file, not to
    // the temporary inode or the directory's default ACL. Fail before publish
    // rather than silently dropping an attribute we cannot copy.
    let source_names: Vec<_> = attribute_names(source)?
        .into_iter()
        .filter(|name| name.as_bytes() != b"security.capability")
        .collect();
    for name in attribute_names(destination)? {
        if source_names.contains(&name) {
            continue;
        }
        // SAFETY: the CString and live descriptor remain valid for this call.
        let result = unsafe { nix::libc::fremovexattr(destination.as_raw_fd(), name.as_ptr()) };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    for name in source_names {
        // SAFETY: null/zero queries the required buffer length.
        let size = unsafe {
            nix::libc::fgetxattr(source.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0)
        };
        if size < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut value = vec![0u8; size as usize];
        // SAFETY: the buffer is writable for the supplied length.
        let size = unsafe {
            nix::libc::fgetxattr(
                source.as_raw_fd(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if size < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if size as usize > value.len() {
            return Err(MemoryError::Other(
                "file attribute grew during replacement".into(),
            ));
        }
        // SAFETY: the value is readable for the checked returned length.
        let result = unsafe {
            nix::libc::fsetxattr(
                destination.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                size as usize,
                0,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn attribute_names(file: &File) -> Result<Vec<CString>> {
    // SAFETY: null/zero queries the required buffer length on a live fd.
    let size = unsafe { nix::libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(nix::libc::ENOTSUP) {
            return Ok(Vec::new());
        }
        return Err(error.into());
    }
    let mut names = vec![0u8; size as usize];
    // SAFETY: names is writable for the supplied length.
    let size =
        unsafe { nix::libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
    if size < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if size as usize > names.len() {
        return Err(MemoryError::Other(
            "file attributes changed during replacement".into(),
        ));
    }
    names[..size as usize]
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            CString::new(name)
                .map_err(|error| MemoryError::Other(format!("invalid attribute name: {error}")))
        })
        .collect()
}
