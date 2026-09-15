//! Pinned directories and atomic files; caller-controlled entries are never followed or truncated.

use crate::filesystem::{READ_FLAGS, open_directory};
use crate::{GuardError, check_deadline, io_error};
use ring::rand::{SecureRandom as _, SystemRandom};
use rustix::fs::{
    AtFlags, Dir, Mode, OFlags, RenameFlags, mkdirat, openat, renameat_with, unlinkat,
};
use rustix::fs::{FileType, statat};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) struct Directory {
    pub file: File,
    pub path: PathBuf,
}

impl Directory {
    pub fn open(path: &Path) -> Result<Self, GuardError> {
        Ok(Self {
            file: open_directory(path)?,
            path: path.into(),
        })
    }

    pub fn child(&self, name: &str, create: bool) -> Result<Self, GuardError> {
        validate_name(name)?;
        let path = self.path.join(name);
        if create {
            match mkdirat(&self.file, name, Mode::from_raw_mode(0o755)) {
                Ok(()) => self.sync()?,
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
        Ok(Self { file, path })
    }

    pub fn fresh_child(&self, name: &str) -> Result<Self, GuardError> {
        validate_name(name)?;
        mkdirat(&self.file, name, Mode::from_raw_mode(0o755))
            .map_err(|e| io_error(self.path.join(name), e))?;
        self.sync()?;
        self.child(name, false)
    }

    pub fn names(&self, deadline: Instant) -> Result<Vec<String>, GuardError> {
        let mut names = Vec::new();
        for item in Dir::read_from(&self.file).map_err(|e| io_error(&self.path, e))? {
            check_deadline(deadline)?;
            let item = item.map_err(|e| io_error(&self.path, e))?;
            let name = item
                .file_name()
                .to_str()
                .map_err(|_| GuardError::Integrity("non-UTF-8 ledger entry".into()))?;
            if !matches!(name, "." | "..") {
                names.push(name.to_owned());
            }
            if names.len() > 100_000 {
                return Err(GuardError::Invalid(
                    "directory exceeds 100000 entries".into(),
                ));
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn read(&self, name: &str, limit: u64, deadline: Instant) -> Result<Vec<u8>, GuardError> {
        validate_name(name)?;
        check_deadline(deadline)?;
        let path = self.path.join(name);
        let file = File::from(
            openat(&self.file, name, READ_FLAGS, Mode::empty()).map_err(|e| io_error(&path, e))?,
        );
        let before = file.metadata().map_err(|e| io_error(&path, e))?;
        if !before.is_file() || before.nlink() != 1 || before.len() > limit {
            return Err(GuardError::Integrity(format!(
                "unsafe or oversized ledger file: {name}"
            )));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&path, e))?;
        check_deadline(deadline)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
            return Err(GuardError::Integrity(format!(
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
    ) -> Result<File, GuardError> {
        validate_name(name)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            return Err(GuardError::Invalid("ledger record exceeds 8 MiB".into()));
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
            self.sync()?;
            Ok(file)
        })();
        let _ = unlinkat(&self.file, temporary.as_str(), AtFlags::empty());
        result
    }

    pub fn write_new(&self, name: &str, bytes: &[u8], mode: u32) -> Result<File, GuardError> {
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
        file.sync_all().map_err(|e| io_error(&path, e))?;
        Ok(file)
    }

    pub fn sync(&self) -> Result<(), GuardError> {
        self.file.sync_all().map_err(|e| io_error(&self.path, e))
    }

    pub fn verify_path(&self) -> Result<(), GuardError> {
        let current = open_directory(&self.path)?
            .metadata()
            .map_err(|e| io_error(&self.path, e))?;
        let pinned = self.file.metadata().map_err(|e| io_error(&self.path, e))?;
        if (current.dev(), current.ino()) != (pinned.dev(), pinned.ino()) {
            return Err(GuardError::Integrity(
                "ledger directory was replaced during operation".into(),
            ));
        }
        Ok(())
    }

    pub fn rename_child(&self, from: &str, to: &str) -> Result<(), GuardError> {
        validate_name(from)?;
        validate_name(to)?;
        renameat_with(&self.file, from, &self.file, to, RenameFlags::NOREPLACE)
            .map_err(|e| io_error(self.path.join(to), e))?;
        self.sync()
    }

    pub fn remove_child(&self, name: &str, deadline: Instant) -> Result<(), GuardError> {
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
}

pub(crate) fn missing(error: &GuardError) -> bool {
    matches!(error, GuardError::Io {source, ..} if source.kind() == std::io::ErrorKind::NotFound)
}

pub(crate) fn nonce(prefix: &str) -> Result<String, GuardError> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| GuardError::Key)?;
    Ok(format!(
        "{prefix}{}",
        crate::integrity::digest(&bytes).trim_start_matches("sha256:")
    ))
}

fn validate_name(name: &str) -> Result<(), GuardError> {
    if matches!(name, "" | "." | "..") || name.contains(['/', '\0']) {
        Err(GuardError::Invalid("invalid ledger entry name".into()))
    } else {
        Ok(())
    }
}

pub(crate) fn set_owner(file: &File, uid: u32, path: &Path) -> Result<(), GuardError> {
    rustix::fs::fchown(file, Some(rustix::process::Uid::from_raw(uid)), None)
        .map_err(|e| io_error(path, e))?;
    file.sync_all().map_err(|e| io_error(path, e))
}
