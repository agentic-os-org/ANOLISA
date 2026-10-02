use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::ns::Namespace;

use super::MountStrategy;

/// Default strategy: place each namespace under `<base>/<ns.dir_name()>/`,
/// the same on-disk layout used in P0+P1. No syscall side effects.
pub struct UserlandMount;

impl MountStrategy for UserlandMount {
    fn ensure(&self, ns: &Namespace, base: &Path) -> Result<PathBuf> {
        let root = base.join(ns.dir_name());
        std::fs::create_dir_all(&root)?;
        super::populate_mount_dir(&root, ns)?;
        Ok(root)
    }

    fn name(&self) -> &'static str {
        "userland"
    }
}
