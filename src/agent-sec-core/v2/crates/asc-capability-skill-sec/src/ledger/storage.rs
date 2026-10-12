//! Pinned directories and atomic files; caller-controlled entries are never followed or truncated.

use crate::filesystem::{READ_FLAGS, open_directory};
use crate::{SkillSecError, check_deadline, io_error};
use ring::rand::{SecureRandom as _, SystemRandom};
use rustix::fs::{
    AtFlags, Dir, Mode, OFlags, RenameFlags, mkdirat, openat, renameat_with, unlinkat,
};
use rustix::fs::{FileType, statat};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;

/// Kernel identity (device, inode) of one directory entry.
///
/// Captured through the handle the creator opened, it attributes an artifact
/// to the invocation that created it: an error cleanup may only remove an
/// entry while it still carries the identity recorded at creation, so a name
/// reused by a competing writer is never deleted by someone else's failure.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EntryIdentity {
    device: rustix::fs::Dev,
    inode: u64,
}

impl EntryIdentity {
    /// Captures the identity of the entry behind an already-open handle.
    pub(crate) fn of(file: &File, path: &Path) -> Result<Self, SkillSecError> {
        let stat = rustix::fs::fstat(file).map_err(|e| io_error(path, e))?;
        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    }
}

pub(crate) struct Directory {
    pub file: File,
    pub path: PathBuf,
}

impl Directory {
    pub fn open(path: &Path) -> Result<Self, SkillSecError> {
        Ok(Self {
            file: open_directory(path)?,
            path: path.into(),
        })
    }

