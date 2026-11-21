//! Private Stop reply bodies referenced by the durable audit receipt.

use super::Audit;
use crate::{rejected, Result};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
};

impl Audit {
    // A Provider can include arbitrary text in its Stop reason. Keep the
    // reply bytes private and link them from the metadata-only audit record.
    pub(crate) fn store_stop_reason(&self, key: &str, step: &str, stdout: &[u8]) -> Result<String> {
        if stdout.len() > aw_provider::MAX_MESSAGE_BYTES {
            return Err(rejected("stop_reply_limit"));
        }
        let directory = self.directory.join("stop-replies");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => fs::File::open(&self.directory)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(self.stop_reason_path(key, step)?)?;
        file.write_all(stdout)?;
        file.sync_all()?;
        fs::File::open(directory)?.sync_all()?;
        Ok(aw_contracts::canonical::digest(stdout))
    }

    pub(crate) fn read_stop_reason(
        &self,
        key: &str,
        step: &str,
        expected: &str,
    ) -> Result<Vec<u8>> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.stop_reason_path(key, step)?)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.mode() & 0o7777 != 0o600 {
            return Err(rejected("invalid_stop_receipt"));
        }
        let mut stdout = Vec::new();
        file.take(aw_provider::MAX_MESSAGE_BYTES as u64 + 1)
            .read_to_end(&mut stdout)?;
        if stdout.len() > aw_provider::MAX_MESSAGE_BYTES
            || aw_contracts::canonical::digest(&stdout) != expected
        {
            return Err(rejected("invalid_stop_receipt"));
        }
        Ok(stdout)
    }

    fn stop_reason_path(&self, key: &str, step: &str) -> Result<PathBuf> {
        let directory = self.directory.join("stop-replies");
        let metadata = fs::symlink_metadata(&directory)?;
        // SAFETY: geteuid reads the process identity without arguments.
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(rejected("invalid_stop_receipt"));
        }
        Ok(directory.join(format!(
            "{key}-{}.stop",
            aw_contracts::canonical::digest(step.as_bytes())
        )))
    }
}
