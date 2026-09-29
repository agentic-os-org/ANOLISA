//! An anchored handle on the directory a session tree is built in.
//!
//! Deciding that a directory may hold sessions and then *using* it are two
//! separate pathname lookups, and a local user who can write to the parent
//! can change what the name resolves to in between. Checking the pathname
//! twice does not help: `symlink_metadata` followed by `metadata` is exactly
//! the pair a rename-then-symlink swap defeats, because each call resolves
//! the name from scratch.
//!
//! So the caller opens the directory **once** — with `O_NOFOLLOW` when it is
//! a name we picked — and validates the resulting descriptor with `fstat`.
//! This type turns that descriptor into the path every later operation uses:
//! `/proc/self/fd/<n>` is a magic symlink that the kernel resolves to the
//! *inode* the descriptor holds rather than re-walking the original
//! pathname, so renaming the directory or replacing it with a symlink
//! afterwards cannot redirect anything built through [`SessionBase::path`].
//!
//! The descriptor has to stay open for as long as those paths are used,
//! which is why `SessionBase` owns it and why `SessionLogService` keeps the
//! `SessionBase` alive next to the paths derived from it.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{AtFlags, OFlag};
use nix::sys::stat::{Mode, SFlag};

use crate::error::{MemoryError, Result};

/// A directory that was opened exactly once and is only ever used through
/// that one open file description.
#[derive(Debug)]
pub struct SessionBase {
    /// The pathname the operator configured, or the fallback we chose.
    /// For messages and logs only — never used to touch the filesystem
    /// again, which is the whole point.
    display: PathBuf,
    /// `/proc/self/fd/<n>`. Every filesystem operation goes through this.
    anchored: PathBuf,
    /// Keeps `anchored` resolving and keeps the descriptor number from being
    /// recycled to an unrelated file.
    fd: OwnedFd,
}

impl SessionBase {
    /// Open `dir` as a session base, refusing a symlink in its final
    /// component. This is the gate for directories *we* chose: an attacker
    /// who plants the name gets `ELOOP` instead of a redirected session.
    pub fn open_nofollow(dir: &Path) -> Result<Self> {
        Self::open_with(dir, OFlag::O_NOFOLLOW)
    }

    /// Open `dir` as a session base, honouring a symlink in its final
    /// component. This is the gate for the operator-configured directory,
    /// where the link is the operator's own decision — but it is still
    /// resolved exactly once, here, and everything afterwards is anchored.
    pub fn open(dir: &Path) -> Result<Self> {
        Self::open_with(dir, OFlag::empty())
    }

    /// Like [`Self::open`], creating `dir` first when it does not exist.
    ///
    /// The retry still goes through `open`, so a name that turns into
    /// something else between the `ENOENT` and the `mkdir` is caught by the
    /// same single resolution rather than by a second pathname check.
    pub fn open_creating(dir: &Path) -> Result<Self> {
        match Self::open(dir) {
            Ok(base) => Ok(base),
            Err(MemoryError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir)?;
                Self::open(dir)
            }
            Err(e) => Err(e),
        }
    }

    fn open_with(dir: &Path, extra: OFlag) -> Result<Self> {
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | extra;
        let raw = match nix::fcntl::open(dir, flags, Mode::empty()) {
            Ok(raw) => raw,
            Err(Errno::ENOTDIR | Errno::ELOOP) => {
                // The open already refused the candidate; this lookup only
                // picks the message. A symlink surfaces as `ELOOP` under
                // `O_NOFOLLOW`, and as `ENOTDIR` once `O_DIRECTORY` is in
                // the mix, because the kernel then checks the *link* for
                // being a directory.
                let is_link =
                    std::fs::symlink_metadata(dir).is_ok_and(|md| md.file_type().is_symlink());
                let why = if is_link {
                    "is a symlink; refusing it as a session dir"
                } else {
                    "is not a directory"
                };
                return Err(MemoryError::Other(format!("{} {why}", dir.display())));
            }
            Err(e) => return Err(io_error(e).into()),
        };
        // SAFETY: `open` just handed us a descriptor nobody else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Self::from_fd(dir.to_path_buf(), fd)
    }

    /// Adopt an already-open directory descriptor.
    fn from_fd(display: PathBuf, fd: OwnedFd) -> Result<Self> {
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()));

        // Everything below rests on `/proc/self/fd/<n>` resolving to this
        // descriptor's inode. Check that once, here, so a host without a
        // usable /proc fails with an explanation instead of turning into a
        // mysterious ENOENT partway through session setup.
        let direct = nix::sys::stat::fstat(fd.as_raw_fd()).map_err(io_error)?;
        let through = nix::sys::stat::stat(&anchored).map_err(|e| {
            MemoryError::Other(format!(
                "cannot anchor {} to an open descriptor ({}: {e}); /proc must be mounted",
                display.display(),
                anchored.display()
            ))
        })?;
        if (direct.st_dev, direct.st_ino) != (through.st_dev, through.st_ino) {
            return Err(MemoryError::Other(format!(
                "{} does not resolve to {}; cannot anchor the session dir",
                anchored.display(),
                display.display()
            )));
        }

        Ok(Self {
            display,
            anchored,
            fd,
        })
    }

    /// The raw descriptor, for the `*at` family.
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// The path every filesystem operation must use.
    pub fn path(&self) -> &Path {
        &self.anchored
    }

    /// The pathname the operator configured or we chose. Display only.
    pub fn display_path(&self) -> &Path {
        &self.display
    }

    /// `stat` the anchored directory — the descriptor's own inode, never a
    /// re-resolved pathname.
    pub fn stat(&self) -> Result<nix::sys::stat::FileStat> {
        nix::sys::stat::fstat(self.fd()).map_err(mem_io_error)
    }

    /// `mkdirat` a direct child of this base, i.e. create it relative to the
    /// descriptor without re-resolving the base pathname at all.
    ///
    /// An existing child is not an error — a session may be reopened — but
    /// whatever occupies the name has to be a real directory, checked
    /// without following it. A name that is a symlink would quietly send
    /// every later write to its target.
    pub fn mkdir_child(&self, name: &str, mode: u32) -> Result<()> {
        let bits = Mode::from_bits_truncate(mode);
        match nix::sys::stat::mkdirat(Some(self.fd()), name, bits) {
            Ok(()) | Err(Errno::EEXIST) => {}
            Err(e) => {
                return Err(MemoryError::Other(format!(
                    "mkdirat({}, {name}): {e}",
                    self.display.display()
                )));
            }
        }

        let st = nix::sys::stat::fstatat(Some(self.fd()), name, AtFlags::AT_SYMLINK_NOFOLLOW)
            .map_err(|e| {
                MemoryError::Other(format!("fstatat({}, {name}): {e}", self.display.display()))
            })?;
        if !SFlag::from_bits_truncate(st.st_mode).contains(SFlag::S_IFDIR) {
            return Err(MemoryError::Other(format!(
                "{name} under {} is not a directory",
                self.display.display()
            )));
        }
        Ok(())
    }
}

fn io_error(e: Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(e as i32)
}

fn mem_io_error(e: Errno) -> MemoryError {
    MemoryError::Io(io_error(e))
}
