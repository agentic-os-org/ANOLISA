use std::io::{self, Write};
use std::sync::atomic::{AtomicI32, Ordering};

use nix::libc;

mod guide_screen;

static mut ORIGINAL_TERMIOS: Option<libc::termios> = None;
static ORIGINAL_FILE_STATUS_FLAGS: AtomicI32 = AtomicI32::new(-1);
static GUIDE_SCREEN: guide_screen::GuideScreenRecovery = guide_screen::GuideScreenRecovery::new();

// Signal-driven exit bypasses `Screen::drop`, so the full-screen guide would
// otherwise leave the tty in the alternate screen with a hidden cursor. Only
// armed while this process actually owns the alternate screen, so the inline
// shell never leaves a parent terminal's alternate screen.
const LEAVE_ALT_SHOW_CURSOR: &[u8] = b"\x1b[?1049l\x1b[?25h";

pub(crate) fn guide_screen_event(event: crate::ui::guide::ScreenEvent) -> bool {
    match GUIDE_SCREEN.update(event) {
        guide_screen::ScreenUpdate::Continue => true,
        guide_screen::ScreenUpdate::Stopped => false,
        guide_screen::ScreenUpdate::WaitForExit => wait_for_signal_exit(),
        guide_screen::ScreenUpdate::Exit(signal) => {
            finish_signal_exit(signal);
            wait_for_signal_exit()
        }
    }
}

fn wait_for_signal_exit() -> ! {
    // Returning to main could exit before the other thread restores the tty.
    loop {
        unsafe { libc::pause() };
    }
}

pub(crate) struct CrLfWriter<'a, W: Write> {
    inner: &'a mut W,
}

/// The child shares the foreground process group, so a Ctrl-C aimed at the
/// child would also reach the waiting parent; ignore it for the handoff and
/// restore the prior handlers afterwards.
/// The child inherits SIG_IGN only until it installs its own handlers.
pub(crate) struct IgnoreInterrupts {
    previous_int: libc::sighandler_t,
    previous_quit: libc::sighandler_t,
}

impl IgnoreInterrupts {
    pub(crate) fn new() -> Self {
        unsafe {
            Self {
                previous_int: libc::signal(libc::SIGINT, libc::SIG_IGN),
                previous_quit: libc::signal(libc::SIGQUIT, libc::SIG_IGN),
            }
        }
    }
}

impl Drop for IgnoreInterrupts {
    fn drop(&mut self) {
        unsafe {
            libc::signal(libc::SIGINT, self.previous_int);
            libc::signal(libc::SIGQUIT, self.previous_quit);
        }
    }
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
        if GUIDE_SCREEN.take_active() && libc::isatty(libc::STDOUT_FILENO) == 1 {
            libc::write(
                libc::STDOUT_FILENO,
                LEAVE_ALT_SHOW_CURSOR.as_ptr().cast(),
                LEAVE_ALT_SHOW_CURSOR.len(),
            );
        }
    }
}

extern "C" fn restore_and_exit(sig: libc::c_int) {
    if GUIDE_SCREEN.defer_signal(sig) {
        return;
    }
    finish_signal_exit(sig);
}

fn finish_signal_exit(sig: libc::c_int) {
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