    pub fn child(&self, name: &str, create: bool) -> Result<Self, SkillSecError> {
        validate_name(name)?;
        let path = self.path.join(name);
        let mut created = false;
        if create {
            match mkdirat(&self.file, name, Mode::from_raw_mode(0o755)) {
                Ok(()) => {
                    self.sync()?;
                    created = true;
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(io_error(&path, e)),
            }
        }
        let file = File::from(
            openat(
                &self.file,
                name,
                READ_FLAGS | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map_err(|e| io_error(&path, e))?,
        );
        if created {
            // The process umask protects secrets; published Skill metadata still needs its
            // explicit reader mode for a separately running SkillFS consumer.
            rustix::fs::fchmod(&file, Mode::from_raw_mode(0o755))
                .map_err(|e| io_error(&path, e))?;
        }
        Ok(Self { file, path })
    }

    pub fn fresh_child(&self, name: &str) -> Result<Self, SkillSecError> {
        validate_name(name)?;
        mkdirat(&self.file, name, Mode::from_raw_mode(0o755))
            .map_err(|e| io_error(self.path.join(name), e))?;
        let child = (|| {
            self.sync()?;
            let child = self.child(name, false)?;
            rustix::fs::fchmod(&child.file, Mode::from_raw_mode(0o755))
                .map_err(|e| io_error(&child.path, e))?;
            Ok(child)
        })();
        if child.is_err() {
            // The directory was created by this call and nothing was published
            // into it, so it must not outlive the failed call: a leaked empty
            // child turns the next fresh_child for the same name into EEXIST
            // (or blocks an exported name's empty-directory guard). Nothing
            // else can have written into the still-daemon-owned directory, so
            // the removal is a plain rmdir; it stays best effort because the
            // filesystem that broke the first step may refuse it too.
            let _ = unlinkat(&self.file, name, AtFlags::REMOVEDIR);
        }
        child
    }

    pub fn names(&self, deadline: Instant) -> Result<Vec<String>, SkillSecError> {
        let mut names = Vec::new();
        for item in Dir::read_from(&self.file).map_err(|e| io_error(&self.path, e))? {
            check_deadline(deadline)?;
            let item = item.map_err(|e| io_error(&self.path, e))?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| SkillSecError::Integrity("non-UTF-8 ledger entry".into()))?;
            if !matches!(name, "." | "..") {
                names.push(name.to_owned());
            }
            if names.len() > 100_000 {
                return Err(SkillSecError::Invalid(
                    "directory exceeds 100000 entries".into(),
                ));
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn read(
        &self,
        name: &str,
        limit: u64,
        deadline: Instant,
    ) -> Result<Vec<u8>, SkillSecError> {
        validate_name(name)?;
        check_deadline(deadline)?;
        let path = self.path.join(name);
        let file = File::from(
            openat(&self.file, name, READ_FLAGS, Mode::empty()).map_err(|e| io_error(&path, e))?,
        );
        let before = file.metadata().map_err(|e| io_error(&path, e))?;
        if !before.is_file() || before.nlink() != 1 || before.len() > limit {
            return Err(SkillSecError::Integrity(format!(
                "unsafe or oversized ledger file: {name}"
            )));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&path, e))?;
        check_deadline(deadline)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
            return Err(SkillSecError::Integrity(format!(
                "ledger file exceeded limit: {name}"
            )));
        }
        Ok(bytes)
    }

    pub fn write_atomic(
        &self,
        name: &str,
        bytes: &[u8],
        replace: bool,
    ) -> Result<File, SkillSecError> {
        validate_name(name)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            return Err(SkillSecError::Invalid("ledger record exceeds 8 MiB".into()));
        }
        let temporary = nonce(".record-")?;
        let result = (|| {
            let file = self.write_new(&temporary, bytes, 0o644)?;
            renameat_with(
                &self.file,
                temporary.as_str(),
                &self.file,
                name,
                if replace {
                    RenameFlags::empty()
                } else {
                    RenameFlags::NOREPLACE
                },
            )
            .map_err(|e| io_error(self.path.join(name), e))?;
            if let Err(failure) = self.sync() {
                if !replace {
                    // The NOREPLACE rename already succeeded, so the entry at
                    // `name` is the one this call just created. A caller that
                    // sees the error must not also find a half-committed
                    // record blocking its next NOREPLACE attempt with EEXIST:
                    // remove it, still identity-checked through the pinned
                    // handle in case another writer already replaced it.
                    // Replacing writes keep the entry — removing it would
                    // destroy the record that was overwritten.
                    if let Ok(identity) = EntryIdentity::of(&file, &self.path.join(name)) {
                        let _ = self.remove_child_if_same(
                            name,
                            &identity,
                            Instant::now() + Duration::from_secs(5),
                        );
                    }
                }
                return Err(failure);
            }
            Ok(file)
        })();
        let _ = unlinkat(&self.file, temporary.as_str(), AtFlags::empty());
        result
    }

    pub fn write_new(
        &self,
        name: &str,
        bytes: &[u8],
        mode: rustix::fs::RawMode,
    ) -> Result<File, SkillSecError> {
        validate_name(name)?;
        let path = self.path.join(name);
        let mut file = File::from(
            openat(
                &self.file,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(mode & 0o777),
            )
            .map_err(|e| io_error(&path, e))?,
        );
        file.write_all(bytes).map_err(|e| io_error(&path, e))?;
        rustix::fs::fchmod(&file, Mode::from_raw_mode(mode & 0o777))
            .map_err(|e| io_error(&path, e))?;
        file.sync_all().map_err(|e| io_error(&path, e))?;
        Ok(file)
    }

    pub fn sync(&self) -> Result<(), SkillSecError> {
        self.file.sync_all().map_err(|e| io_error(&self.path, e))
    }

    pub fn verify_path(&self) -> Result<(), SkillSecError> {
        let current = open_directory(&self.path)?
            .metadata()
            .map_err(|e| io_error(&self.path, e))?;
        let pinned = self.file.metadata().map_err(|e| io_error(&self.path, e))?;
        if (current.dev(), current.ino()) != (pinned.dev(), pinned.ino()) {
            return Err(SkillSecError::Integrity(
                "ledger directory was replaced during operation".into(),
            ));
        }
        Ok(())
    }

    pub fn rename_child(&self, from: &str, to: &str) -> Result<(), SkillSecError> {
        validate_name(from)?;
        validate_name(to)?;
        renameat_with(&self.file, from, &self.file, to, RenameFlags::NOREPLACE)
            .map_err(|e| io_error(self.path.join(to), e))?;
        self.sync()
    }

    pub fn remove_child(&self, name: &str, deadline: Instant) -> Result<(), SkillSecError> {
        validate_name(name)?;
        check_deadline(deadline)?;
        let stat = match statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => return Err(io_error(self.path.join(name), error)),
        };
        let flags = if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
            let child = self.child(name, false)?;
            for entry in child.names(deadline)? {
                child.remove_child(&entry, deadline)?;
            }
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        unlinkat(&self.file, name, flags).map_err(|e| io_error(self.path.join(name), e))?;
        self.sync()
    }

    /// Removes `name` only while it is still the entry `identity` describes.
    ///
    /// Returns `Ok(false)` — touching nothing — when the name is already gone
    /// or was replaced by a different inode, so an error cleanup can never
    /// delete an artifact another invocation created at the same name (for
    /// example a concurrent export that raced past an empty-destination
    /// check). The check and the removal both go through the pinned directory
    /// handle, so they agree on the same directory even if the path was
    /// swapped; the residual window between the two syscalls only matters to
    /// an actor racing the cleanup itself.
    pub fn remove_child_if_same(
        &self,
        name: &str,
        identity: &EntryIdentity,
        deadline: Instant,
    ) -> Result<bool, SkillSecError> {
        validate_name(name)?;
        check_deadline(deadline)?;
        let stat = match statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(false),
            Err(error) => return Err(io_error(self.path.join(name), error)),
        };
        if (stat.st_dev, stat.st_ino) != (identity.device, identity.inode) {
            return Ok(false);
        }
        self.remove_child(name, deadline)?;
        Ok(true)
    }
}

