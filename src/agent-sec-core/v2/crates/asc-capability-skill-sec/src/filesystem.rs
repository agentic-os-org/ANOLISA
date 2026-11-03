//! Descriptor-relative filesystem primitives keep privileged traversal inside opened directories.

use crate::{SkillSecError, io_error};
use rustix::fs::{Mode, OFlags, open, openat};
use std::fs::File;
use std::path::{Component, Path};

pub(crate) const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);

pub(crate) fn open_directory(path: &Path) -> Result<File, SkillSecError> {
    if !path.is_absolute() {
        return Err(SkillSecError::Invalid("directory must be absolute".into()));
    }
    let mut current = File::from(
        open("/", READ_FLAGS | OFlags::DIRECTORY, Mode::empty()).map_err(|e| io_error(path, e))?,
    );
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = File::from(
                    openat(
                        &current,
                        name,
                        READ_FLAGS | OFlags::DIRECTORY,
                        Mode::empty(),
                    )
                    .map_err(|e| io_error(path, e))?,
                );
            }
            _ => {
                return Err(SkillSecError::Invalid(
                    "directory contains traversal".into(),
                ));
            }
        }
    }
    Ok(current)
}
