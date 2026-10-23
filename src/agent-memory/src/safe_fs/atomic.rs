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

/// Write a complete replacement in `staging_dir` before atomically publishing it.
///
/// `staging_dir` must be a hidden metadata directory beneath `root`, on the same
/// filesystem as the destination. Existing files must be writable regular files.
/// Mode (without set-ID bits), POSIX access ACLs and `user.*` attributes are
/// required; copying failures abort before publication. Owner/group preservation
/// is best effort: permission or namespace restrictions retain the staging
/// identity with a warning. Host-managed attributes (`security.*`, `trusted.*`
/// and other system attributes) are not copied or removed; the new inode keeps
/// its host-assigned labels. This does not preserve IMA/EVM appraisal signatures
/// or custom labels. File capabilities are not copied.
///
/// Other hard links and open descriptors retain the old inode. Both directories
/// must be readable/writable, space for the complete replacement must be free,
/// and Linux procfs must expose `/proc/self/fd`. Missing files return NotFound;
/// callers should create them with the normal exclusive-create operation.
///
/// # Errors
/// Returns an error without replacing the destination if preparation, writing,
/// metadata copying or publication fails. A directory-sync error after the
/// rename explicitly reports that publication already occurred.
pub fn write_atomic(
    root: BorrowedFd<'_>,
    staging_dir: &Path,
    rel: &Path,
    content: &[u8],
) -> Result<u64> {
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
        Err(error) => return Err(error),
    };

    let staging = open_in_root(
        root,
        staging_dir,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY,
        Mode::empty(),
    )?;
    if staging.metadata()?.dev() != parent.metadata()?.dev() {
        return Err(MemoryError::InvalidArgument(format!(
            "atomic replacement of {} requires staging directory {} on the same filesystem",
            rel.display(),
            staging_dir.display()
        )));
    }
    // Held descriptors anchor creation, cleanup and publication even if either
    // pathname changes. Metadata storage is excluded from listings, Git and tar.
    let anchored_parent = Path::new("/proc/self/fd").join(parent.as_raw_fd().to_string());
    let anchored_staging = Path::new("/proc/self/fd").join(staging.as_raw_fd().to_string());
    let mut staged = tempfile::Builder::new()
        .prefix(".memory-write-")
        .suffix(".tmp")
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile_in(&anchored_staging)
        .map_err(|e| context(rel, "create replacement in metadata directory", e))?;
    staged
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    staged
        .write_all(content)
        .map_err(|e| context(rel, "write replacement", e))?;
    if let Some(existing) = existing {
        copy_metadata(&existing, staged.as_file(), rel)?;
    }
    staged
        .as_file()
        .sync_all()
        .map_err(|e| context(rel, "sync replacement", e))?;
    staged
        .persist(anchored_parent.join(name))
        .map_err(|error| context(rel, "publish replacement", error.error))?;
    staging.sync_all().map_err(|error| {
        context(
            rel,
            "replacement published, but staging directory sync failed",
            error,
        )
    })?;
    parent.sync_all().map_err(|error| {
        MemoryError::Other(format!(
            "replacement published for {}, but directory sync failed: {error}",
            rel.display()
        ))
    })?;
    Ok(content.len() as u64)
}

fn context(path: &Path, step: &str, error: impl std::fmt::Display) -> MemoryError {
    MemoryError::Other(format!(
        "atomic replacement of {}: {step}: {error}",
        path.display()
    ))
}

fn copy_metadata(source: &File, destination: &File, path: &Path) -> Result<()> {
    let meta = source
        .metadata()
        .map_err(|e| context(path, "read source metadata", e))?;
    preserve_ownership(destination, meta.uid(), meta.gid(), path)?;
    destination
        .set_permissions(std::fs::Permissions::from_mode(meta.mode() & 0o1777))
        .map_err(|e| context(path, "preserve file mode", e))?;
    copy_attributes(source, destination, path)
}

