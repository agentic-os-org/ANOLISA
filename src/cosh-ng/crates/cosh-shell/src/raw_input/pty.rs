use std::fs::File;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use nix::libc;
use nix::pty::Winsize;

pub(crate) fn set_pty_winsize(fd: i32, winsize: Winsize) -> io::Result<()> {
    let result = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as libc::c_ulong, &winsize) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn signal_process_group(child_pid: u32, signal: i32) -> io::Result<()> {
    signal_process_group_id(child_pid as i32, signal)
}

pub(crate) fn signal_foreground_process_group(
    master_fd: i32,
    terminal_fd: i32,
    fallback_child_pid: u32,
    signal: i32,
) -> io::Result<()> {
    if let Some(process_group) = foreground_process_group_for_fds(master_fd, terminal_fd) {
        return signal_process_group_id(process_group, signal);
    }
    signal_process_group(fallback_child_pid, signal)
}

pub(crate) fn foreground_process_group_for_fds(master_fd: i32, terminal_fd: i32) -> Option<i32> {
    foreground_process_group(master_fd).or_else(|| foreground_process_group(terminal_fd))
}

pub(crate) fn process_group_exists(process_group: i32) -> bool {
    let result = unsafe { libc::kill(-process_group, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn foreground_process_group(fd: i32) -> Option<i32> {
    let mut process_group: libc::pid_t = 0;
    let result = unsafe { libc::ioctl(fd, libc::TIOCGPGRP as libc::c_ulong, &mut process_group) };
    if result == 0 && process_group > 0 {
        Some(process_group as i32)
    } else {
        None
    }
}

pub(crate) fn signal_process_group_id(process_group: i32, signal: i32) -> io::Result<()> {
    let result = unsafe { libc::kill(-process_group, signal) };
    if result < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

thread_local! {
    static NONBLOCKING_REPLAY_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static DEFERRED_REPLAY_BYTES: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static COMMITTED_REPLAY_BYTES: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(super) struct NonblockingReplayWrite {
    previous_active: bool,
    previous_bytes: Vec<u8>,
    previous_committed: Vec<u8>,
}

impl NonblockingReplayWrite {
    #[cfg(test)]
    pub(super) fn is_active() -> bool {
        NONBLOCKING_REPLAY_WRITE.with(|active| active.get())
    }

    pub(super) fn enter(master: &File) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_NONBLOCK == 0 {
            return Err(io::Error::other(
                "protected replay requires a nonblocking PTY",
            ));
        }
        Ok(Self {
            previous_active: NONBLOCKING_REPLAY_WRITE.with(|active| active.replace(true)),
            previous_bytes: DEFERRED_REPLAY_BYTES
                .with(|bytes| std::mem::take(&mut *bytes.borrow_mut())),
            previous_committed: COMMITTED_REPLAY_BYTES
                .with(|bytes| std::mem::take(&mut *bytes.borrow_mut())),
        })
    }

    pub(super) fn take_deferred(&self) -> Vec<u8> {
        DEFERRED_REPLAY_BYTES.with(|bytes| std::mem::take(&mut *bytes.borrow_mut()))
    }

    pub(super) fn take_committed(&self) -> Vec<u8> {
        COMMITTED_REPLAY_BYTES.with(|bytes| std::mem::take(&mut *bytes.borrow_mut()))
    }
}

impl Drop for NonblockingReplayWrite {
    fn drop(&mut self) {
        NONBLOCKING_REPLAY_WRITE.with(|active| active.set(self.previous_active));
        DEFERRED_REPLAY_BYTES
            .with(|bytes| *bytes.borrow_mut() = std::mem::take(&mut self.previous_bytes));
        COMMITTED_REPLAY_BYTES
            .with(|bytes| *bytes.borrow_mut() = std::mem::take(&mut self.previous_committed));
    }
}

pub(crate) fn write_all_pty(master: &mut File, mut bytes: &[u8]) -> io::Result<()> {
    let protected = NONBLOCKING_REPLAY_WRITE.with(|active| active.get());
    if protected && DEFERRED_REPLAY_BYTES.with(|pending| !pending.borrow().is_empty()) {
        DEFERRED_REPLAY_BYTES.with(|pending| pending.borrow_mut().extend_from_slice(bytes));
        return Ok(());
    }
    while !bytes.is_empty() {
        match master.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write to PTY",
                ));
            }
            Ok(n) => {
                if protected {
                    COMMITTED_REPLAY_BYTES
                        .with(|committed| committed.borrow_mut().extend_from_slice(&bytes[..n]));
                }
                bytes = &bytes[n..];
            }
            Err(err) if protected && err.kind() == io::ErrorKind::WouldBlock => {
                DEFERRED_REPLAY_BYTES.with(|pending| pending.borrow_mut().extend_from_slice(bytes));
                return Ok(());
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(err) => return Err(err),
        }
    }
    master.flush()
}

#[cfg(test)]
mod replay_write_tests {
    use super::*;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;

    #[test]
    fn r10_protected_write_guard_restores_nested_and_error_state() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let mut master = unsafe { File::from_raw_fd(stream.into_raw_fd()) };
        let payload = [b'x'; 8192];
        loop {
            match master.write(&payload) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill writer: {error}"),
            }
        }
        assert!(!NONBLOCKING_REPLAY_WRITE.with(|active| active.get()));
        {
            let _outer = NonblockingReplayWrite::enter(&master).unwrap();
            {
                let inner = NonblockingReplayWrite::enter(&master).unwrap();
                write_all_pty(&mut master, b"blocked").unwrap();
                assert_eq!(inner.take_deferred(), b"blocked");
            }
            assert!(NONBLOCKING_REPLAY_WRITE.with(|active| active.get()));
        }
        assert!(!NONBLOCKING_REPLAY_WRITE.with(|active| active.get()));
    }
}
