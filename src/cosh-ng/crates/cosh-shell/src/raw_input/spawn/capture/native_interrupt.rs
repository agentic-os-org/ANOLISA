use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

use nix::libc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeCtrlC {
    NonTerminal,
    Literal,
    Signal { flush: bool },
}

pub(super) fn classify_native_ctrl_c(terminal: Option<&File>) -> io::Result<NativeCtrlC> {
    let Some(terminal) = terminal else {
        return Ok(NativeCtrlC::NonTerminal);
    };
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(terminal.as_raw_fd(), &mut termios) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if termios.c_lflag & libc::ISIG == 0 || termios.c_cc[libc::VINTR] != 0x03 {
        return Ok(NativeCtrlC::Literal);
    }
    Ok(NativeCtrlC::Signal {
        flush: termios.c_lflag & libc::NOFLSH == 0,
    })
}

pub(super) fn signal_native_replay_interrupt(
    master: &File,
    terminal: &File,
    policy: NativeCtrlC,
) -> io::Result<bool> {
    let NativeCtrlC::Signal { flush } = policy else {
        return Ok(false);
    };
    // Retain the original slave: reopening it can fail after foreground TIOCEXCL.
    if cfg!(target_os = "linux")
        && flush
        && unsafe { libc::tcflush(terminal.as_raw_fd(), libc::TCIOFLUSH) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // macOS TIOCSIG applies NOFLSH itself; Linux needs the explicit slave flush above.
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSIG as _, libc::SIGINT) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}
