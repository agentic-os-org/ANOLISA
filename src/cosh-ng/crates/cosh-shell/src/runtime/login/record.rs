mod scan;

use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::account::{supported_bash, Account};
use super::{Error, Result, COSH_PATH};

const LIMIT: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Prepared,
    Applied,
    Restored,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub schema: u32,
    pub username: String,
    pub uid: u32,
    pub original_shell: String,
    pub cosh_shell: String,
    pub original_sha256: String,
    pub phase: Phase,
}

impl Record {
    pub fn validate(&self, account: &Account) -> Result<()> {
        self.validate_shape()?;
        if self.username != account.name || self.uid != account.uid {
            return Err(Error::new(
                "login record identity does not match the account",
            ));
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        if self.schema != 1
            || !super::account::valid_name(&self.username)
            || !supported_bash(&self.original_shell)
            || self.cosh_shell != COSH_PATH
            || self.original_sha256.len() != 64
            || !self.original_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::new(
                "untrusted login record; administrator review is required",
            ));
        }
        Ok(())
    }
}

pub(super) struct Store {
    path: PathBuf,
    owner: u32,
    #[cfg(test)]
    fixture: bool,
}

impl Store {
    pub fn system() -> Self {
        Self {
            path: PathBuf::from("/var/lib/cosh/login"),
            owner: 0,
            #[cfg(test)]
            fixture: false,
        }
    }

