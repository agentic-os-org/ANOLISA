//! Preserve the shared shell table across atomic, conflict-checked updates.

use super::Error;
use fs2::FileExt;
use nix::unistd::{fchown, Gid, Uid};
use rustix::fs::{fgetxattr, flistxattr, fremovexattr, fsetxattr, XattrFlags};
use std::{
    ffi::CString,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

const LIMIT: u64 = 1024 * 1024;

pub(super) fn lock(path: &Path) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != nix::unistd::geteuid().as_raw() {
        return Err(Error::Conflict(
            "manager lock is not an owned regular file".into(),
        ));
    }
    file.try_lock_exclusive().map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Error::Conflict("another login-shell manager is running".into())
        } else {
            Error::Io(e)
        }
    })?;
    Ok(file)
}

pub(super) struct Table {
    path: PathBuf,
    target: PathBuf,
    original: Option<File>,
    metadata: Option<Metadata>,
    pub(super) bytes: Vec<u8>,
}

impl Table {
    pub(super) fn read(path: &Path) -> Result<Self, Error> {
        let target = match fs::canonicalize(path) {
            Ok(target) => target,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && fs::symlink_metadata(path)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                let parent = path
                    .parent()
                    .ok_or_else(|| Error::Invalid("shell table has no parent".into()))?;
                fs::canonicalize(parent)?.join(
                    path.file_name()
                        .ok_or_else(|| Error::Invalid("shell table has no name".into()))?,
                )
            }
            Err(error) => return Err(error.into()),
        };
        let original = match OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&target)
        {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let metadata = original.as_ref().map(File::metadata).transpose()?;
        if metadata
            .as_ref()
            .is_some_and(|m| !m.is_file() || m.nlink() != 1 || m.len() > LIMIT)
        {
            return Err(Error::Conflict(
                "shell table must be a bounded regular file without hard links".into(),
            ));
        }
        let mut bytes = Vec::new();
        if let Some(file) = &original {
            file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        }
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Conflict("shell table exceeds 1 MiB".into()));
        }
        Ok(Self {
            path: path.into(),
            target,
            original,
            metadata,
            bytes,
        })
    }

    pub(super) fn contains(&self, shell: &Path) -> bool {
        self.bytes
            .split(|b| *b == b'\n')
            .any(|line| active_entry(line) == shell.as_os_str().as_encoded_bytes())
    }

    pub(super) fn changed(&self, shell: &Path, register: bool) -> Vec<u8> {
        let entry = shell.as_os_str().as_encoded_bytes();
        if register {
            if self.contains(shell) {
                return self.bytes.clone();
            }
            let mut bytes = self.bytes.clone();
            if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(entry);
            bytes.push(b'\n');
            bytes
        } else {
            let mut result = Vec::new();
            for line in self.bytes.split_inclusive(|b| *b == b'\n') {
                if active_entry(line) != entry {
                    result.extend_from_slice(line);
                } else if let Some(comment) = line.iter().position(|b| *b == b'#') {
                    // Removing the selected entry must not discard its admin note.
                    result.extend_from_slice(&line[comment..]);
                }
            }
            result
        }
    }

    pub(super) fn replace(&self, bytes: &[u8]) -> Result<(), Error> {
        let parent = self
            .target
            .parent()
            .ok_or_else(|| Error::Invalid("shell table has no parent".into()))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".cosh-shells-")
            .tempfile_in(parent)?;
        temporary.write_all(bytes)?;
        if let (Some(original), Some(meta)) = (&self.original, &self.metadata) {
            fchown(
                temporary.as_raw_fd(),
                Some(Uid::from_raw(meta.uid())),
                Some(Gid::from_raw(meta.gid())),
            )
            .map_err(std::io::Error::from)?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(meta.mode()))?;
            copy_attributes(original, temporary.as_file())?;
        } else {
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o644))?;
        }
        temporary.as_file().sync_all()?;
        self.check_unchanged()?;
        temporary
            .persist(&self.target)
            .map_err(|error| Error::Io(error.error))?;
        // A failed directory sync is visible even though the rename already committed.
        File::open(parent)?.sync_all()?;
        Ok(())
    }

    fn check_unchanged(&self) -> Result<(), Error> {
        let current = Self::read(&self.path)?;
        let stamp = |meta: &Metadata| {
            (
                meta.dev(),
                meta.ino(),
                meta.len(),
                meta.mode(),
                meta.uid(),
                meta.gid(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
            )
        };
        if current.target != self.target
            || current.bytes != self.bytes
            || current.metadata.as_ref().map(stamp) != self.metadata.as_ref().map(stamp)
        {
            return Err(Error::Conflict(
                "shell table changed during preparation; retry after inspecting it".into(),
            ));
        }
        Ok(())
    }
}

fn active_entry(line: &[u8]) -> &[u8] {
    let text = line.split(|byte| *byte == b'#').next().unwrap_or_default();
    text.trim_ascii()
}

fn attribute_names(file: &File) -> Result<Vec<CString>, Error> {
    // Linux bounds an xattr name list and each value to 64 KiB. An unsupported
    // filesystem has no attributes to preserve; every other error is visible.
    let mut names = vec![0; 65_536];
    let size = match flistxattr(file, &mut names) {
        Ok(size) => size,
        Err(rustix::io::Errno::NOTSUP) => return Ok(Vec::new()),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let bytes: Vec<u8> = names[..size]
        .iter()
        .map(|byte| byte.to_ne_bytes()[0])
        .collect();
    bytes
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            CString::new(name)
                .map_err(|_| Error::Conflict("invalid extended attribute name".into()))
        })
        .collect()
}

fn copy_attributes(source: &File, destination: &File) -> Result<(), Error> {
    let names = attribute_names(source)?;
    // A temporary file may inherit a directory default ACL absent on the old
    // file. Remove such new attributes instead of silently changing access.
    for name in attribute_names(destination)? {
        if !names.contains(&name) {
            fremovexattr(destination, &name).map_err(std::io::Error::from)?;
        }
    }
    for name in names {
        let mut value = vec![0; 65_536];
        let size = fgetxattr(source, &name, &mut value).map_err(std::io::Error::from)?;
        fsetxattr(destination, &name, &value[..size], XattrFlags::empty())
            .map_err(std::io::Error::from)?;
    }
    Ok(())
}
