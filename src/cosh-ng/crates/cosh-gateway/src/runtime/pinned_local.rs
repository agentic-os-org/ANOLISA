//! Inode-bound local executable and directory handles for Runtime launch.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(target_os = "linux")]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

/// Stable local filesystem identity retained with a pinned descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PinnedFileIdentity {
    device: u64,
    inode: u64,
}

impl PinnedFileIdentity {
    /// Returns the filesystem device number.
    #[must_use]
    pub const fn device(self) -> u64 {
        self.device
    }

    /// Returns the inode number on the filesystem device.
    #[must_use]
    pub const fn inode(self) -> u64 {
        self.inode
    }
}

/// Exact executable inode selected during trusted admission.
#[derive(Clone)]
pub struct PinnedExecutable {
    canonical_path: Arc<PathBuf>,
    descriptor: Arc<File>,
    identity: PinnedFileIdentity,
}

impl PinnedExecutable {
    /// Opens and validates one absolute executable without retaining a launch-time path lookup.
    ///
    /// # Errors
    ///
    /// Returns an error for relative, unavailable, non-regular, or non-executable targets.
    pub fn pin(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pinned executable path must be absolute",
            ));
        }
        // The single open is the admission linearization point. Following the
        // configured final symlink is intentional for installed command shims;
        // every later lookup is descriptor-relative and cannot drift to a
        // replacement at the configured or resolved path.
        let descriptor = open_pinned(path)?;
        let metadata = descriptor.metadata()?;
        if !metadata.is_file() || !metadata_executable(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pinned executable must be an executable regular file",
            ));
        }
        let canonical_path = descriptor_display_path(&descriptor)?;
        Ok(Self {
            canonical_path: Arc::new(canonical_path),
            identity: metadata_identity(&metadata)?,
            descriptor: Arc::new(descriptor),
        })
    }

    /// Opens an executable and additionally requires a native ELF image.
    ///
    /// Gateway-owned Core launch uses this constructor so the descriptor can
    /// remain close-on-exec. Script interpreters require a separately governed
    /// package launch policy and are not admitted through this boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when ordinary pinning fails or the inode is not ELF.
    pub fn pin_native(path: impl AsRef<Path>) -> io::Result<Self> {
        let pinned = Self::pin(path)?;
        let mut readable = File::open(pinned.descriptor_path())?;
        let mut magic = [0_u8; 4];
        readable.read_exact(&mut magic)?;
        if magic != *b"\x7fELF" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pinned native executable must be an ELF image",
            ));
        }
        Ok(pinned)
    }

    /// Returns the canonical path for diagnostics and policy digests only.
    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        self.canonical_path.as_path()
    }

    /// Returns the immutable filesystem identity selected at admission.
    #[must_use]
    pub const fn identity(&self) -> PinnedFileIdentity {
        self.identity
    }

    /// Returns the descriptor-backed path consumed directly by `execve`.
    pub(crate) fn descriptor_path(&self) -> PathBuf {
        proc_descriptor_path(self.descriptor.as_raw_fd())
    }

    /// Returns the descriptor inherited by a script interpreter after fork.
    pub(crate) fn descriptor_fd(&self) -> RawFd {
        self.descriptor.as_raw_fd()
    }

    #[cfg(test)]
    pub(crate) fn descriptor_weak(&self) -> std::sync::Weak<File> {
        Arc::downgrade(&self.descriptor)
    }
}

impl fmt::Debug for PinnedExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinnedExecutable")
            .field("canonical_path", &self.canonical_path)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// Exact directory inode selected as a Runtime working directory.
#[derive(Clone)]
pub struct PinnedDirectory {
    canonical_path: Arc<PathBuf>,
    descriptor: Arc<File>,
    identity: PinnedFileIdentity,
}

impl PinnedDirectory {
    /// Opens and validates one absolute directory for later descriptor-backed `chdir`.
    ///
    /// # Errors
    ///
    /// Returns an error for relative, unavailable, or non-directory targets.
    pub fn pin(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pinned directory path must be absolute",
            ));
        }
        let descriptor = open_pinned(path)?;
        let metadata = descriptor.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pinned working directory must be a directory",
            ));
        }
        let canonical_path = descriptor_display_path(&descriptor)?;
        Ok(Self {
            canonical_path: Arc::new(canonical_path),
            identity: metadata_identity(&metadata)?,
            descriptor: Arc::new(descriptor),
        })
    }

    /// Returns the canonical path for display and stable admission records.
    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        self.canonical_path.as_path()
    }

    /// Returns the immutable filesystem identity selected at admission.
    #[must_use]
    pub const fn identity(&self) -> PinnedFileIdentity {
        self.identity
    }

    /// Returns the descriptor-backed directory used by the child before exec.
    pub(crate) fn descriptor_path(&self) -> PathBuf {
        proc_descriptor_path(self.descriptor.as_raw_fd())
    }

    #[cfg(test)]
    pub(crate) fn descriptor_weak(&self) -> std::sync::Weak<File> {
        Arc::downgrade(&self.descriptor)
    }
}

impl fmt::Debug for PinnedDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinnedDirectory")
            .field("canonical_path", &self.canonical_path)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
fn open_pinned(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_PATH | nix::libc::O_CLOEXEC)
        .open(path)
}

#[cfg(not(target_os = "linux"))]
fn open_pinned(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "descriptor-pinned Runtime launch requires Linux",
    ))
}

#[cfg(target_os = "linux")]
fn metadata_identity(metadata: &fs::Metadata) -> io::Result<PinnedFileIdentity> {
    Ok(PinnedFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(target_os = "linux"))]
fn metadata_identity(_metadata: &fs::Metadata) -> io::Result<PinnedFileIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem identity pinning requires Linux",
    ))
}

#[cfg(target_os = "linux")]
fn metadata_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(target_os = "linux"))]
fn metadata_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn proc_descriptor_path(descriptor: std::os::fd::RawFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{descriptor}"))
}

#[cfg(not(target_os = "linux"))]
fn proc_descriptor_path(_descriptor: std::os::fd::RawFd) -> PathBuf {
    PathBuf::new()
}

#[cfg(target_os = "linux")]
fn descriptor_display_path(descriptor: &File) -> io::Result<PathBuf> {
    let path = fs::read_link(proc_descriptor_path(descriptor.as_raw_fd()))?;
    if !path.is_absolute() || path.to_string_lossy().ends_with(" (deleted)") {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "pinned target was removed during admission",
        ));
    }
    Ok(path)
}

#[cfg(not(target_os = "linux"))]
fn descriptor_display_path(_descriptor: &File) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "descriptor display paths require Linux",
    ))
}
