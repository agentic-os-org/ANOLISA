//! Hold a lifecycle lease and preserve request coordinates until owned cleanup succeeds.

use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};

use rustix::fs::{
    AtFlags, FileType, FlockOperation, Mode, OFlags, flock, fstat, openat, statat, unlinkat,
};
use serde_json::json;

use crate::{Error, Result};

pub(super) struct Lease {
    lock: File,
    requests: File,
}

impl Lease {
    #[cfg(test)]
    pub(super) fn fixture(root: &Path) -> Result<Self> {
        tests::fixture(root)
    }

    pub(super) fn acquire(root: &Path) -> Result<Self> {
        let directory = open_directory(root)?;
        let lock: File = openat(
            &directory,
            ".lifecycle.lock",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        flock(&lock, FlockOperation::NonBlockingLockShared).map_err(|_| {
            Error::Runtime("scoring runtime is being prepared or removed; retry later".into())
        })?;
        verify_lock(&lock, &directory)?;
        let requests: File = openat(
            &directory,
            "requests",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        let records = requests.metadata()?;
        let lock_meta = lock.metadata()?;
        let access = (records.mode() & 0o7777, lock_meta.mode() & 0o7777);
        if records.uid() != directory.metadata()?.uid()
            || lock_meta.uid() != records.uid()
            || !(access == (0o1700, 0o600)
                || (access == (0o1770, 0o640) && lock_meta.gid() == records.gid()))
        {
            return Err(Error::Runtime(
                "scoring request recovery directory is unsafe; repair the component".into(),
            ));
        }
        Ok(Self { lock, requests })
    }

    pub(super) fn begin(&self, id: &str, project: &str) -> Result<()> {
        if self.lock.metadata()?.nlink() != 1 {
            return Err(Error::Runtime("scoring lifecycle lease was removed".into()));
        }
        let mut record: File = openat(
            &self.requests,
            record_name(id)?,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?
        .into();
        serde_json::to_writer(
            &mut record,
            &json!({
                "schema": "agent-sec-skill-score/pending-request-v1",
                "request_id": id, "compose_project": project,
            }),
        )?;
        record.flush()?;
        record.sync_all()?;
        Ok(())
    }

    pub(super) fn complete(&self, id: &str) -> Result<()> {
        unlinkat(&self.requests, record_name(id)?, AtFlags::empty())?;
        Ok(())
    }
}

fn verify_lock(lock: &File, directory: &File) -> Result<()> {
    let metadata = fstat(lock)?;
    let current = statat(directory, ".lifecycle.lock", AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || metadata.st_nlink != 1
        || metadata.st_dev != current.st_dev
        || metadata.st_ino != current.st_ino
    {
        return Err(Error::Runtime(
            "scoring lifecycle lock changed or is unsafe".into(),
        ));
    }
    Ok(())
}

fn record_name(id: &str) -> Result<String> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Runtime(
            "invalid scoring recovery request identifier".into(),
        ));
    }
    Ok(format!("{}.json", id.to_ascii_lowercase()))
}

fn open_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        return Err(Error::Runtime(
            "scoring installation path must be absolute".into(),
        ));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory: File = openat(rustix::fs::CWD, "/", flags, Mode::empty())?.into();
    for part in path.components() {
        match part {
            Component::RootDir => (),
            Component::Normal(name) => {
                directory = openat(&directory, name, flags, Mode::empty())?.into();
            }
            _ => {
                return Err(Error::Runtime(
                    "scoring installation path escapes its prefix".into(),
                ));
            }
        }
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    pub(super) fn fixture(root: &Path) -> Result<Lease> {
        std::fs::write(root.join(".lifecycle.lock"), "")?;
        std::fs::set_permissions(
            root.join(".lifecycle.lock"),
            std::fs::Permissions::from_mode(0o640),
        )?;
        std::fs::create_dir(root.join("requests"))?;
        std::fs::set_permissions(
            root.join("requests"),
            std::fs::Permissions::from_mode(0o1770),
        )?;
        Lease::acquire(&root.canonicalize()?)
    }

    #[test]
    fn owner_only_docker_is_supported_and_world_access_requires_repair() -> Result<()> {
        let root = tempfile::tempdir()?;
        drop(fixture(root.path())?);
        let requests = root.path().join("requests");
        let lock = root.path().join(".lifecycle.lock");
        std::fs::set_permissions(&requests, std::fs::Permissions::from_mode(0o1700))?;
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600))?;
        drop(Lease::acquire(&root.path().canonicalize()?)?);
        std::fs::set_permissions(&requests, std::fs::Permissions::from_mode(0o1777))?;
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644))?;
        assert!(Lease::acquire(&root.path().canonicalize()?).is_err());
        Ok(())
    }

    #[test]
    fn shared_leases_block_removal_until_all_runtimes_exit() -> Result<()> {
        let root = tempfile::tempdir()?;
        let one = fixture(root.path())?;
        let two = Lease::acquire(&root.path().canonicalize()?)?;
        let removal = File::open(root.path().join(".lifecycle.lock"))?;
        assert!(flock(&removal, FlockOperation::NonBlockingLockExclusive).is_err());
        drop(one);
        assert!(flock(&removal, FlockOperation::NonBlockingLockExclusive).is_err());
        drop(two);
        flock(&removal, FlockOperation::NonBlockingLockExclusive)?;
        assert!(Lease::acquire(&root.path().canonicalize()?).is_err());
        Ok(())
    }

    #[test]
    fn ordinary_readonly_installation_supports_private_requests_without_fd_inheritance()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let initial = fixture(root.path())?;
        drop(initial);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555))?;
        let outcome = (|| {
            let lease = Lease::acquire(&root.path().canonicalize()?)?;
            for file in [&lease.lock, &lease.requests] {
                assert!(rustix::io::fcntl_getfd(file)?.contains(rustix::io::FdFlags::CLOEXEC));
            }
            let id = "d".repeat(32);
            lease.begin(&id, "project")?;
            lease.complete(&id)
        })();
        // Restore directory ownership permissions even if the lease check fails.
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))?;
        outcome
    }

    #[test]
    fn an_unlinked_or_replaced_lock_cannot_resume_an_old_waiter() -> Result<()> {
        let root = tempfile::tempdir()?;
        let lease = fixture(root.path())?;
        let directory = open_directory(&root.path().canonicalize()?)?;
        std::fs::remove_file(root.path().join(".lifecycle.lock"))?;
        assert!(verify_lock(&lease.lock, &directory).is_err());
        assert!(lease.begin(&"e".repeat(32), "project").is_err());
        std::fs::write(root.path().join(".lifecycle.lock"), "")?;
        assert!(verify_lock(&lease.lock, &directory).is_err());
        Ok(())
    }

    #[test]
    fn recovery_survives_owner_exit_and_only_confirmed_requests_are_removed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let lease = fixture(root.path())?;
        let pending = "a".repeat(32);
        let complete = "b".repeat(32);
        lease.begin(&pending, "asc-score-pending__env")?;
        lease.begin(&complete, "asc-score-complete__env")?;
        lease.complete(&complete)?;
        drop(lease);
        let record = root.path().join("requests").join(record_name(&pending)?);
        assert!(record.is_file());
        assert_eq!(record.metadata()?.mode() & 0o777, 0o600);
        assert!(
            !root
                .path()
                .join("requests")
                .join(record_name(&complete)?)
                .exists()
        );
        Ok(())
    }

    #[test]
    fn lease_and_request_links_cannot_redirect_reads_or_writes() -> Result<()> {
        let root = tempfile::tempdir()?;
        let lease = fixture(root.path())?;
        let external = root.path().join("external");
        std::fs::write(&external, "untouched")?;
        let id = "c".repeat(32);
        symlink(
            &external,
            root.path().join("requests").join(record_name(&id)?),
        )?;
        assert!(lease.begin(&id, "project").is_err());
        assert!(lease.begin("../../escape", "project").is_err());
        drop(lease);
        std::fs::remove_file(root.path().join(".lifecycle.lock"))?;
        symlink(&external, root.path().join(".lifecycle.lock"))?;
        assert!(Lease::acquire(&root.path().canonicalize()?).is_err());
        assert_eq!(std::fs::read_to_string(&external)?, "untouched");
        Ok(())
    }
}
