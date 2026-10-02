use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use nix::libc;

static SIGNAL_OUTPUT_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_OUTPUT_IS_TTY: AtomicBool = AtomicBool::new(false);
static SIGNAL_RECOVERY_ARMED: AtomicBool = AtomicBool::new(false);

const MODIFY_OTHER_KEYS_DISABLE: &[u8] = b"\x1b[>4;0m";

fn arm_signal_recovery(output_fd: i32) {
    SIGNAL_OUTPUT_FD.store(output_fd, Ordering::Relaxed);
    SIGNAL_OUTPUT_IS_TTY.store(unsafe { libc::isatty(output_fd) == 1 }, Ordering::Relaxed);
    SIGNAL_RECOVERY_ARMED.store(true, Ordering::Release);
}

fn disarm_signal_recovery() {
    SIGNAL_RECOVERY_ARMED.store(false, Ordering::Release);
    SIGNAL_OUTPUT_FD.store(-1, Ordering::Relaxed);
    SIGNAL_OUTPUT_IS_TTY.store(false, Ordering::Relaxed);
}

pub(crate) fn restore_raw_mode_signal_state() {
    if !SIGNAL_RECOVERY_ARMED.load(Ordering::Acquire) {
        return;
    }
    let output_fd = SIGNAL_OUTPUT_FD.load(Ordering::Relaxed);
    if output_fd < 0 {
        return;
    }
    unsafe {
        let output_flags = libc::fcntl(output_fd, libc::F_GETFL);
        if output_flags < 0 {
            return;
        }
        let write_flags = if SIGNAL_OUTPUT_IS_TTY.load(Ordering::Relaxed) {
            output_flags & !libc::O_NONBLOCK
        } else {
            output_flags | libc::O_NONBLOCK
        };
        libc::fcntl(output_fd, libc::F_SETFL, write_flags);
        libc::write(
            output_fd,
            MODIFY_OTHER_KEYS_DISABLE.as_ptr().cast(),
            MODIFY_OTHER_KEYS_DISABLE.len(),
        );
        libc::fcntl(output_fd, libc::F_SETFL, output_flags);
    }
}

/// Re-open stdout on a fresh, blocking file description when needed.
///
/// On Linux terminals (and SSH sessions), stdin and stdout often share the
/// same underlying open file description. `RawModeGuard` sets `O_NONBLOCK` on
/// stdin (fd 0), which therefore also makes stdout (fd 1) non-blocking.
/// Subsequent writes to stdout can then return `EAGAIN` / `EWOULDBLOCK`.
///
/// This function opens `/dev/tty` to obtain a new, independent file
/// description that is not marked `O_NONBLOCK`, and uses `dup2` to replace
/// fd 1 with it. The termios configuration is per-device, so the new fd
/// inherits the raw-mode settings already applied by `RawModeGuard`.
///
/// If stdout is not a terminal, or if `/dev/tty` cannot be opened, the
/// function logs a warning and returns success so that the shell can still
/// start. In that case the caller retains the original (possibly
/// non-blocking) stdout behavior.
pub(super) fn reopen_stdout_blocking() -> io::Result<()> {
    if unsafe { libc::isatty(libc::STDOUT_FILENO) } == 0 {
        // stdout is not a terminal, so the stdin/stdout shared file
        // description problem does not apply here.
        return Ok(());
    }
    let tty = match OpenOptions::new().write(true).open("/dev/tty") {
        Ok(tty) => tty,
        Err(err) => {
            eprintln!(
                "cosh-shell: warning: cannot reopen stdout as blocking: {err}; \
                 EAGAIN risk remains"
            );
            return Ok(());
        }
    };
    let tty_fd = tty.as_raw_fd();
    if unsafe { libc::dup2(tty_fd, libc::STDOUT_FILENO) } < 0 {
        let err = io::Error::last_os_error();
        eprintln!(
            "cosh-shell: warning: cannot reopen stdout as blocking: {err}; \
             EAGAIN risk remains"
        );
    }
    // `tty` is dropped here, but the duplicated fd remains alive because fd 1
    // now refers to the same open file description.
    Ok(())
}

pub(super) struct RawModeGuard {
    fd: i32,
    /// Where the modifyOtherKeys withdrawal is written. The enable travels
    /// on the relay's stdout path, so the withdrawal must reach the same
    /// terminal path; stdin may be a read-only or separately opened
    /// descriptor that never carries bytes to the terminal.
    output_fd: i32,
    original_termios: Option<libc::termios>,
    original_flags: i32,
    active: bool,
    recovery_armed: bool,
}

impl RawModeGuard {
    /// Field order mirrors the struct: `(fd, output_fd, original_termios,
    /// original_flags)`; unlike `activate_fd_with_output` the termios and
    /// flags are injected instead of being probed from the fd.
    #[cfg(test)]
    pub(super) fn for_test(
        fd: i32,
        output_fd: i32,
        original_termios: Option<libc::termios>,
        original_flags: i32,
    ) -> Self {
        Self {
            fd,
            output_fd,
            original_termios,
            original_flags,
            active: true,
            recovery_armed: false,
        }
    }