pub(crate) fn missing(error: &SkillSecError) -> bool {
    matches!(error, SkillSecError::Io {source, ..} if source.kind() == std::io::ErrorKind::NotFound)
}

pub(crate) fn nonce(prefix: &str) -> Result<String, SkillSecError> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| SkillSecError::Key)?;
    Ok(format!(
        "{prefix}{}",
        crate::integrity::digest(&bytes).trim_start_matches("sha256:")
    ))
}

fn validate_name(name: &str) -> Result<(), SkillSecError> {
    if matches!(name, "" | "." | "..") || name.contains(['/', '\0']) {
        Err(SkillSecError::Invalid("invalid ledger entry name".into()))
    } else {
        Ok(())
    }
}

pub(crate) fn set_owner(file: &File, uid: u32, path: &Path) -> Result<(), SkillSecError> {
    rustix::fs::fchown(file, Some(rustix::process::Uid::from_raw(uid)), None)
        .map_err(|e| io_error(path, e))?;
    file.sync_all().map_err(|e| io_error(path, e))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn fresh_child_refuses_an_existing_name_without_touching_it() {
        // A competing writer already created the child: the caller gets
        // EEXIST and the existing entry — partial output of that other
        // invocation — must survive untouched.
        let temporary = tempfile::tempdir().unwrap();
        let parent = Directory::open(&temporary.path().canonicalize().unwrap()).unwrap();
        let existing = parent.fresh_child("snapshot").unwrap();
        fs::write(existing.path.join("partial"), b"other export").unwrap();

        let error = parent
            .fresh_child("snapshot")
            .err()
            .expect("fresh_child must fail on an existing name");
        assert!(
            matches!(error, SkillSecError::Io { ref source, .. }
                if source.kind() == std::io::ErrorKind::AlreadyExists),
            "expected EEXIST, got: {error}"
        );
        assert_eq!(
            fs::read(existing.path.join("partial")).unwrap(),
            b"other export"
        );
        assert_eq!(
            parent.names(deadline()).unwrap(),
            vec!["snapshot".to_owned()]
        );
    }

    #[test]
    fn remove_child_if_same_never_deletes_a_replaced_entry() {
        // The entry this invocation created was swapped for another writer's
        // tree at the same name: the identity no longer matches, so the
        // cleanup must skip it, and only the matching identity removes it.
        let temporary = tempfile::tempdir().unwrap();
        let parent = Directory::open(&temporary.path().canonicalize().unwrap()).unwrap();
        let ours = parent.fresh_child("snapshot").unwrap();
        let identity = EntryIdentity::of(&ours.file, &ours.path).unwrap();

        // Competing writer: move ours away and put a fresh tree at the name.
        fs::rename(&ours.path, temporary.path().join("moved")).unwrap();
        let foreign = parent.fresh_child("snapshot").unwrap();
        fs::write(foreign.path.join("marker"), b"foreign").unwrap();

        assert!(
            !parent
                .remove_child_if_same("snapshot", &identity, deadline())
                .unwrap()
        );
        assert_eq!(
            fs::read(foreign.path.join("marker")).unwrap(),
            b"foreign",
            "the replaced entry must survive"
        );
        assert!(temporary.path().join("moved").exists());

        // The current entry's own identity removes it.
        let current = EntryIdentity::of(&foreign.file, &foreign.path).unwrap();
        assert!(
            parent
                .remove_child_if_same("snapshot", &current, deadline())
                .unwrap()
        );
        assert!(!foreign.path.exists());
        assert!(temporary.path().join("moved").exists());
    }

    #[test]
    fn remove_child_if_same_preserves_an_entry_on_a_different_device() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = Directory::open(&temporary.path().canonicalize().unwrap()).unwrap();
        let child = parent.fresh_child("snapshot").unwrap();
        let mut identity = EntryIdentity::of(&child.file, &child.path).unwrap();
        identity.device ^= 1;

        assert!(
            !parent
                .remove_child_if_same("snapshot", &identity, deadline())
                .unwrap()
        );
        assert!(child.path.exists());
    }

    #[test]
    fn remove_child_if_same_treats_a_missing_entry_as_removed() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = Directory::open(&temporary.path().canonicalize().unwrap()).unwrap();
        let ours = parent.fresh_child("manifest.json").unwrap();
        let identity = EntryIdentity::of(&ours.file, &ours.path).unwrap();
        fs::remove_dir(&ours.path).unwrap();

        assert!(
            !parent
                .remove_child_if_same("manifest.json", &identity, deadline())
                .unwrap()
        );
    }
}
