//! FD-anchored Skill directory resolution shared by control-plane readers and writers.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::path::SkillLayout;

/// Failure classes from protocol-neutral Skill directory resolution.
#[derive(Debug)]
pub(crate) enum SkillDirError {
    InvalidDepth,
    RootPathContainsNul,
    RootOpen(io::Error),
    ComponentContainsNul,
    ComponentSymlink {
        component: String,
    },
    ComponentMissing {
        component: String,
    },
    ComponentNotDirectory {
        component: String,
    },
    ComponentOpen {
        component: String,
        source: io::Error,
    },
    NestedBelowTopLevelSkill {
        parent: String,
        child: String,
    },
    MarkerInspect(io::Error),
    MissingMarker,
    Identity(io::Error),
}

/// Identity read from an already-verified Skill directory fd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SkillDirIdentity {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

/// Owned fd for a directory that satisfies the configured Skill boundary.
#[derive(Debug)]
pub(crate) struct VerifiedSkillDir(OwnedFd);

impl VerifiedSkillDir {
    /// Returns the raw fd for fd-relative metadata mutations.
    pub(crate) fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    /// Reads the stable filesystem identity from the verified directory fd.
    pub(crate) fn identity(&self) -> Result<SkillDirIdentity, SkillDirError> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstat(self.as_raw_fd(), &mut stat) };
        if rc != 0 {
            return Err(SkillDirError::Identity(io::Error::last_os_error()));
        }
        Ok(SkillDirIdentity {
            dev: stat.st_dev,
            ino: stat.st_ino,
        })
    }
}

/// Open and verify a layout-relative Skill directory without following links.
///
/// The returned fd is the capability used by both read-side identity queries
/// and write-side metadata mutations. Protocol-specific validation and error
/// vocabulary remain with the caller.
pub(crate) fn open_verified_skill_dir(
    root: &Path,
    layout: SkillLayout,
    components: &[&str],
) -> Result<VerifiedSkillDir, SkillDirError> {
    let valid_depth = match layout {
        SkillLayout::Flat => components.len() == 1,
        SkillLayout::Hermes => matches!(components.len(), 1 | 2),
    };
    if !valid_depth {
        return Err(SkillDirError::InvalidDepth);
    }

    let root_name = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| SkillDirError::RootPathContainsNul)?;
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(SkillDirError::RootOpen(io::Error::last_os_error()));
    }
    // SAFETY: `open` returned a new fd and ownership is transferred once.
    let mut current = unsafe { OwnedFd::from_raw_fd(root_fd) };

    for (index, component) in components.iter().enumerate() {
        let child = open_child_dir(current.as_raw_fd(), component)?;
        if layout == SkillLayout::Hermes
            && components.len() == 2
            && index == 0
            && dir_has_regular_skill_md(child.as_raw_fd())?
        {
            return Err(SkillDirError::NestedBelowTopLevelSkill {
                parent: (*component).to_string(),
                child: components[1].to_string(),
            });
        }
        current = child;
    }

    if !dir_has_regular_skill_md(current.as_raw_fd())? {
        return Err(SkillDirError::MissingMarker);
    }
    Ok(VerifiedSkillDir(current))
}

fn open_child_dir(parent_fd: RawFd, component: &str) -> Result<OwnedFd, SkillDirError> {
    let child_name =
        CString::new(component.as_bytes()).map_err(|_| SkillDirError::ComponentContainsNul)?;
    let child_fd = unsafe {
        libc::openat(
            parent_fd,
            child_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if child_fd >= 0 {
        // SAFETY: `openat` returned a new fd and ownership is transferred once.
        return Ok(unsafe { OwnedFd::from_raw_fd(child_fd) });
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ELOOP) => Err(SkillDirError::ComponentSymlink {
            component: component.to_string(),
        }),
        Some(libc::ENOENT) => Err(SkillDirError::ComponentMissing {
            component: component.to_string(),
        }),
        Some(libc::ENOTDIR) if entry_is_symlink(parent_fd, &child_name) => {
            Err(SkillDirError::ComponentSymlink {
                component: component.to_string(),
            })
        }
        Some(libc::ENOTDIR) => Err(SkillDirError::ComponentNotDirectory {
            component: component.to_string(),
        }),
        _ => Err(SkillDirError::ComponentOpen {
            component: component.to_string(),
            source: error,
        }),
    }
}

fn entry_is_symlink(parent_fd: RawFd, name: &CStr) -> bool {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    rc == 0 && (stat.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

fn dir_has_regular_skill_md(dir_fd: RawFd) -> Result<bool, SkillDirError> {
    let marker = c"SKILL.md";
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir_fd,
            marker.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok((stat.st_mode & libc::S_IFMT) == libc::S_IFREG);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOENT) | Some(libc::ENOTDIR) => Ok(false),
        _ => Err(SkillDirError::MarkerInspect(error)),
    }
}
