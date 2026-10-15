//! Process and Unix-socket ownership for one UI worker generation.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use crate::ui::worker_protocol::{ProtocolLimits, FRAME_LENGTH_PREFIX_LEN, PROTOCOL_HEADER_LEN};

#[derive(Debug)]
pub(crate) enum ProcessError {
    Io(io::Error),
    FrameTooLarge,
    TruncatedFrame,
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "worker process I/O failed: {error}"),
            Self::FrameTooLarge => write!(formatter, "worker frame exceeded the injected ceiling"),
            Self::TruncatedFrame => write!(formatter, "worker closed IPC during a frame"),
        }
    }
}

impl std::error::Error for ProcessError {}

impl From<io::Error> for ProcessError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
struct FrameReader {
    prefix: [u8; FRAME_LENGTH_PREFIX_LEN],
    prefix_read: usize,
    frame: Vec<u8>,
    frame_read: usize,
}

impl FrameReader {
    fn new() -> Self {
        Self {
            prefix: [0; FRAME_LENGTH_PREFIX_LEN],
            prefix_read: 0,
            frame: Vec::new(),
            frame_read: 0,
        }
    }

    fn read(
        &mut self,
        stream: &mut UnixStream,
        limits: ProtocolLimits,
    ) -> Result<Option<Vec<u8>>, ProcessError> {
        while self.prefix_read < FRAME_LENGTH_PREFIX_LEN {
            match stream.read(&mut self.prefix[self.prefix_read..]) {
                Ok(0) if self.prefix_read == 0 => {
                    return Err(ProcessError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "worker closed IPC",
                    )))
                }
                Ok(0) => return Err(ProcessError::TruncatedFrame),
                Ok(read) => self.prefix_read += read,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(ProcessError::Io(error)),
            }
        }

        if self.frame.is_empty() {
            let frame_len = u32::from_le_bytes(self.prefix) as usize;
            let maximum = PROTOCOL_HEADER_LEN
                .checked_add(limits.max_payload_bytes)
                .ok_or(ProcessError::FrameTooLarge)?;
            if frame_len > maximum {
                return Err(ProcessError::FrameTooLarge);
            }
            let total_len = FRAME_LENGTH_PREFIX_LEN
                .checked_add(frame_len)
                .ok_or(ProcessError::FrameTooLarge)?;
            self.frame = vec![0; total_len];
            self.frame[..FRAME_LENGTH_PREFIX_LEN].copy_from_slice(&self.prefix);
            self.frame_read = FRAME_LENGTH_PREFIX_LEN;
        }

        while self.frame_read < self.frame.len() {
            match stream.read(&mut self.frame[self.frame_read..]) {
                Ok(0) => return Err(ProcessError::TruncatedFrame),
                Ok(read) => self.frame_read += read,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(ProcessError::Io(error)),
            }
        }

        self.prefix_read = 0;
        self.frame_read = 0;
        Ok(Some(std::mem::take(&mut self.frame)))
    }
}

pub(super) struct WorkerProcess {
    child: Option<Child>,
    ipc: UnixStream,
    reader: FrameReader,
}

fn move_above_standard_fds(stream: UnixStream) -> Result<UnixStream, ProcessError> {
    if stream.as_raw_fd() >= 3 {
        return Ok(stream);
    }
    let duplicated = unsafe { nix::libc::fcntl(stream.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(ProcessError::Io(io::Error::last_os_error()));
    }
    let moved = unsafe { UnixStream::from_raw_fd(duplicated) };
    drop(stream);
    Ok(moved)
}

fn isolate_before_exec(child_fd: i32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        if nix::libc::setsid() < 0 {
            return Err(io::Error::last_os_error());
        }
        if nix::libc::syscall(nix::libc::SYS_close_range, 3u32, u32::MAX, 4u32) < 0 {
            let error = io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(nix::libc::ENOSYS | nix::libc::EINVAL)
            ) {
                return Err(error);
            }
            mark_inherited_fds_cloexec()?;
        }
        if child_fd != 3 && nix::libc::dup2(child_fd, 3) < 0 {
            return Err(io::Error::last_os_error());
        }
        if nix::libc::fcntl(3, nix::libc::F_SETFD, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = child_fd;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "worker isolation requires Linux close_range(CLOEXEC)",
        ))
    }
}

