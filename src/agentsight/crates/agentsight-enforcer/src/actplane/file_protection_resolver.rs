//! Race-resistant path resolution against a held process root.

use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;

use agentsight_enforcement_protocol::FileProtectionTargetKind;

use super::inode_policy::userspace_dev_to_kernel;
use crate::BackendError;

const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_IN_ROOT: u64 = 0x10;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct FileIdentity {
    pub(super) ino: u64,
    pub(super) dev: u32,
    pub(super) mnt_id: u32,
    pub(super) mode: u16,
}

#[derive(Clone, Debug)]
pub(super) struct HeldPath {
    pub(super) authored_path: String,
    pub(super) identity: FileIdentity,
    pub(super) _handle: Arc<File>,
}

#[derive(Debug)]
pub(super) struct ResolutionError {
    path: String,
    operation: ResolutionOperation,
    #[cfg(test)]
    kind: io::ErrorKind,
    raw_errno: Option<i32>,
    source: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResolutionOperation {
    Open,
    Stat,
}

impl ResolutionError {
    fn new(path: &str, operation: ResolutionOperation, error: io::Error) -> Self {
        Self {
            path: path.into(),
            operation,
            #[cfg(test)]
            kind: error.kind(),
            raw_errno: error.raw_os_error(),
            source: error.to_string(),
        }
    }

    #[cfg(test)]
    pub(super) fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    #[cfg(test)]
    pub(super) fn raw_errno(&self) -> Option<i32> {
        self.raw_errno
    }

    pub(super) fn is_missing_final(&self) -> bool {
        self.operation == ResolutionOperation::Open && self.raw_errno == Some(libc::ENOENT)
    }
}

impl fmt::Display for ResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.operation == ResolutionOperation::Open && self.raw_errno == Some(libc::ENOSYS) {
            return formatter.write_str(
                "kernel lacks openat2; no equally race-safe compatibility resolver is available",
            );
        }
        match self.operation {
            ResolutionOperation::Open => write!(
                formatter,
                "cannot resolve {:?} in target mount namespace: {}",
                self.path, self.source
            ),
            ResolutionOperation::Stat => write!(
                formatter,
                "cannot stat held path {:?}: {}",
                self.path, self.source
            ),
        }
    }
}

impl std::error::Error for ResolutionError {}

pub(super) fn open_held(root: &File, authored_path: &str) -> Result<HeldPath, ResolutionError> {
    open_held_with_flags(root, authored_path, libc::O_NOFOLLOW)
}

pub(super) fn open_held_directory(
    root: &File,
    authored_path: &str,
) -> Result<HeldPath, ResolutionError> {
    open_held_with_flags(root, authored_path, libc::O_DIRECTORY)
}

fn open_held_with_flags(
    root: &File,
    authored_path: &str,
    final_flags: i32,
) -> Result<HeldPath, ResolutionError> {
    let relative = authored_path.strip_prefix('/').unwrap_or(authored_path);
    let relative = if relative.is_empty() { "." } else { relative };
    let path = CString::new(relative).map_err(|_| {
        ResolutionError::new(
            authored_path,
            ResolutionOperation::Open,
            io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"),
        )
    })?;
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC | final_flags) as u64,
        mode: 0,
        resolve: RESOLVE_IN_ROOT | RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: root, CString path, and repr(C) OpenHow remain live for the syscall.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        ) as i32
    };
    if fd < 0 {
        return Err(ResolutionError::new(
            authored_path,
            ResolutionOperation::Open,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: the successful syscall returned a unique FD whose ownership transfers to File.
    let file = unsafe { File::from_raw_fd(fd) };
    let identity = statx_identity(&file)
        .map_err(|error| ResolutionError::new(authored_path, ResolutionOperation::Stat, error))?;
    Ok(HeldPath {
        authored_path: authored_path.into(),
        identity,
        _handle: Arc::new(file),
    })
}

pub(super) fn require_type(
    path: &HeldPath,
    kind: FileProtectionTargetKind,
) -> Result<(), BackendError> {
    match kind {
        FileProtectionTargetKind::ExactFile
            if path.identity.mode & libc::S_IFMT as u16 == libc::S_IFREG as u16 =>
        {
            Ok(())
        }
        FileProtectionTargetKind::DirectoryTree
            if path.identity.mode & libc::S_IFMT as u16 == libc::S_IFDIR as u16 =>
        {
            Ok(())
        }
        FileProtectionTargetKind::ExactFile => Err(invalid(format!(
            "exact-file target {:?} must be an existing regular file and cannot be a symlink",
            path.authored_path
        ))),
        FileProtectionTargetKind::DirectoryTree => Err(invalid(format!(
            "directory-tree target {:?} must be an existing directory and cannot be a symlink",
            path.authored_path
        ))),
    }
}

pub(super) fn require_directory(path: &HeldPath) -> Result<(), BackendError> {
    if path.identity.mode & libc::S_IFMT as u16 == libc::S_IFDIR as u16 {
        Ok(())
    } else {
        Err(invalid(format!(
            "path {:?} must resolve to a directory",
            path.authored_path
        )))
    }
}

fn statx_identity(file: &File) -> io::Result<FileIdentity> {
    // SAFETY: statx contains only integer fields, so zeroing yields a valid output buffer.
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: File provides a valid FD and stat is a live writable output buffer.
    let rc = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
            &mut stat,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if stat.stx_mask & (libc::STATX_INO | libc::STATX_TYPE | libc::STATX_MNT_ID)
        != (libc::STATX_INO | libc::STATX_TYPE | libc::STATX_MNT_ID)
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "statx omitted inode, type, or mount ID",
        ));
    }
    let mnt_id = u32::try_from(stat.stx_mnt_id)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "mount ID exceeds u32"))?;
    let encoded = libc::makedev(stat.stx_dev_major, stat.stx_dev_minor);
    Ok(FileIdentity {
        ino: stat.stx_ino,
        dev: userspace_dev_to_kernel(encoded),
        mnt_id,
        // Permissions can change without changing the file's identity.
        mode: stat.stx_mode & libc::S_IFMT as u16,
    })
}

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::CompileFailure(message.into())
}

#[cfg(test)]
#[path = "file_protection_resolver_tests.rs"]
mod tests;
