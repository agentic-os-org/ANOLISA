//! Persist same-boot initialization evidence and secure its local directory.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use asc_policy_types::target::TargetRef;
use serde::{Deserialize, Serialize};

use super::LuaLsmClient;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Confirmation {
    pub(super) boot: String,
    pub(super) digest: String,
    // Preserve existing receipts for handover from a previous template revision.
    #[serde(rename = "active")]
    pub(super) initialized: bool,
    pub(super) instance: String,
}

pub(super) fn valid_instance(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl LuaLsmClient {
    fn confirmation(&self, target: &TargetRef) -> PathBuf {
        self.directory.join(format!("{}.json", target.id))
    }

    pub(super) fn read_confirmation(&self, target: &TargetRef) -> Option<Confirmation> {
        let bytes = fs::read(self.confirmation(target)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub(super) fn clear_confirmation(&self, target: &TargetRef) -> io::Result<()> {
        match fs::remove_file(self.confirmation(target)) {
            Ok(()) => File::open(&self.directory)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn save_confirmation(
        &self,
        target: &TargetRef,
        record: &Confirmation,
    ) -> io::Result<()> {
        let temporary = self
            .directory
            .join(format!("receipt-{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, self.confirmation(target))?;
        File::open(&self.directory)?.sync_all()
    }

    pub(super) fn ensure_directory(path: &Path) -> io::Result<()> {
        use std::path::Component;
        if !path.is_absolute() {
            return Err(io::Error::other("Lua-LSM state directory must be absolute"));
        }
        let uid = rustix::process::geteuid().as_raw();
        let mut parent = PathBuf::new();
        for component in path.components() {
            if !matches!(component, Component::RootDir | Component::Normal(_)) {
                return Err(io::Error::other("invalid Lua-LSM state directory"));
            }
            parent.push(component.as_os_str());
            match fs::symlink_metadata(&parent) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match fs::DirBuilder::new().mode(0o700).create(&parent) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
                Ok(_) => {}
            }
            let metadata = fs::symlink_metadata(&parent)?;
            let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
            if !metadata.is_dir()
                || metadata.uid() != 0 && metadata.uid() != uid
                || metadata.mode() & 0o022 != 0 && !sticky_root
                || parent == path && (metadata.uid() != uid || metadata.mode() & 0o077 != 0)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe Lua-LSM state directory",
                ));
            }
        }
        Ok(())
    }
}
