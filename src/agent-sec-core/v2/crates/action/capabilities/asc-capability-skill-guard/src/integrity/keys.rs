//! Atomic system-key initialization in an explicitly provisioned private state directory.

use super::{SigningIdentity, digest};
use crate::filesystem::{READ_FLAGS, open_directory};
use crate::{GuardError, io_error};
use ring::{
    rand::{SecureRandom as _, SystemRandom},
    signature::Ed25519KeyPair,
};
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags, openat, renameat_with, unlinkat};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

const KEY_FILE: &str = "signing-key.pk8";
const MAX_KEY_BYTES: u64 = 4096;

/// Pinned service-owned key directory; only initialization is exposed to business setup.
pub struct KeyStore {
    directory: File,
    path: PathBuf,
}

impl KeyStore {
    /// Opens an existing private directory without following symlink components.
    ///
    /// # Errors
    /// Rejects a directory not owned by the effective service UID or accessible to other users.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GuardError> {
        let path = path.as_ref();
        let directory = open_directory(path)?;
        let meta = directory.metadata().map_err(|e| io_error(path, e))?;
        if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o077 != 0 {
            return Err(GuardError::Invalid(
                "signing directory must be service-owned and private".into(),
            ));
        }
        Ok(Self {
            directory,
            path: path.to_path_buf(),
        })
    }

    /// Loads only the current key, without V1 imports or historical keyring fallback.
    ///
    /// # Errors
    /// Rejects missing, malformed, hard-linked, symlinked, oversized or permissively owned keys.
    pub fn load(&self) -> Result<SigningIdentity, GuardError> {
        let path = self.path.join(KEY_FILE);
        let file = File::from(
            openat(&self.directory, KEY_FILE, READ_FLAGS, Mode::empty())
                .map_err(|e| io_error(&path, e))?,
        );
        let meta = file.metadata().map_err(|e| io_error(&path, e))?;
        if !meta.is_file()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
            || meta.len() > MAX_KEY_BYTES
        {
            return Err(GuardError::Invalid(
                "signing key must be a private service-owned regular file".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_KEY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&path, e))?;
        let result = Ed25519KeyPair::from_pkcs8(&bytes)
            .map(SigningIdentity)
            .map_err(|_| GuardError::Key);
        // PKCS8 is transient; the key pair owns the only retained signing representation.
        bytes.fill(0);
        result
    }

    /// Creates the first key atomically, or loads the existing identity unchanged.
    ///
    /// # Errors
    /// Propagates unsafe existing keys, entropy failures and persistence errors. Never repairs
    /// or replaces an invalid key automatically, since doing so would reset the trust domain.
    pub fn initialize(&self) -> Result<SigningIdentity, GuardError> {
        match self.load() {
            Ok(key) => return Ok(key),
            Err(GuardError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            }
            Err(error) => return Err(error),
        }
        let rng = SystemRandom::new();
        let key = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| GuardError::Key)?;
        let mut nonce = [0_u8; 32];
        rng.fill(&mut nonce).map_err(|_| GuardError::Key)?;
        let temp = format!(".key-{}.tmp", digest(&nonce).trim_start_matches("sha256:"));
        let mut file = File::from(
            openat(
                &self.directory,
                temp.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|e| io_error(&self.path, e))?,
        );
        let outcome = (|| {
            file.write_all(key.as_ref())
                .map_err(|e| io_error(&self.path, e))?;
            file.sync_all().map_err(|e| io_error(&self.path, e))?;
            match renameat_with(
                &self.directory,
                temp.as_str(),
                &self.directory,
                KEY_FILE,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(io_error(&self.path, error)),
            }
            self.directory
                .sync_all()
                .map_err(|e| io_error(&self.path, e))?;
            self.load()
        })();
        // No recursive cleanup and no user-supplied path: this removes only our exclusive temporary file.
        let _ = unlinkat(&self.directory, temp.as_str(), AtFlags::empty());
        outcome
    }
}
