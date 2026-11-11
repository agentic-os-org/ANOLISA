use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::{account, native_lease::NativeLease, Error, Result, COSH_PATH};

pub(crate) const EDITOR_FLAG: &str = "--__cosh-login-editor-v1";
pub(crate) const CONTROL_FD: i32 = 3;
pub(crate) const TEMP_PATH: &str = "/etc/passwd.edit";
const MAX_CONTROL: usize = 4096;
const MAX_PASSWD: usize = 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Control {
    pub(super) username: String,
    pub(super) uid: u32,
    pub(super) expected_shell: String,
    pub(super) target_shell: String,
}

impl Control {
    fn validate(&self) -> Result<()> {
        if !account::valid_name(&self.username)
            || !((account::supported_bash(&self.expected_shell) && self.target_shell == COSH_PATH)
                || (self.expected_shell == COSH_PATH
                    && account::supported_bash(&self.target_shell)))
        {
            return Err(Error::new("invalid managed login editor request"));
        }
        Ok(())
    }
}

fn replace_shell(bytes: &[u8], control: &Control) -> Result<Vec<u8>> {
    control.validate()?;
    if bytes.len() > MAX_PASSWD || bytes.contains(&0) {
        return Err(Error::new("invalid account editor snapshot"));
    }
    let mut output = Vec::with_capacity(bytes.len() + control.target_shell.len());
    let mut matched = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        if body.is_empty() || body.starts_with(b"#") {
            output.extend_from_slice(line);
            continue;
        }
        let fields = body.split(|byte| *byte == b':').collect::<Vec<_>>();
        if fields.len() != 7 {
            return Err(Error::new("invalid account editor snapshot"));
        }
        let uid = std::str::from_utf8(fields[2])
            .ok()
            .and_then(|uid| uid.parse::<u32>().ok())
            .ok_or_else(|| Error::new("invalid account UID"))?;
        if fields[0] == control.username.as_bytes() || uid == control.uid {
            if matched
                || fields[0] != control.username.as_bytes()
                || uid != control.uid
                || fields[6] != control.expected_shell.as_bytes()
            {
                return Err(Error::new(
                    "account identity or shell changed under the native account lock",
                ));
            }
            matched = true;
            output.extend_from_slice(&body[..body.len() - fields[6].len()]);
            output.extend_from_slice(control.target_shell.as_bytes());
            if line.ends_with(b"\n") {
                output.push(b'\n');
            }
        } else {
            output.extend_from_slice(line);
        }
    }
    if !matched {
        return Err(Error::new(
            "account disappeared under the native account lock",
        ));
    }
    Ok(output)
}

fn edit_file(file: &mut File, control: &Control, owner: u32) -> i32 {
    let Ok(metadata) = file.metadata() else {
        return 97;
    };
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.nlink() != 1
        || metadata.mode() & 0o6022 != 0
        || metadata.len() > MAX_PASSWD as u64
    {
        return 97;
    }
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_err()
        || (&mut *file)
            .take(MAX_PASSWD as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
    {
        return 97;
    }
    let Ok(updated) = replace_shell(&bytes, control) else {
        return 96;
    };
    let Some(next_mtime) = metadata.mtime().checked_add(1) else {
        return 97;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_secs() as i64)
        .unwrap_or(next_mtime);
    let times = [
        nix::libc::timespec {
            tv_sec: metadata.atime() as _,
            tv_nsec: metadata.atime_nsec() as _,
        },
        nix::libc::timespec {
            tv_sec: next_mtime.max(now) as _,
            tv_nsec: 0,
        },
    ];
    if file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(&updated))
        .and_then(|_| file.set_len(updated.len() as u64))
        .is_err()
        || unsafe { nix::libc::futimens(file.as_raw_fd(), times.as_ptr()) } != 0
        || file.sync_all().is_err()
    {
        return 97;
    }
    0
}

