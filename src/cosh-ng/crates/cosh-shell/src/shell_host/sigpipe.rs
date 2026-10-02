//! SIGPIPE disposition inherited from the parent process.
//!
//! The Rust runtime rewrites SIGPIPE to SIG_IGN before `main`, so the true
//! inherited disposition is captured from an `.init_array` constructor that
//! runs earlier. On platforms without the capture the restore is a no-op and
//! child processes keep today's behavior (fail-safe).

use std::sync::atomic::{AtomicU8, Ordering};

const UNKNOWN: u8 = 0;
const DEFAULT: u8 = 1;
const IGNORED: u8 = 2;

static INHERITED: AtomicU8 = AtomicU8::new(UNKNOWN);

#[cfg(target_os = "linux")]
unsafe extern "C" fn capture_inherited_sigpipe() {
    use nix::libc;

    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut action) } == 0 {
        // A caught (custom handler) disposition maps to DEFAULT on purpose:
        // execve(2) resets caught signals to their default action in the new
        // image, so "default" is exactly what the inner shell would have
        // inherited from such a parent.
        let value = if action.sa_sigaction == libc::SIG_IGN {
            IGNORED
        } else {
            DEFAULT
        };
        INHERITED.store(value, Ordering::Relaxed);
    }
}

#[cfg(target_os = "linux")]
#[used]
#[link_section = ".init_array"]
static CAPTURE_INHERITED_SIGPIPE: unsafe extern "C" fn() = capture_inherited_sigpipe;

/// Restore the captured disposition in a child. Async-signal-safe; meant for
/// `pre_exec` on every user-shell spawn/exec path.
pub(crate) fn restore_in_child() -> std::io::Result<()> {
    use nix::libc;

    let handler = match INHERITED.load(Ordering::Relaxed) {
        IGNORED => libc::SIG_IGN,
        DEFAULT => libc::SIG_DFL,
        _ => return Ok(()),
    };
    if unsafe { libc::signal(libc::SIGPIPE, handler) } == libc::SIG_ERR {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