    #[cfg(test)]
    pub fn fixture(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            owner: unsafe { nix::libc::geteuid() },
            fixture: true,
        }
    }

    fn directory(&self, create: bool) -> Result<Option<File>> {
        #[cfg(test)]
        if self.fixture {
            let name = CString::new(self.path.as_os_str().as_encoded_bytes())
                .map_err(|_| Error::new("invalid fixture path"))?;
            let fd = unsafe {
                nix::libc::open(
                    name.as_ptr(),
                    nix::libc::O_RDONLY
                        | nix::libc::O_DIRECTORY
                        | nix::libc::O_NOFOLLOW
                        | nix::libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(Error::io(std::io::Error::last_os_error()));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            self.validate(&file, true)?;
            return Ok(Some(file));
        }
        self.directory_from(
            File::open("/").map_err(Error::io)?,
            &self.path,
            create,
            File::sync_all,
        )
    }

    fn directory_from(
        &self,
        mut directory: File,
        path: &Path,
        create: bool,
        mut sync_parent: impl FnMut(&File) -> std::io::Result<()>,
    ) -> Result<Option<File>> {
        for component in path.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            self.validate(&directory, true)?;
            let name = CString::new(name.as_encoded_bytes())
                .map_err(|_| Error::new("invalid record directory"))?;
            let mut fd = unsafe {
                nix::libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    nix::libc::O_RDONLY
                        | nix::libc::O_DIRECTORY
                        | nix::libc::O_NOFOLLOW
                        | nix::libc::O_CLOEXEC,
                )
            };
            let mut created = false;
            if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                if !create {
                    return Ok(None);
                }
                let made =
                    unsafe { nix::libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) };
                if made != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(Error::io(std::io::Error::last_os_error()));
                }
                created = made == 0;
                fd = unsafe {
                    nix::libc::openat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        nix::libc::O_RDONLY
                            | nix::libc::O_DIRECTORY
                            | nix::libc::O_NOFOLLOW
                            | nix::libc::O_CLOEXEC,
                    )
                };
            }
            if fd < 0 {
                return Err(Error::io(std::io::Error::last_os_error()));
            }
            let child = unsafe { File::from_raw_fd(fd) };
            if created && unsafe { nix::libc::fchmod(child.as_raw_fd(), 0o755) } != 0 {
                return Err(Error::io(std::io::Error::last_os_error()));
            }
            if create {
                child.sync_all().map_err(Error::io)?;
                sync_parent(&directory).map_err(Error::io)?;
            }
            directory = child;
        }
        self.validate(&directory, true)?;
        Ok(Some(directory))
    }

    fn validate(&self, file: &File, directory: bool) -> Result<()> {
        let metadata = file.metadata().map_err(Error::io)?;
        if metadata.uid() != self.owner
            || metadata.mode() & 0o022 != 0
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file() || metadata.nlink() != 1
            }
        {
            return Err(Error::new("unsafe administrator login record path"));
        }
        Ok(())
    }

    fn open_file(&self, directory: &File, name: &str, create: bool) -> Result<Option<File>> {
        let name = CString::new(name).map_err(|_| Error::new("invalid record name"))?;
        let fd = unsafe {
            nix::libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                (if create {
                    nix::libc::O_RDWR | nix::libc::O_CREAT
                } else {
                    nix::libc::O_RDONLY
                }) | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::NotFound && !create {
                Ok(None)
            } else {
                Err(Error::io(error))
            };
        }
        let file = unsafe { File::from_raw_fd(fd) };
        self.validate(&file, false)?;
        Ok(Some(file))
    }

    pub fn read(&self, uid: u32) -> Result<Option<Record>> {
        let Some(directory) = self.directory(false)? else {
            return Ok(None);
        };
        self.read_at(&directory, uid)
    }

    fn read_at(&self, directory: &File, uid: u32) -> Result<Option<Record>> {
        let Some(file) = self.open_file(directory, &format!("{uid}.json"), false)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take(LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(Error::io)?;
        if bytes.len() > LIMIT {
            return Err(Error::new("login record exceeds size limit"));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Error::new("corrupt login record; administrator review is required"))
    }

    pub fn lock(&self) -> Result<File> {
        let directory = self.directory(true)?.expect("created record directory");
        let file = self
            .open_file(&directory, ".lock", true)?
            .expect("created lock");
        if file.metadata().map_err(Error::io)?.mode() & 0o077 != 0 {
            return Err(Error::new(
                "unsafe administrator transaction lock permissions",
            ));
        }
        crate::config::private_state::try_lock_exclusive(&file).map_err(|error| {
            Error::new(format!("login transaction lock unavailable: {error:?}"))
        })?;
        Ok(file)
    }

    pub fn write(&self, record: &Record) -> Result<()> {
        let directory = self.directory(true)?.expect("created record directory");
        let name = format!("{}.json", record.uid);
        self.open_file(&directory, &name, false)?;
        let bytes =
            serde_json::to_vec(record).map_err(|_| Error::new("unable to encode login record"))?;
        if bytes.len() > LIMIT {
            return Err(Error::new("login record exceeds size limit"));
        }
        let temporary = format!(".{}.tmp", uuid::Uuid::new_v4().simple());
        let temporary_name = CString::new(temporary).unwrap();
        let name = CString::new(name).unwrap();
        let fd = unsafe {
            nix::libc::openat(
                directory.as_raw_fd(),
                temporary_name.as_ptr(),
                nix::libc::O_WRONLY
                    | nix::libc::O_CREAT
                    | nix::libc::O_EXCL
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            return Err(Error::io(std::io::Error::last_os_error()));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            self.validate(&file, false)?;
            if unsafe { nix::libc::fchmod(file.as_raw_fd(), 0o644) } != 0 {
                return Err(Error::io(std::io::Error::last_os_error()));
            }
            file.write_all(&bytes).map_err(Error::io)?;
            file.sync_all().map_err(Error::io)?;
            if unsafe {
                nix::libc::renameat(
                    directory.as_raw_fd(),
                    temporary_name.as_ptr(),
                    directory.as_raw_fd(),
                    name.as_ptr(),
                )
            } != 0
            {
                return Err(Error::io(std::io::Error::last_os_error()));
            }
            directory.sync_all().map_err(Error::io)
        })();
        if result.is_err() {
            unsafe {
                nix::libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests;
