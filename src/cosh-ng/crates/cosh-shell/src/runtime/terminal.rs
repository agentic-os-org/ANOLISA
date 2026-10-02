use std::io::{self, Write};
use std::sync::atomic::{AtomicI32, Ordering};

use nix::libc;

static mut ORIGINAL_TERMIOS: Option<libc::termios> = None;
static ORIGINAL_FILE_STATUS_FLAGS: AtomicI32 = AtomicI32::new(-1);

pub(crate) struct CrLfWriter<'a, W: Write> {
    inner: &'a mut W,
}

pub(crate) fn install_terminal_recovery() {
    let fd = libc::STDIN_FILENO;
    if unsafe { libc::isatty(fd) } != 1 {
        return;
    }
    let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(fd, &mut original) } < 0 {
        return;
    }
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0 {
        return;
    }
    unsafe { ORIGINAL_TERMIOS = Some(original) };
    ORIGINAL_FILE_STATUS_FLAGS.store(original_flags, Ordering::Release);

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev_hook(info);
    }));

    unsafe {
        libc::signal(
            libc::SIGINT,
            restore_and_exit as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            restore_and_exit as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGHUP,
            restore_and_exit as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGQUIT,
            restore_and_exit as *const () as libc::sighandler_t,
        );
    }
}

fn restore_terminal() {
    unsafe {
        let original_flags = ORIGINAL_FILE_STATUS_FLAGS.load(Ordering::Acquire);
        if original_flags >= 0 {
            // RawModeGuard temporarily forces blocking I/O for its cleanup so
            // the withdrawal cannot be lost to EAGAIN, then restores the
            // exact inherited flags after the terminal state is safe.
            libc::fcntl(
                libc::STDIN_FILENO,
                libc::F_SETFL,
                original_flags & !libc::O_NONBLOCK,
            );
        }
        crate::shell_host::restore_raw_mode_signal_state();
        if let Some(ref original) = ORIGINAL_TERMIOS {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original);
        }
        if original_flags >= 0 {
            libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, original_flags);
        }
    }
}

extern "C" fn restore_and_exit(sig: libc::c_int) {
    restore_terminal();
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

impl<'a, W: Write> CrLfWriter<'a, W> {
    pub(crate) fn new(inner: &'a mut W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for CrLfWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for byte in buf {
            if *byte == b'\n' {
                self.inner.write_all(b"\r\n")?;
            } else {
                self.inner.write_all(&[*byte])?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
