use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::PathBuf;
use std::sync::Arc;

use rustix::fs::{open, statat, AtFlags, Mode, OFlags};

use super::io::io_error;
use super::{ProviderSessionId, SessionError, SessionLock};

pub(crate) struct ReapprovalCheckpoint {
    lease: Arc<SessionLock>,
    directory: File,
    directory_path: PathBuf,
    filename: String,
}

impl ReapprovalCheckpoint {
    pub(super) fn new(
        directory_path: PathBuf,
        id: &ProviderSessionId,
        lease: Arc<SessionLock>,
    ) -> Result<Self, SessionError> {
        let descriptor = open(
            &directory_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| io_error("open reapproval directory", &directory_path, error.into()))?;
        let checkpoint = Self {
            lease,
            directory: File::from(descriptor),
            directory_path,
            filename: format!(".{id}.live"),
        };
        checkpoint.pending()?;
        Ok(checkpoint)
    }

    pub(crate) fn pending(&self) -> Result<bool, SessionError> {
        self.validate()?;
        let mut bytes = [0; 2];
        let count = self
            .lease
            .file
            .read_at(&mut bytes, 0)
            .map_err(|error| self.error("read reapproval checkpoint", error))?;
        match &bytes[..count] {
            [] | [0] => Ok(false),
            [1] => Ok(true),
            _ => Err(self.error(
                "read reapproval checkpoint",
                io::Error::other("invalid approval checkpoint"),
            )),
        }
    }

    pub(crate) fn set_pending(&self, pending: bool) -> Result<(), SessionError> {
        self.pending()?;
        self.lease
            .file
            .write_all_at(&[u8::from(pending)], 0)
            .map_err(|error| self.error("write reapproval checkpoint", error))?;
        self.lease
            .file
            .sync_all()
            .map_err(|error| self.error("sync reapproval checkpoint", error))?;
        self.directory
            .sync_all()
            .map_err(|error| self.error("sync reapproval directory", error))
    }

    // libc and linux_raw expose different stat field widths.
    #[allow(clippy::unnecessary_cast)]
    fn validate(&self) -> Result<(), SessionError> {
        let owned = self
            .lease
            .file
            .metadata()
            .map_err(|error| self.error("inspect reapproval checkpoint", error))?;
        let directory = self
            .directory
            .metadata()
            .map_err(|error| self.error("inspect reapproval directory", error))?;
        let named = statat(
            &self.directory,
            self.filename.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|error| self.error("inspect named reapproval checkpoint", error.into()))?;
        let uid = nix::unistd::geteuid().as_raw();
        if !owned.is_file()
            || owned.uid() != uid
            || owned.nlink() != 1
            || owned.mode() & 0o077 != 0
            || owned.len() > 1
            || directory.uid() != uid
            || directory.mode() & 0o077 != 0
            || named.st_dev as u64 != owned.dev()
            || named.st_ino as u64 != owned.ino()
            || !super::scoped::directory_matches_path(&self.directory, &self.directory_path)
        {
            return Err(self.error(
                "validate reapproval checkpoint",
                io::Error::other("approval checkpoint identity or ownership changed"),
            ));
        }
        Ok(())
    }

    fn error(&self, operation: &'static str, error: io::Error) -> SessionError {
        io_error(operation, &self.directory_path.join(&self.filename), error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionStore;

    fn checkpoint() -> (
        tempfile::TempDir,
        SessionStore,
        ProviderSessionId,
        ReapprovalCheckpoint,
    ) {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::for_test_workspace(
            root.path().join("sessions").to_str().unwrap(),
            root.path(),
        )
        .unwrap();
        let id = ProviderSessionId::new();
        let lease = Arc::new(store.acquire_live_lease(&id).unwrap().unwrap());
        let checkpoint = store.reapproval_checkpoint(&id, lease).unwrap();
        (root, store, id, checkpoint)
    }

    #[test]
    fn checkpoint_retains_lock_and_pending_state_until_reacquired() {
        let (_root, store, id, checkpoint) = checkpoint();
        assert!(!checkpoint.pending().unwrap());
        checkpoint.set_pending(true).unwrap();
        assert!(
            store.acquire_live_lease(&id).is_err(),
            "checkpoint must retain the exclusive lease"
        );
        drop(checkpoint);
        let lease = Arc::new(store.acquire_live_lease(&id).unwrap().unwrap());
        let checkpoint = store.reapproval_checkpoint(&id, lease).unwrap();
        assert!(checkpoint.pending().unwrap());
        checkpoint.set_pending(false).unwrap();
        assert!(!checkpoint.pending().unwrap());
        assert_eq!(checkpoint.lease.file.metadata().unwrap().len(), 1);
    }

    #[test]
    fn replaced_checkpoint_is_not_followed_or_cleared() {
        let (_root, _store, _id, checkpoint) = checkpoint();
        checkpoint.set_pending(true).unwrap();
        let path = checkpoint.directory_path.join(&checkpoint.filename);
        let retained = checkpoint.directory_path.join("retained-live");
        std::fs::rename(&path, &retained).unwrap();
        let unrelated = checkpoint.directory_path.join("unrelated");
        std::fs::write(&unrelated, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&unrelated, &path).unwrap();
        assert!(checkpoint.set_pending(false).is_err());
        assert_eq!(std::fs::read(&retained).unwrap(), [1]);
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unchanged");
    }

    #[test]
    fn invalid_checkpoint_is_not_silently_reset() {
        let (_root, _store, _id, checkpoint) = checkpoint();
        let path = checkpoint.directory_path.join(&checkpoint.filename);
        std::fs::write(&path, b"invalid").unwrap();
        assert!(checkpoint.pending().is_err());
        assert!(checkpoint.set_pending(false).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"invalid");
    }

    #[test]
    fn writable_checkpoint_rejects_changed_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, _store, _id, checkpoint) = checkpoint();
        checkpoint.set_pending(true).unwrap();
        checkpoint
            .lease
            .file
            .set_permissions(std::fs::Permissions::from_mode(0o666))
            .unwrap();
        assert!(checkpoint.set_pending(false).is_err());
        let mut byte = [0];
        checkpoint.lease.file.read_exact_at(&mut byte, 0).unwrap();
        assert_eq!(byte, [1]);
    }
    #[test]
    fn checkpoint_write_failure_does_not_clear_pending_state() {
        let (_root, store, id, checkpoint) = checkpoint();
        checkpoint.set_pending(true).unwrap();
        let path = checkpoint.directory_path.join(&checkpoint.filename);
        let readonly = Arc::new(SessionLock {
            file: File::open(path).unwrap(),
            created: false,
        });
        let readonly = store.reapproval_checkpoint(&id, readonly).unwrap();
        assert!(readonly.set_pending(false).is_err());
        assert!(checkpoint.pending().unwrap());
    }
}
