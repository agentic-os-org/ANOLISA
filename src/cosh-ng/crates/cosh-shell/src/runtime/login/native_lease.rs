use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{account, Error, Result};

const LOCK_PATH: &str = "/usr/lib/sysimage/rpm/.rpm.lock";

// vipw must retain this OFD lease after the parent exits; do not explicitly unlock on Drop.
pub(super) struct NativeLease {
    file: File,
}

impl NativeLease {
    pub(super) fn acquire() -> Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(Error::new("native RPM transaction leases require Linux"));
        }
        account::require_platform()?;
        validate_effective_path(&rpm_output(&["--eval", "%{_rpmlock_path}"])?)?;
        let path = account::trusted_path(Path::new(LOCK_PATH), false)?;
        if path != Path::new(LOCK_PATH) {
            return Err(Error::new(
                "native RPM lock path is outside the qualified database",
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
            .open(&path)
            .map_err(Error::io)?;
        validate_metadata(&file, 0)?;
        let lease = Self::lock_file(file)?;
        validate_metadata(&lease.file, 0)?;
        let held = lease.file.metadata().map_err(Error::io)?;
        let current = std::fs::symlink_metadata(LOCK_PATH).map_err(Error::io)?;
        if held.dev() != current.dev() || held.ino() != current.ino() {
            return Err(Error::new(
                "native RPM lock inode changed during acquisition",
            ));
        }
        validate_effective_path(&rpm_output(&["--eval", "%{_rpmlock_path}"])?)?;
        validate_build(
            &rpm_output(&[
                "-q",
                "--qf",
                "%{VERSION}-%{RELEASE}.%{ARCH}\n",
                "rpm",
                "rpm-libs",
            ])?,
            std::env::consts::ARCH,
        )?;
        Ok(lease)
    }

    #[cfg(target_os = "linux")]
    fn lock_file(file: File) -> Result<Self> {
        let copied = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 4) };
        if copied < 0 {
            return Err(Error::io(std::io::Error::last_os_error()));
        }
        let file = unsafe { File::from_raw_fd(copied) };
        let mut lock: nix::libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = nix::libc::F_RDLCK as _;
        lock.l_whence = nix::libc::SEEK_SET as _;
        if unsafe {
            nix::libc::fcntl(
                file.as_raw_fd(),
                nix::libc::F_OFD_SETLK,
                &lock as *const nix::libc::flock,
            )
        } < 0
        {
            return Err(Error::new(format!(
                "native RPM transaction lease unavailable: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self { file })
    }

    #[cfg(not(target_os = "linux"))]
    fn lock_file(_file: File) -> Result<Self> {
        Err(Error::new(
            "native RPM OFD transaction leases require Linux",
        ))
    }

    pub(super) fn inherit_to_vipw(&self, command: &mut Command) {
        let fd = self.file.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                let flags = nix::libc::fcntl(fd, nix::libc::F_GETFD);
                if flags < 0
                    || nix::libc::fcntl(fd, nix::libc::F_SETFD, flags & !nix::libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

fn validate_effective_path(bytes: &[u8]) -> Result<()> {
    if bytes != format!("{LOCK_PATH}\n").as_bytes() {
        return Err(Error::new(
            "effective RPM lock path is outside the qualified database",
        ));
    }
    Ok(())
}

fn validate_metadata(file: &File, owner: u32) -> Result<()> {
    let metadata = file.metadata().map_err(Error::io)?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.mode() & 0o6022 != 0
    {
        return Err(Error::new("unsafe native RPM lock inode"));
    }
    Ok(())
}

fn validate_build(bytes: &[u8], arch: &str) -> Result<()> {
    if !matches!(arch, "aarch64" | "x86_64") {
        return Err(Error::new(
            "native RPM transaction lease architecture is not qualified",
        ));
    }
    let build = format!("4.18.2-6.alnx4.{arch}\n");
    if bytes != format!("{build}{build}").as_bytes() {
        return Err(Error::new(
            "native RPM transaction lease requires the qualified ALinux4 RPM build",
        ));
    }
    Ok(())
}

fn rpm_output(args: &[&str]) -> Result<Vec<u8>> {
    let mut child = account::command("/usr/bin/rpm")?
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::io)?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("missing RPM query output"))?
        .take(1025)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > 1024 {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::new(
            "native RPM lease query exceeded its output limit",
        ));
    }
    if !child.wait().map_err(Error::io)?.success() {
        return Err(Error::new("native RPM lease query failed"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
