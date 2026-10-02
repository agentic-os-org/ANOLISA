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

pub(crate) fn write_all_pty(master: &mut File, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match master.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write to PTY",
                ));
            }
            Ok(n) => bytes = &bytes[n..],
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(err) => return Err(err),
        }
    }
    master.flush()
}