fn preserve_ownership(destination: &File, uid: u32, gid: u32, path: &Path) -> Result<()> {
    let current = destination
        .metadata()
        .map_err(|e| context(path, "read replacement metadata", e))?;
    if uid != current.uid() || gid != current.gid() {
        if let Err(error) = fchown(
            destination.as_raw_fd(),
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        ) {
            // An ordinary writable shared file need not belong to this user.
            // Atomic inode replacement cannot retain an unmapped/foreign owner
            // without privileges; keep the usable replacement and report it.
            tracing::warn!(path = %path.display(), uid, gid,
                %error, "replacement ownership could not be preserved; retaining writer ownership");
            if gid != current.gid() {
                if let Err(error) = fchown(destination.as_raw_fd(), None, Some(Gid::from_raw(gid)))
                {
                    tracing::warn!(path = %path.display(), gid, %error,
                        "replacement group could not be preserved; retaining staging group");
                }
            }
        }
    }
    Ok(())
}

fn copy_attributes(source: &File, destination: &File, path: &Path) -> Result<()> {
    // Preserve user data and access rules. Never attempt LSM relabeling or
    // carry integrity signatures over changed content. Do not remove inherited
    // host labels when they differ from the source.
    let source_names: Vec<_> = attribute_names(source, path)?
        .into_iter()
        .filter(|name| {
            if portable_attribute(name) {
                true
            } else {
                tracing::warn!(path = %path.display(), attribute = %name.to_string_lossy(),
                    "host-managed attribute not copied to replacement; retaining host defaults");
                false
            }
        })
        .collect();
    for name in attribute_names(destination, path)? {
        if !portable_attribute(&name) || source_names.contains(&name) {
            continue;
        }
        // SAFETY: the CString and live descriptor remain valid for this call.
        let result = unsafe { nix::libc::fremovexattr(destination.as_raw_fd(), name.as_ptr()) };
        if result < 0 {
            return Err(attribute_error(path, "remove inherited", &name));
        }
    }
    for name in source_names {
        // SAFETY: null/zero queries the required buffer length.
        let size = unsafe {
            nix::libc::fgetxattr(source.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0)
        };
        if size < 0 {
            return Err(attribute_error(path, "size source", &name));
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
            return Err(attribute_error(path, "read source", &name));
        }
        if size as usize > value.len() {
            return Err(context(
                path,
                &format!("read attribute {}", name.to_string_lossy()),
                "attribute grew during replacement",
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
            return Err(attribute_error(path, "copy", &name));
        }
    }
    Ok(())
}

fn portable_attribute(name: &CString) -> bool {
    name.as_bytes().starts_with(b"user.") || name.as_bytes() == b"system.posix_acl_access"
}

fn attribute_error(path: &Path, operation: &str, name: &CString) -> MemoryError {
    context(
        path,
        &format!("{operation} attribute {}", name.to_string_lossy()),
        io::Error::last_os_error(),
    )
}

fn attribute_names(file: &File, path: &Path) -> Result<Vec<CString>> {
    // SAFETY: null/zero queries the required buffer length on a live fd.
    let size = unsafe { nix::libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(nix::libc::ENOTSUP) {
            return Ok(Vec::new());
        }
        return Err(context(path, "list attribute names (size)", error));
    }
    let mut names = vec![0u8; size as usize];
    // SAFETY: names is writable for the supplied length.
    let size =
        unsafe { nix::libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
    if size < 0 {
        return Err(context(
            path,
            "list attribute names",
            io::Error::last_os_error(),
        ));
    }
    if size as usize > names.len() {
        return Err(context(
            path,
            "list attribute names",
            "attributes changed during replacement",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_ownership_change_keeps_writable_staging_file() {
        if nix::unistd::geteuid().is_root() {
            return;
        }
        let mut file = tempfile::tempfile().unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        let foreign = if uid == 65534 { 65533 } else { 65534 };
        assert_eq!(
            fchown(file.as_raw_fd(), Some(Uid::from_raw(foreign)), None),
            Err(nix::errno::Errno::EPERM)
        );
        preserve_ownership(
            &file,
            foreign,
            file.metadata().unwrap().gid(),
            Path::new("shared.md"),
        )
        .unwrap();
        file.write_all(b"replacement").unwrap();
        assert_eq!(file.metadata().unwrap().uid(), uid);
    }
}
