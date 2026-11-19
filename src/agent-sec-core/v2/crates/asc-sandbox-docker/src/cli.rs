//! Resolve the host control executable through trusted directories and hold it for the session.

use crate::{ContainerPlan, DockerError};
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

fn check_owner(path: &Path, meta: &fs::Metadata) -> Result<(), DockerError> {
    if meta.uid() != 0 || (!meta.is_symlink() && meta.mode() & 0o022 != 0) {
        return Err(DockerError::Unsupported(format!(
            "untrusted Docker CLI path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_cli(
    path: &Path,
    mut visit: impl FnMut(&Path, &fs::Metadata) -> Result<(), DockerError>,
) -> Result<PathBuf, DockerError> {
    let mut pending = path.to_owned();
    // Check the directories containing symlinks as well as the resolved target.
    for _ in 0..=40 {
        let mut resolved = PathBuf::from("/");
        let mut parts = pending.components();
        let mut link = None;
        while let Some(part) = parts.next() {
            match part {
                Component::RootDir | Component::CurDir => {}
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::Normal(name) => resolved.push(name),
                Component::Prefix(_) => {
                    return Err(DockerError::Unsupported("invalid Docker CLI path".into()));
                }
            }
            let meta = fs::symlink_metadata(&resolved)?;
            visit(&resolved, &meta)?;
            if meta.is_symlink() {
                let target = fs::read_link(&resolved)?;
                resolved.pop();
                link = Some(resolved.join(target).join(parts.as_path()));
                break;
            }
            if !parts.as_path().as_os_str().is_empty() && !meta.is_dir() {
                return Err(DockerError::Unsupported(
                    "Docker CLI ancestor is not a directory".into(),
                ));
            }
        }
        if let Some(target) = link {
            pending = target;
        } else {
            return Ok(resolved);
        }
    }
    Err(DockerError::Unsupported(
        "too many Docker CLI symlinks".into(),
    ))
}

pub(crate) struct PinnedCli {
    file: File,
    // A later session resolves the operator's path again, including every symlink hop.
    protected: Vec<(PathBuf, (u64, u64))>,
}

impl PinnedCli {
    #[cfg(target_os = "linux")]
    pub(crate) fn trusted(path: &Path) -> Result<Self, DockerError> {
        Self::open(path, check_owner)
    }

    #[cfg(test)]
    pub(crate) fn scripted(path: &Path) -> Result<Self, DockerError> {
        Self::open(path, |_, _| Ok(()))
    }

    fn open(
        path: &Path,
        check: impl Fn(&Path, &fs::Metadata) -> Result<(), DockerError>,
    ) -> Result<Self, DockerError> {
        let mut protected = Vec::new();
        let target = resolve_cli(path, |name, meta| {
            check(name, meta)?;
            protected.push((name.to_owned(), (meta.dev(), meta.ino())));
            Ok(())
        })?;
        #[cfg(target_os = "linux")]
        let file = {
            use rustix::fs::{Mode, OFlags, open};
            File::from(
                open(
                    &target,
                    OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(std::io::Error::from)?,
            )
        };
        #[cfg(not(target_os = "linux"))]
        let file = File::open(&target)?;
        let meta = file.metadata()?;
        check(&target, &meta)?;
        if !meta.is_file() || meta.mode() & 0o111 == 0 {
            return Err(DockerError::Unsupported(
                "Docker CLI must be a regular executable file".into(),
            ));
        }
        protected.push((path.to_owned(), (meta.dev(), meta.ino())));
        Ok(Self { file, protected })
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub(crate) fn verify_mounts(&self, plan: &ContainerPlan) -> Result<(), DockerError> {
        let writable: Vec<_> = plan
            .mounts
            .iter()
            .filter(|mount| !mount.read_only)
            .collect();
        if writable.is_empty() {
            return Ok(());
        }
        let executable = self.file.metadata()?;
        for mount in &writable {
            if self
                .protected
                .iter()
                .any(|(path, _)| path.starts_with(&mount.destination))
            {
                return Err(DockerError::Unsupported(
                    "writable grant exposes the Docker CLI".into(),
                ));
            }
            self.check_identity(Path::new(&mount.source), &executable)?;
        }
        // Staging is recursively private by this point. Check nested bind mounts
        // too, since they can expose the same host objects at unrelated names.
        for line in fs::read("/proc/self/mountinfo")?
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
        {
            let field = line
                .split(|&b| b == b' ')
                .nth(4)
                .ok_or_else(|| DockerError::Protocol("invalid mountinfo record".into()))?;
            let path = mount_point(field);
            if writable
                .iter()
                .any(|m| path.starts_with(&m.source) && path != Path::new(&m.source))
            {
                self.check_identity(&path, &executable)?;
            }
        }
        Ok(())
    }

    fn check_identity(&self, path: &Path, executable: &fs::Metadata) -> Result<(), DockerError> {
        let meta = fs::metadata(path)?;
        if self
            .protected
            .iter()
            .any(|(_, id)| *id == (meta.dev(), meta.ino()))
        {
            return Err(DockerError::Unsupported(
                "writable grant exposes the Docker CLI".into(),
            ));
        }
        // Another hard link could lie beneath any directory on this filesystem.
        if meta.is_dir() && meta.dev() == executable.dev() && executable.nlink() > 1 {
            return Err(DockerError::Unsupported(
                "writable directories on the same filesystem require a singly linked Docker CLI"
                    .into(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn executable_path(pid: u32, fd: RawFd) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/fd/{fd}"))
}

fn mount_point(mut field: &[u8]) -> PathBuf {
    let mut bytes = Vec::with_capacity(field.len());
    while !field.is_empty() {
        let escape = match field.get(..4) {
            Some(b"\\040") => Some(b' '),
            Some(b"\\011") => Some(b'\t'),
            Some(b"\\012") => Some(b'\n'),
            Some(b"\\134") => Some(b'\\'),
            _ => None,
        };
        if let Some(byte) = escape {
            bytes.push(byte);
            field = &field[4..];
        } else {
            bytes.push(field[0]);
            field = &field[1..];
        }
    }
    OsString::from_vec(bytes).into()
}
