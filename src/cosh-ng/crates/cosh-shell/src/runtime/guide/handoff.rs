//! Runs the existing Cosh auth flow in a child session while the wizard is
//! suspended, reusing `/auth` instead of duplicating its state machine.

use std::os::unix::process::CommandExt;
use std::process::Command;

use nix::libc;

pub(crate) fn run() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let _interrupts = IgnoreInterrupts::new();
    Command::new(exe)
        .arg0("cosh")
        .env("COSH_GUIDE_AUTH_HANDOFF", "1")
        .status()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The child shares the foreground process group, so a Ctrl-C aimed at the
/// child would also reach the waiting wizard; ignore it for the handoff and
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
