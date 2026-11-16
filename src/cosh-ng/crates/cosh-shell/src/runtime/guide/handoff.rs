//! Runs the existing Cosh auth flow in a child session while the wizard is
//! suspended, reusing `/auth` instead of duplicating its state machine.

use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::runtime::terminal::IgnoreInterrupts;

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