#[cfg(target_os = "linux")]
fn mark_inherited_fds_cloexec() -> io::Result<()> {
    // Enumerate after fork with raw syscalls: no allocation, fd race, or exec-pipe closure.
    let directory = unsafe {
        nix::libc::open(
            c"/proc/self/fd".as_ptr(),
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
        )
    };
    if directory < 0 {
        return Err(io::Error::last_os_error());
    }
    let directory = unsafe { std::os::fd::OwnedFd::from_raw_fd(directory) };
    let mut entries = [0u8; 4096];
    loop {
        let read = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getdents64,
                directory.as_raw_fd(),
                entries.as_mut_ptr(),
                entries.len(),
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if read == 0 {
            return Ok(());
        }
        let read = read as usize;
        let mut offset = 0;
        while offset < read {
            if read - offset < 20 {
                return Err(io::Error::from_raw_os_error(nix::libc::EIO));
            }
            let length = u16::from_ne_bytes([entries[offset + 16], entries[offset + 17]]) as usize;
            if length < 20 || length > read - offset {
                return Err(io::Error::from_raw_os_error(nix::libc::EIO));
            }
            let name = &entries[offset + 19..offset + length];
            offset += length;
            if name[0] == b'.' {
                continue;
            }
            let mut fd = 0i32;
            for &digit in name.iter().take_while(|&&digit| digit != 0) {
                if !digit.is_ascii_digit() {
                    return Err(io::Error::from_raw_os_error(nix::libc::EIO));
                }
                fd = fd
                    .checked_mul(10)
                    .and_then(|fd| fd.checked_add(i32::from(digit - b'0')))
                    .ok_or_else(|| io::Error::from_raw_os_error(nix::libc::EIO))?;
            }
            if fd < 3 {
                continue;
            }
            let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) };
            if flags < 0
                || unsafe {
                    nix::libc::fcntl(fd, nix::libc::F_SETFD, flags | nix::libc::FD_CLOEXEC)
                } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
    }
}

impl WorkerProcess {
    pub(super) fn spawn(
        binary_path: &Path,
        limits: ProtocolLimits,
        heartbeat_interval: Duration,
    ) -> Result<Self, ProcessError> {
        let (host, child_ipc) = UnixStream::pair()?;
        host.set_nonblocking(true)?;
        let child_ipc = move_above_standard_fds(child_ipc)?;
        let child_fd = child_ipc.as_raw_fd();
        let mut command = Command::new(binary_path);
        command
            .env_clear()
            .current_dir("/")
            .arg("--ipc-fd")
            .arg("3")
            .arg("--max-payload-bytes")
            .arg(limits.max_payload_bytes.to_string())
            .arg("--max-draw-ops")
            .arg(limits.max_draw_ops.to_string())
            .arg("--max-text-bytes")
            .arg(limits.max_text_bytes.to_string())
            .arg("--heartbeat-interval-nanos")
            .arg(heartbeat_interval.as_nanos().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || isolate_before_exec(child_fd));
        }
        let child = command.spawn()?;
        drop(child_ipc);
        Ok(Self {
            child: Some(child),
            ipc: host,
            reader: FrameReader::new(),
        })
    }

    pub(super) fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    pub(super) fn try_status(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        let status = child.try_wait()?;
        if status.is_some() {
            self.child = None;
        }
        Ok(status)
    }

    pub(super) fn write(&mut self, bytes: &[u8], offset: &mut usize) -> Result<bool, ProcessError> {
        while *offset < bytes.len() {
            match self.ipc.write(&bytes[*offset..]) {
                Ok(0) => {
                    return Err(ProcessError::Io(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "worker IPC accepted zero bytes",
                    )))
                }
                Ok(written) => *offset += written,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(ProcessError::Io(error)),
            }
        }
        Ok(true)
    }

    pub(super) fn read_frame(
        &mut self,
        limits: ProtocolLimits,
    ) -> Result<Option<Vec<u8>>, ProcessError> {
        self.reader.read(&mut self.ipc, limits)
    }

    pub(super) fn terminate_and_reap(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        let Some(mut child) = self.child.take() else {
            return Ok(None);
        };
        let outcome = kill_and_reap(&mut child);
        if outcome.is_err() {
            // `Child::drop` does not reap, so an error must not lose the handle.
            // Keeping it lets `Drop` retry and keeps the worker visible to `pid()`
            // instead of turning it into a zombie nobody can report or clean up.
            self.child = Some(child);
        }
        outcome
    }
}

fn kill_and_reap(child: &mut Child) -> Result<Option<ExitStatus>, ProcessError> {
    if let Some(status) = child.try_wait()? {
        return Ok(Some(status));
    }
    match child.kill() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(ProcessError::Io(error)),
    }
    child.wait().map(Some).map_err(ProcessError::Io)
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
