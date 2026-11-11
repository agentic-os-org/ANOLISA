use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use super::{private_state, Error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSnapshot {
    pub(crate) generation: String,
    pub(crate) enabled: bool,
}
pub(crate) struct RuntimeState {
    pub(super) root: PathBuf,
    directory: std::fs::File,
}

impl RuntimeState {
    pub(crate) fn open(base: &Path) -> Result<Self, Error> {
        Self::open_checked(base, is_local)
    }

    fn open_checked(
        base: &Path,
        is_local: impl Fn(&std::fs::File) -> Result<bool, Error>,
    ) -> Result<Self, Error> {
        let directory = private_state::open_private_directory(base)?;
        if !is_local(&directory)? {
            return Err(Error::RuntimeUnavailable);
        }
        let root = base.join("cosh-guide");
        let directory = private_state::ensure_directory_at(&directory, "cosh-guide")?;
        if !is_local(&directory)? {
            return Err(Error::RuntimeUnavailable);
        }
        Ok(Self { root, directory })
    }

    pub(crate) fn read(&self) -> Result<Option<RuntimeSnapshot>, Error> {
        let disabled = self.disabled()?;
        let Some(bytes) = private_state::read_bounded_at(&self.directory, "current", 64)? else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| Error::Corrupt)?;
        let Some((generation, enabled)) = text.trim_end_matches('\n').split_once(' ') else {
            return Err(Error::Corrupt);
        };
        if !valid_generation(generation) || !matches!(enabled, "0" | "1") {
            return Err(Error::Corrupt);
        }
        Ok(Some(RuntimeSnapshot {
            generation: generation.to_string(),
            enabled: enabled == "1" && !disabled,
        }))
    }

    pub(super) fn disabled(&self) -> Result<bool, Error> {
        Ok(private_state::read_bounded_at(&self.directory, "disabled", 0)?.is_some())
    }

    pub(super) fn initialize(&self, enabled: bool) -> Result<(), Error> {
        let _lock = private_state::LockGuard::try_acquire_at(&self.directory, "runtime.lock")?;
        if self.read()?.is_none() {
            self.publish_inner(enabled && !self.disabled()?)?;
        }
        Ok(())
    }

    pub(super) fn publish(
        &self,
        enabled: bool,
    ) -> Result<(RuntimeSnapshot, private_state::LockGuard), Error> {
        let lock = private_state::LockGuard::try_acquire_at(&self.directory, "runtime.lock")?;
        let snapshot = self.publish_inner(enabled)?;
        Ok((snapshot, lock))
    }

    fn publish_inner(&self, enabled: bool) -> Result<RuntimeSnapshot, Error> {
        // A failed later write must leave off effective for existing hooks.
        if !enabled {
            private_state::atomic_write_at(&self.directory, "disabled", b"", 0)?;
        }
        let generation = uuid::Uuid::new_v4().simple().to_string();
        let content = format!("{generation} {}\n", if enabled { 1 } else { 0 });
        private_state::atomic_write_at(&self.directory, "current", content.as_bytes(), 64)?;
        if enabled {
            private_state::remove_owner_file_at(&self.directory, "disabled")?;
        }
        Ok(RuntimeSnapshot {
            generation,
            enabled,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.root
    }
}

pub(super) fn valid_generation(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_local(directory: &std::fs::File) -> Result<bool, Error> {
    let mut stat = std::mem::MaybeUninit::<nix::libc::statfs>::uninit();
    if unsafe { nix::libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(private_state::Error::from(std::io::Error::last_os_error()).into());
    }
    let stat = unsafe { stat.assume_init() };
    #[cfg(target_os = "linux")]
    {
        Ok(matches!(
            stat.f_type as u64,
            0x01021994 | 0x858458f6 | 0xef53 | 0x58465342 | 0x9123683e | 0xf2f52010
        ))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(stat.f_flags & nix::libc::MNT_LOCAL as u32 != 0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = stat;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn rejects_nonlocal_actual_root_even_if_base_is_local() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("cosh-guide");
        private_state::ensure_directory(&root).unwrap();
        let root_inode = std::fs::metadata(&root).unwrap().ino();
        let result = RuntimeState::open_checked(&base, |directory| {
            Ok(directory.metadata().unwrap().ino() != root_inode)
        });
        assert!(matches!(result, Err(Error::RuntimeUnavailable)));
    }
    #[test]
    fn creates_actual_root_relative_to_verified_base_descriptor() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = parent.join("base");
        let original = parent.join("original-base");
        private_state::ensure_directory(&base).unwrap();
        let replaced = std::cell::Cell::new(false);
        let runtime = RuntimeState::open_checked(&base, |_| {
            if !replaced.replace(true) {
                std::fs::rename(&base, &original).unwrap();
                private_state::ensure_directory(&base).unwrap();
            }
            Ok(true)
        })
        .unwrap();
        runtime.publish(false).unwrap();
        assert!(original.join("cosh-guide/disabled").exists());
        assert!(!base.join("cosh-guide").exists());
    }
}