fn read_control(file: &mut File, owner: u32) -> Result<Control> {
    let flags = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFL) };
    let metadata = file.metadata().map_err(Error::io)?;
    if flags < 0
        || flags & nix::libc::O_ACCMODE != nix::libc::O_RDONLY
        || !metadata.file_type().is_fifo()
        || metadata.uid() != owner
        || (cfg!(target_os = "linux") && metadata.mode() & 0o022 != 0)
        || flags & nix::libc::O_NONBLOCK == 0
    {
        return Err(Error::new("untrusted login editor control descriptor"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONTROL as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io)?;
    if bytes.len() > MAX_CONTROL {
        return Err(Error::new("oversized login editor request"));
    }
    let control: Control =
        serde_json::from_slice(&bytes).map_err(|_| Error::new("invalid login editor request"))?;
    control.validate()?;
    Ok(control)
}

pub(crate) fn run_editor() -> i32 {
    if !cfg!(target_os = "linux") || unsafe { nix::libc::geteuid() } != 0 {
        return 97;
    }
    let copied =
        unsafe { nix::libc::fcntl(CONTROL_FD, nix::libc::F_DUPFD_CLOEXEC, CONTROL_FD + 1) };
    if copied < 0 {
        return 97;
    }
    let mut control_file = unsafe { File::from_raw_fd(copied) };
    let Ok(control) = read_control(&mut control_file, 0) else {
        return 97;
    };
    let Ok(mut file) = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(TEMP_PATH)
    else {
        return 97;
    };
    edit_file(&mut file, &control, 0)
}

fn control_pipe(control: &Control) -> Result<File> {
    control.validate()?;
    let bytes =
        serde_json::to_vec(control).map_err(|_| Error::new("invalid login editor request"))?;
    if bytes.len() > MAX_CONTROL {
        return Err(Error::new("oversized login editor request"));
    }
    #[cfg(target_os = "linux")]
    let (reader, writer) =
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC | nix::fcntl::OFlag::O_NONBLOCK)
            .map_err(|error| Error::io(error.into()))?;
    #[cfg(not(target_os = "linux"))]
    let (reader, writer) = {
        let pair = nix::unistd::pipe().map_err(|error| Error::io(error.into()))?;
        for fd in [&pair.0, &pair.1] {
            if unsafe {
                nix::libc::fcntl(fd.as_raw_fd(), nix::libc::F_SETFD, nix::libc::FD_CLOEXEC)
            } < 0
                || unsafe {
                    nix::libc::fcntl(fd.as_raw_fd(), nix::libc::F_SETFL, nix::libc::O_NONBLOCK)
                } < 0
            {
                return Err(Error::io(std::io::Error::last_os_error()));
            }
        }
        pair
    };
    let mut writer = File::from(writer);
    writer.write_all(&bytes).map_err(Error::io)?;
    drop(writer);
    Ok(File::from(reader))
}

fn configure_editor(command: &mut Command, control: &File, parent_pid: u32) {
    let editor = format!("/proc/{parent_pid}/exe {EDITOR_FLAG} {CONTROL_FD}");
    command
        .args(["-q", "-p"])
        .env("EDITOR", &editor)
        .env("VISUAL", editor)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let source = control.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            if nix::libc::dup2(source, CONTROL_FD) < 0
                || nix::libc::fcntl(CONTROL_FD, nix::libc::F_SETFD, 0) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn inherit_store_lease(command: &mut Command, store_lock: &File) -> Result<File> {
    let fd = unsafe { nix::libc::fcntl(store_lock.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 4) };
    if fd < 0 {
        return Err(Error::io(std::io::Error::last_os_error()));
    }
    let lease = unsafe { File::from_raw_fd(fd) };
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
    Ok(lease)
}

pub(super) fn change_shell(
    expected: &account::Account,
    target: &str,
    native_lease: Option<&NativeLease>,
    store_lock: &File,
) -> Result<()> {
    if !cfg!(target_os = "linux") || unsafe { nix::libc::geteuid() } != 0 {
        return Err(Error::new(
            "native login editing requires the Linux root account",
        ));
    }
    let parent_pid = std::process::id();
    let mut image = File::open(format!("/proc/{parent_pid}/exe")).map_err(Error::io)?;
    let metadata = image.metadata().map_err(Error::io)?;
    let mut magic = [0u8; 4];
    image.read_exact(&mut magic).map_err(Error::io)?;
    if metadata.uid() != 0 || metadata.mode() & 0o6022 != 0 || magic != *b"\x7fELF" {
        return Err(Error::new(
            "the running account editor image is not trusted",
        ));
    }
    let control = control_pipe(&Control {
        username: expected.name.clone(),
        uid: expected.uid,
        expected_shell: expected.shell.clone(),
        target_shell: target.into(),
    })?;
    let mut command = account::command("/usr/sbin/vipw")?;
    configure_editor(&mut command, &control, parent_pid);
    let _store_lease = inherit_store_lease(&mut command, store_lock)?;
    if let Some(lease) = native_lease {
        lease.inherit_to_vipw(&mut command);
    }
    let status = command.status().map_err(Error::io)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            "native account editor refused the transaction; inspect login status before retrying",
        ))
    }
}

#[cfg(test)]
#[path = "editor/tests.rs"]
mod tests;
