//! Terminal-mode recovery after cosh interrupts a foreground command.

use std::io;
use std::path::Path;

use nix::libc;

use super::{
    shell_has_active_foreground_command, shell_has_completed_foreground_command, OscParser,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalRecoveryOwner {
    CoshTimeoutInterrupt,
}

#[derive(Default)]
pub(super) struct PendingTerminalRecovery {
    owner: Option<TerminalRecoveryOwner>,
    snapshot: Option<libc::termios>,
}

impl PendingTerminalRecovery {
    pub(super) fn record_intervention_start(&mut self, terminal_fd: i32) {
        self.snapshot = read_recoverable_terminal_snapshot(terminal_fd)
            .ok()
            .flatten();
        self.owner = None;
    }

    pub(super) fn mark_owner(&mut self, owner: TerminalRecoveryOwner, terminal_fd: i32) {
        if self.snapshot.is_none() {
            self.snapshot = read_recoverable_terminal_snapshot(terminal_fd)
                .ok()
                .flatten();
        }
        self.owner = Some(owner);
    }

    fn clear(&mut self) {
        self.owner = None;
        self.snapshot = None;
    }

    pub(super) fn restore_modes(&self, terminal_fd: i32) -> io::Result<()> {
        if self.owner.is_none() {
            return Ok(());
        }
        if let Some(snapshot) = self.snapshot {
            restore_pty_terminal_modes_from_snapshot(terminal_fd, snapshot)
        } else {
            restore_pty_terminal_modes_to_minimal_sane(terminal_fd)
        }
    }

    pub(super) fn request_shell_recovery(&self, path: &Path) -> io::Result<()> {
        if self.owner.is_none() {
            return Ok(());
        }
        std::fs::write(path, b"1")
    }
}

pub(super) fn restore_terminal_after_interrupted_command(
    terminal_fd: i32,
    parser: &OscParser,
    pending_terminal_restore: &mut PendingTerminalRecovery,
) -> io::Result<bool> {
    if pending_terminal_restore.owner.is_none()
        || shell_has_active_foreground_command(&parser.events)
        || !shell_has_completed_foreground_command(&parser.events)
    {
        return Ok(false);
    }
    pending_terminal_restore.restore_modes(terminal_fd)?;
    pending_terminal_restore.clear();
    Ok(false)
}

fn read_pty_terminal_modes(terminal_fd: i32) -> io::Result<libc::termios> {
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(terminal_fd, &mut termios) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(termios)
}

fn read_recoverable_terminal_snapshot(terminal_fd: i32) -> io::Result<Option<libc::termios>> {
    let termios = read_pty_terminal_modes(terminal_fd)?;
    if terminal_modes_look_like_external_command_state(&termios) {
        Ok(Some(termios))
    } else {
        Ok(None)
    }
}

fn terminal_modes_look_like_external_command_state(termios: &libc::termios) -> bool {
    let required = libc::ECHO | libc::ICANON | libc::ISIG;
    termios.c_lflag & required == required
}

fn restore_pty_terminal_modes_from_snapshot(
    terminal_fd: i32,
    snapshot: libc::termios,
) -> io::Result<()> {
    if unsafe { libc::tcsetattr(terminal_fd, libc::TCSANOW, &snapshot) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn restore_pty_terminal_modes_to_minimal_sane(terminal_fd: i32) -> io::Result<()> {
    let mut termios = read_pty_terminal_modes(terminal_fd)?;
    termios.c_lflag |= libc::ICANON | libc::ISIG | libc::IEXTEN | libc::ECHO;
    termios.c_iflag |= libc::ICRNL | libc::IXON;
    termios.c_oflag |= libc::OPOST;
    if unsafe { libc::tcsetattr(terminal_fd, libc::TCSANOW, &termios) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
