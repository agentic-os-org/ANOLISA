use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::process::{ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use super::child::{
    ensure_waitable_children, set_nonblocking, CleanupScope, CommandChild, ReapOwner,
};
use super::job::{
    configure_cloexec, configure_isolated_worker, install_worker_timeout_handler, timer_remaining,
    JobDeadline,
};

const MAX_FRAME: usize = 4096;

pub(crate) struct BoundedWorker {
    child: Option<CommandChild<'static>>,
    stdout: Option<ChildStdout>,
    bytes: Vec<u8>,
    deadline: Instant,
    limit: usize,
    sealed: bool,
}

impl BoundedWorker {
    pub(crate) fn spawn(
        mut command: Command,
        request: &[u8],
        deadline: Instant,
        max_bytes: usize,
    ) -> io::Result<Self> {
        if max_bytes == 0 || max_bytes > MAX_FRAME || request.len() > max_bytes {
            return Err(io::Error::from_raw_os_error(nix::libc::EFBIG));
        }
        let job =
            JobDeadline::at_instant(deadline, deadline.saturating_duration_since(Instant::now()))?;
        if job.remaining()?.is_zero() {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure_isolated_worker(&mut command, job);
        configure_cloexec(&mut command, 3)?;
        ensure_waitable_children()?;
        let owner = ReapOwner::global()?;
        let permit = owner
            .reserve()
            .ok_or_else(|| io::Error::from_raw_os_error(nix::libc::EAGAIN))?;
        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let guard = permit.register(child, CleanupScope::OwnedGroup, deadline);
        let mut stdin = stdin.ok_or_else(|| io::Error::from_raw_os_error(nix::libc::EBADF))?;
        let stdout = stdout.ok_or_else(|| io::Error::from_raw_os_error(nix::libc::EBADF))?;
        let worker = Self {
            child: Some(guard),
            stdout: Some(stdout),
            bytes: Vec::new(),
            deadline,
            limit: max_bytes,
            sealed: false,
        };
        set_nonblocking(stdin.as_raw_fd())?;
        set_nonblocking(worker.stdout.as_ref().unwrap().as_raw_fd())?;
        let mut written = 0;
        while written < request.len() {
            if Instant::now() >= deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            match stdin.write(&request[written..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        drop(stdin);
        Ok(worker)
    }

    pub(crate) fn poll_ready(&mut self) -> Option<io::Result<Vec<u8>>> {
        if self.sealed {
            return None;
        }
        match self.read_available() {
            Ok(None) => None,
            Ok(Some(bytes)) => Some(self.finish().map(|()| bytes)),
            Err(error) => {
                self.cancel();
                Some(Err(error))
            }
        }
    }

    fn read_available(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut buffer = [0; 512];
        loop {
            if Instant::now() >= self.deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            let stream = self
                .stdout
                .as_mut()
                .ok_or_else(|| io::Error::from_raw_os_error(nix::libc::EBADF))?;
            match stream.read(&mut buffer) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(count) => {
                    if self.bytes.len().saturating_add(count) > self.limit + 1 {
                        return Err(io::Error::from_raw_os_error(nix::libc::EFBIG));
                    }
                    self.bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = self.bytes.iter().position(|byte| *byte == b'\n') {
                        if end > self.limit || end + 1 != self.bytes.len() {
                            return Err(io::Error::from(io::ErrorKind::InvalidData));
                        }
                        self.bytes.pop();
                        return Ok(Some(std::mem::take(&mut self.bytes)));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn cancel(&mut self) {
        let _ = self.finish();
    }

    fn finish(&mut self) -> io::Result<()> {
        self.sealed = true;
        self.stdout.take();
        if let Some(mut child) = self.child.take() {
            if child.cleanup()?.is_some() {
                return Ok(());
            }
        }
        Err(io::Error::from(io::ErrorKind::TimedOut))
    }
}

impl Drop for BoundedWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(crate) fn read_worker_request(
    max_bytes: usize,
    maximum_budget: Duration,
) -> io::Result<Vec<u8>> {
    install_worker_timeout_handler()?;
    let remaining = timer_remaining()?;
    if max_bytes == 0
        || max_bytes > MAX_FRAME
        || remaining.is_zero()
        || remaining > maximum_budget
        || unsafe { nix::libc::isatty(0) != 0 || nix::libc::isatty(1) != 0 }
    {
        return Err(io::Error::from_raw_os_error(nix::libc::EPERM));
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::from_raw_os_error(nix::libc::EFBIG));
    }
    Ok(bytes)
}

pub(crate) fn write_worker_reply(bytes: &[u8], max_bytes: usize) -> io::Result<()> {
    if max_bytes == 0 || max_bytes > MAX_FRAME || bytes.len() > max_bytes || bytes.contains(&b'\n')
    {
        return Err(io::Error::from_raw_os_error(nix::libc::EFBIG));
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

#[cfg(test)]
mod tests;
