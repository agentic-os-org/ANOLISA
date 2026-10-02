//! Process-group primitives for one-shot child processes.
//!
//! `child.kill()` only reaches the direct child; grandchildren spawned by
//! `sh -c` scripts survive it. Spawning the command as the leader of a fresh
//! process group lets callers tear down the whole descendant tree with a
//! single `killpg` on timeout.

use std::process::Command;

use nix::errno::Errno;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;

/// Makes the spawned child the leader of a new process group (pgid == pid).
///
/// Must be applied before `spawn()`. Pair with [`kill_process_group`] to
/// reap every descendant of the command on timeout.
pub fn isolate_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

/// Sends `SIGKILL` to the whole process group led by `pgid`.
///
/// `ESRCH` means the group already exited and is treated as success. The
/// caller must still `wait()` on the direct child to reap it.
///
/// # Errors
///
/// Returns the underlying errno for any failure other than `ESRCH` so the
/// caller can record the leaked group.
pub fn kill_process_group(pgid: u32) -> Result<(), Errno> {
    match killpg(Pid::from_raw(pgid as i32), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(e) => Err(e),
    }
}
