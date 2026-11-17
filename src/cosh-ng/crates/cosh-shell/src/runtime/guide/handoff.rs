//! Explicit "ask questions" handoff: the user asked to leave the wizard and
//! start a Cosh session. Configuration never uses this path — it runs as a
//! finite in-process task instead.

use std::os::unix::process::CommandExt;
use std::process::Command;

use nix::libc;

pub(crate) fn start_questions() -> i32 {
    let result = std::env::current_exe().and_then(|exe| {
        let _interrupts = IgnoreInterrupts::new();
        Command::new(exe)
            .arg0("cosh")
            .env_remove("COSH_GUIDE_AUTH_HANDOFF")
            .status()
    });
    match result {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            tracing::warn!(operation = "start_questions", kind = ?error.kind(), "guide operation failed");
            eprintln!("cosh: {error}");
            1
        }
    }
}

/// The child shares the foreground process group, so a Ctrl-C aimed at the
/// auth session would also reach the waiting wizard; ignore it for the
/// handoff and restore the prior handlers afterwards.
/// The child inherits SIG_IGN only until main installs terminal recovery handlers.
struct IgnoreInterrupts {
    previous_int: libc::sighandler_t,
    previous_quit: libc::sighandler_t,
}

impl IgnoreInterrupts {
    fn new() -> Self {
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
