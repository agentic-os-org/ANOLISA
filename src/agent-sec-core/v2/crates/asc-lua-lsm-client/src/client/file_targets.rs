//! Pin and label the filesystem objects selected by a Lua-LSM deployment.

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use asc_file_policy::FileProtectionTargetKind;
use asc_lua_lsm_protocol::LuaLsmPlan;
use asc_policy_types::target::Failure;

use super::{LuaLsmClient, MAX_OBJECTS, encode, local_error, rejected, retryable};

impl LuaLsmClient {
    pub(super) fn label_file_targets(
        &self,
        plan: &LuaLsmPlan,
        controls: &Path,
    ) -> Result<Vec<File>, Failure> {
        let mut count = 0;
        let mut pins = Vec::new();
        for (index, target) in plan.policy.targets.iter().enumerate() {
            let path = fs::canonicalize(&target.path).map_err(local_error)?;
            let pinned = OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::PATH.bits().cast_signed())
                .open(&path)
                .map_err(local_error)?;
            let metadata = pinned.metadata().map_err(local_error)?;
            if match target.kind {
                FileProtectionTargetKind::ExactFile => !metadata.is_file(),
                FileProtectionTargetKind::DirectoryTree => !metadata.is_dir(),
            } {
                return Err(rejected("LUA_LSM_TARGET_TYPE_MISMATCH"));
            }
            self.descriptor_command(controls, "anchor", index + 1, b"", &pinned)?;
            self.tag_tree(
                controls,
                index + 1,
                &path,
                &path,
                target.kind == FileProtectionTargetKind::DirectoryTree,
                &mut count,
            )?;
            if let Some(parent) = path.parent()
                && let Some(name) = path.file_name()
            {
                self.object_command(controls, "slot", index + 1, name.as_bytes(), parent)?;
            }
            if target.protections.delete
                || target.protections.read
                || target.protections.ordinary_write
            {
                let mut ancestor = Some(path.as_path());
                while let Some(path) = ancestor {
                    self.object_command(controls, "stable", index + 1, b"", path)?;
                    ancestor = path.parent();
                }
            }
            let current = fs::metadata(&target.path).map_err(local_error)?;
            if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
                return Err(retryable("LUA_LSM_TARGET_CHANGED"));
            }
            pins.push(pinned);
        }
        Ok(pins)
    }

    fn object_command(
        &self,
        controls: &Path,
        kind: &str,
        index: usize,
        relative: &[u8],
        path: &Path,
    ) -> Result<(), Failure> {
        // Resolve long relative paths inside the callback; command filenames
        // stay bounded and can be reused for each pinned descriptor number.
        let (kind, relative) = if kind == "tag" {
            (
                if relative.is_empty() { "root" } else { "tag" },
                b"".as_slice(),
            )
        } else {
            (kind, relative)
        };
        let flags = rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW;
        let pinned = OpenOptions::new()
            .read(true)
            .custom_flags(flags.bits().cast_signed())
            .open(path)
            .map_err(local_error)?;
        self.descriptor_command(controls, kind, index, relative, &pinned)
    }

    fn descriptor_command(
        &self,
        controls: &Path,
        kind: &str,
        index: usize,
        relative: &[u8],
        pinned: &File,
    ) -> Result<(), Failure> {
        let reference = format!("/proc/self/fd/{}", pinned.as_raw_fd());
        let command = format!(
            "{kind}/{index}/{}/at/{}",
            encode(relative),
            encode(reference.as_bytes())
        );
        self.command(controls, &command)
    }

    fn tag_tree(
        &self,
        controls: &Path,
        index: usize,
        root: &Path,
        path: &Path,
        tree: bool,
        count: &mut usize,
    ) -> Result<(), Failure> {
        *count += 1;
        if *count > MAX_OBJECTS {
            return Err(rejected("LUA_LSM_OBJECT_LIMIT_EXCEEDED"));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| rejected("LUA_LSM_INVALID_PATH"))?;
        self.object_command(
            controls,
            "tag",
            index,
            relative.as_os_str().as_bytes(),
            path,
        )?;
        if tree && fs::symlink_metadata(path).map_err(local_error)?.is_dir() {
            for child in fs::read_dir(path).map_err(local_error)? {
                self.tag_tree(
                    controls,
                    index,
                    root,
                    &child.map_err(local_error)?.path(),
                    true,
                    count,
                )?;
            }
        }
        Ok(())
    }
}
