//! Startup PATH proven by the managed shell, handed to provider processes.
//!
//! The process-wide `PATH` stays the inherited one: mutating the environment of
//! this multi-threaded process is unsound, and only provider and Core launches
//! need the login-profile PATH the user's shell actually ended up with.

use std::ffi::OsStr;
use std::process::Command;
use std::sync::OnceLock;

static TRUSTED_STARTUP_PATH: OnceLock<String> = OnceLock::new();

/// Records the managed shell's startup PATH; later reports are ignored.
pub(crate) fn record_trusted_startup_path(path: String) {
    let _ = TRUSTED_STARTUP_PATH.set(path);
}

/// Builds a provider command with the trusted startup PATH. Without a report
/// the child keeps the inherited environment and program lookup behavior.
pub(crate) fn command_with_trusted_startup_path(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    if let Some(path) = TRUSTED_STARTUP_PATH.get() {
        command.env("PATH", path);
    }
    command
}