    /// #1932 F4: modifyOtherKeys level 1 makes the terminal report
    /// modifier-carrying editing keys (Shift+Enter -> `CSI 27;2;13~`)
    /// that already sit on the soft-newline whitelist, with zero terminal
    /// configuration. Level 1 leaves every conventionally-encoded key
    /// (Esc, Alt+letter, Ctrl+letter) untouched, and terminals without
    /// the feature ignore the sequence entirely. The enable is written on
    /// the relay's ordered stdout path; this guard owns the withdrawal and
    /// writes it to the same stdout path so the tty never keeps the mode
    /// after exit.
    pub(super) fn activate_stdin() -> io::Result<Option<Self>> {
        Self::activate_fd_with_output(0, libc::STDOUT_FILENO)
    }

    #[cfg(test)]
    pub(super) fn activate_fd(fd: i32) -> io::Result<Option<Self>> {
        Self::activate_fd_with_output(fd, fd)
    }

    fn activate_fd_with_output(fd: i32, output_fd: i32) -> io::Result<Option<Self>> {
        let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if original_flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let original_termios = if unsafe { libc::isatty(fd) } == 1 {
            let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
            if unsafe { libc::tcgetattr(fd, &mut original) } < 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::fcntl(fd, libc::F_SETFL, original_flags);
                }
                return Err(error);
            }

            let mut raw = original;
            unsafe {
                libc::cfmakeraw(&mut raw);
            }
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } < 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::fcntl(fd, libc::F_SETFL, original_flags);
                }
                return Err(error);
            }
            Some(original)
        } else {
            None
        };

        let recovery_armed = fd == libc::STDIN_FILENO && original_termios.is_some();
        if recovery_armed {
            arm_signal_recovery(output_fd);
        }

        Ok(Some(Self {
            fd,
            output_fd,
            original_termios,
            original_flags,
            active: true,
            recovery_armed,
        }))
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.active {
            // Clear O_NONBLOCK temporarily so the cleanup write and termios
            // restore cannot be lost to EAGAIN. This is necessary even if the
            // descriptor inherited O_NONBLOCK from the parent process, because
            // original_flags would then still contain that bit and a plain
            // restore would leave the fd non-blocking during the write.
            // The actual original flags are restored after the cleanup.
            unsafe {
                libc::fcntl(
                    self.fd,
                    libc::F_SETFL,
                    self.original_flags & !libc::O_NONBLOCK,
                );
            }
            if let Some(original) = &self.original_termios {
                // Withdraw the keyboard negotiation before handing the tty
                // back (#1932 F4); paired with the enable the relay writes on
                // the stdout path, so the withdrawal targets the same path.
                if self.output_fd == self.fd {
                    unsafe {
                        libc::write(
                            self.fd,
                            MODIFY_OTHER_KEYS_DISABLE.as_ptr().cast(),
                            MODIFY_OTHER_KEYS_DISABLE.len(),
                        );
                    }
                } else {
                    // Mirroring the input-fd handling above, O_NONBLOCK is
                    // cleared around the write so the disable sequence
                    // cannot be lost to EAGAIN on an inherited non-blocking
                    // stdout — but only when the output fd is a terminal.
                    // With stdout on a pipe the enable never reached the
                    // terminal either, so the withdrawal flips to a
                    // non-blocking best-effort write instead: a blocking
                    // write into a full pipe would stall exit before the
                    // termios restore below.
                    //
                    // The flags are snapshotted now, not at activation:
                    // fd 1 may have been replaced afterwards (see
                    // reopen_stdout_blocking) and would otherwise get flags
                    // belonging to the old file description. A failed
                    // F_GETFL means the fd is no longer usable, so the
                    // write is skipped instead of running unguarded.
                    let flags = unsafe { libc::fcntl(self.output_fd, libc::F_GETFL) };
                    if flags >= 0 {
                        let write_flags = if unsafe { libc::isatty(self.output_fd) } == 1 {
                            flags & !libc::O_NONBLOCK
                        } else {
                            flags | libc::O_NONBLOCK
                        };
                        unsafe {
                            libc::fcntl(self.output_fd, libc::F_SETFL, write_flags);
                            libc::write(
                                self.output_fd,
                                MODIFY_OTHER_KEYS_DISABLE.as_ptr().cast(),
                                MODIFY_OTHER_KEYS_DISABLE.len(),
                            );
                            libc::fcntl(self.output_fd, libc::F_SETFL, flags);
                        }
                    }
                }
                unsafe {
                    libc::tcsetattr(self.fd, libc::TCSANOW, original);
                }
            }
            // Restore the exact flags we inherited, even if they included
            // O_NONBLOCK.
            unsafe {
                libc::fcntl(self.fd, libc::F_SETFL, self.original_flags);
            }
            if self.recovery_armed {
                disarm_signal_recovery();
            }
        }
    }
}
