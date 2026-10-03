//! Platform process-group isolation and signalling.

use std::fmt;
use std::io;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{killpg, Signal};
#[cfg(unix)]
use nix::unistd::Pid;

/// Lifecycle operations used to isolate and terminate one runtime process group.
///
/// Implementations are injected into the supervisor so lifecycle tests can
/// observe escalation without duplicating OS process ownership elsewhere.
pub trait ProcessGroupLifecycle: fmt::Debug + Send + Sync {
    /// Configures a command before spawn so the child leads a dedicated group.
    fn configure(&self, command: &mut Command);

    /// Sends a graceful termination signal to the complete process group.
    ///
    /// # Errors
    ///
    /// Returns an OS error when the group exists but cannot be signalled.
    fn terminate(&self, process_group: u32) -> io::Result<()>;

    /// Sends an unconditional kill signal to the complete process group.
    ///
    /// # Errors
    ///
    /// Returns an OS error when the group exists but cannot be signalled.
    fn kill(&self, process_group: u32) -> io::Result<()>;
}

/// Native process-group implementation used by production supervisors.
#[derive(Debug, Default)]
pub struct PlatformProcessGroup;

impl ProcessGroupLifecycle for PlatformProcessGroup {
    fn configure(&self, command: &mut Command) {
        #[cfg(unix)]
        command.process_group(0);
    }

    fn terminate(&self, process_group: u32) -> io::Result<()> {
        signal_group(process_group, GroupSignal::Terminate)
    }

    fn kill(&self, process_group: u32) -> io::Result<()> {
        signal_group(process_group, GroupSignal::Kill)
    }
}

#[derive(Debug, Clone, Copy)]
enum GroupSignal {
    Terminate,
    Kill,
}

#[cfg(unix)]
fn signal_group(process_group: u32, signal: GroupSignal) -> io::Result<()> {
    let signal = match signal {
        GroupSignal::Terminate => Signal::SIGTERM,
        GroupSignal::Kill => Signal::SIGKILL,
    };
    match killpg(Pid::from_raw(process_group as i32), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

#[cfg(not(unix))]
fn signal_group(_process_group: u32, _signal: GroupSignal) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process-group signalling is unavailable on this platform",
    ))
}
