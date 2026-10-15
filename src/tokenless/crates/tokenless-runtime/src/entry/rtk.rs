//! Deadline-bound RTK output collection with exclusive process-group ownership.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use crate::RuntimeError;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

/// Collects UTF-8 stdout and exit status under the same operation deadline.
pub(super) fn run(
    mut command: Command,
    path: &Path,
    timeout: Duration,
) -> Result<(ExitStatus, String), RuntimeError> {
    let child = command
        .process_group(0)
        .spawn()
        .map_err(|source| RuntimeError::RtkSpawn {
            path: path.to_path_buf(),
            source,
        })?;
    let mut owned = OwnedChild {
        child,
        reaped: false,
    };
    let result = collect_output(&mut owned, timeout);
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = owned.cleanup();
            return Err(error);
        }
    };
    let status = owned.cleanup().map_err(RuntimeError::RtkWait)?;
    let stdout = String::from_utf8(bytes).map_err(|error| {
        RuntimeError::RtkOutput(io::Error::new(io::ErrorKind::InvalidData, error))
    })?;
    Ok((status, stdout))
}

fn collect_output(owned: &mut OwnedChild, timeout: Duration) -> Result<Vec<u8>, RuntimeError> {
    let mut pipe = owned.child.stdout.take().ok_or_else(|| {
        RuntimeError::RtkOutput(io::Error::other("RTK stdout pipe was not created"))
    })?;
    make_nonblocking(&pipe).map_err(RuntimeError::RtkOutput)?;
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut buffer = [0; 16 * 1024];
    let mut eof = false;
    loop {
        if started.elapsed() >= timeout {
            return Err(RuntimeError::RtkTimeout);
        }
        if !eof {
            match pipe.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(RuntimeError::RtkOutput(error)),
            }
        }
        if owned.observe_exit().map_err(RuntimeError::RtkWait)? && eof {
            return Ok(bytes);
        }
        thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
    }
}

fn make_nonblocking(pipe: &ChildStdout) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: the owned stdout pipe keeps this descriptor live across both calls.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct OwnedChild {
    child: Child,
    reaped: bool,
}

impl OwnedChild {
    fn observe_exit(&mut self) -> io::Result<bool> {
        // SAFETY: waitid fills this live siginfo allocation. WNOWAIT reserves the
        // leader PID until the final group signal, even when stdout outlives it.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.reaped = true;
            }
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        // SAFETY: successful waitid populated the PID, or left it zero.
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn signal_group(&mut self) -> io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        // SAFETY: this owner is the only reaper, so the unreaped group leader
        // reserves the PGID. No signal is sent after try_wait releases its PID.
        if unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) } < 0 {
            let error = io::Error::last_os_error();
            #[cfg(target_os = "macos")]
            if error.raw_os_error() == Some(libc::EPERM) && self.is_exited_group_leader()? {
                // Darwin excludes zombies from group signals, returning EPERM for an empty match.
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn is_exited_group_leader(&mut self) -> io::Result<bool> {
        if !self.observe_exit()? {
            return Ok(false);
        }
        let pid = self.child.id() as libc::pid_t;
        let mut members = [0 as libc::pid_t; 2];
        // SAFETY: the buffer holds two PIDs, and WNOWAIT still reserves the group leader.
        let count = unsafe {
            libc::proc_listpgrppids(
                pid,
                members.as_mut_ptr().cast(),
                std::mem::size_of_val(&members) as libc::c_int,
            )
        };
        Ok(count == 1 && members[0] == pid)
    }

    fn cleanup(&mut self) -> io::Result<ExitStatus> {
        let started = Instant::now();
        self.signal_group()?;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.reaped = true;
                    return Ok(status);
                }
                Err(error) => {
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        self.reaped = true;
                    }
                    return Err(error);
                }
                Ok(None) => {}
            }
            if started.elapsed() >= CLEANUP_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "RTK process cleanup timed out",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            // Unwinding must remain bounded even for a kernel-stuck child.
            let _ = self.signal_group();
            let _ = self.child.try_wait();
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;

    fn waiter(group: libc::pid_t) -> Child {
        Command::new("/bin/sh")
            .args(["-c", "read -r value; exit 3"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(group)
            .spawn()
            .unwrap()
    }

    fn finish_leader(owned: &mut OwnedChild) {
        drop(owned.child.stdin.take());
        let started = Instant::now();
        while !owned.observe_exit().unwrap() {
            assert!(started.elapsed() < Duration::from_secs(2));
            thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn singleton_zombie_retains_ownership_until_reaped() {
        let mut owned = OwnedChild {
            child: waiter(0),
            reaped: false,
        };
        finish_leader(&mut owned);
        assert!(owned.is_exited_group_leader().unwrap());
        assert!(!owned.reaped);
        assert_eq!(owned.cleanup().unwrap().code(), Some(3));
        assert!(owned.reaped);
        assert!(owned.signal_group().is_ok());
    }

    #[test]
    fn live_leader_cannot_bypass_group_signalling() {
        let mut owned = OwnedChild {
            child: waiter(0),
            reaped: false,
        };
        assert!(!owned.is_exited_group_leader().unwrap());
        assert_eq!(owned.cleanup().unwrap().signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn exited_leader_with_closed_stdout_still_cleans_live_members() {
        let mut owned = OwnedChild {
            child: waiter(0),
            reaped: false,
        };
        let mut member = waiter(owned.child.id() as libc::pid_t);
        finish_leader(&mut owned);
        let is_singleton = owned.is_exited_group_leader().unwrap();
        let status = owned.cleanup();
        let started = Instant::now();
        let member_status = loop {
            if let Some(status) = member.try_wait().unwrap() {
                break Some(status);
            }
            if started.elapsed() >= CLEANUP_TIMEOUT {
                member.kill().unwrap();
                member.wait().unwrap();
                break None;
            }
            thread::sleep(POLL_INTERVAL);
        };
        assert!(!is_singleton);
        assert_eq!(status.unwrap().code(), Some(3));
        assert_eq!(member_status.unwrap().signal(), Some(libc::SIGKILL));
    }
}
