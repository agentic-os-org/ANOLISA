//! Exclusive child ownership and bounded Linux process-group cleanup.

use super::{io_error, POLL_INTERVAL};
use crate::{CommandSpec, Error};
use std::{
    fs, io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const CLEANUP_GRACE: Duration = Duration::from_secs(1);
const MAX_SCAN_ENTRIES: usize = 65_536;

pub(super) struct OwnedChild {
    child: Child,
    reaped: bool,
}

impl OwnedChild {
    pub(super) fn spawn(spec: &CommandSpec) -> Result<Self, Error> {
        Self::spawn_with_streams(spec, true)
    }

    pub(super) fn spawn_foreground(spec: &CommandSpec) -> Result<Self, Error> {
        Self::spawn_with_streams(spec, false)
    }

    fn spawn_with_streams(spec: &CommandSpec, piped: bool) -> Result<Self, Error> {
        let stream = || {
            if piped {
                Stdio::piped()
            } else {
                Stdio::inherit()
            }
        };
        let parent = std::process::id();
        let mut command = Command::new(&spec.program);
        // Nested Provider transports have separate process groups. Kill their
        // immediate command if the owning thread dies before cleanup can run.
        // SAFETY: the post-fork callback uses only async-signal-safe syscalls
        // and constructs an OS error; it takes no locks and does not allocate.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // Close the race where the parent died before prctl.
                if libc::getppid() as u32 != parent {
                    libc::_exit(127);
                }
                Ok(())
            });
        }
        let child = command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(&spec.environment)
            .stdin(stream())
            .stdout(stream())
            .stderr(stream())
            .process_group(0)
            .spawn()
            .map_err(|e| io_error("spawn", e))?;
        Ok(Self {
            child,
            reaped: false,
        })
    }

    pub(super) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn take_pipes(&mut self) -> Result<(ChildStdin, ChildStdout, ChildStderr), Error> {
        let missing = || io_error("pipe setup", io::Error::other("missing owned pipe"));
        Ok((
            self.child.stdin.take().ok_or_else(missing)?,
            self.child.stdout.take().ok_or_else(missing)?,
            self.child.stderr.take().ok_or_else(missing)?,
        ))
    }

    fn signal_group(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL)
    }

    pub(super) fn signal(&self, signal: i32) -> io::Result<()> {
        // SAFETY: the unreaped group leader reserves the PGID until the final
        // group signal. Exclusive child reaping is required by the public API.
        if unsafe { libc::kill(-(self.id() as i32), signal) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn observe_exit(&self) -> io::Result<bool> {
        // SAFETY: zero initializes siginfo_t; waitid fills this live allocation.
        // WNOWAIT observes termination without making the leader PID reusable.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        // SAFETY: successful waitid populated the child-event fields, or left PID zero.
        Ok(unsafe { info.si_pid() } != 0)
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + Duration::from_millis(250);
        while !self.observe_exit()? && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }

    pub(super) fn cleanup(&mut self) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + CLEANUP_GRACE;
        self.signal_group()?;
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "cleanup deadline exceeded",
                ));
            }
            if self.observe_exit()? && group_stopped(self.id(), deadline)? {
                let status = self
                    .child
                    .try_wait()?
                    .ok_or_else(|| io::Error::other("exited child was not reapable"))?;
                self.reaped = true;
                return Ok(status);
            }
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            // Explicit cleanup reports errors; unwinding cannot block forever
            // on a kernel-stuck child. Only attempt a signal and nonblocking reap.
            let _ = self.signal_group();
            let _ = self.child.try_wait();
        }
    }
}

fn charge_scan(entries: &mut usize, deadline: Instant) -> io::Result<()> {
    *entries += 1;
    if *entries > MAX_SCAN_ENTRIES || Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "cleanup scan budget exceeded",
        ));
    }
    Ok(())
}

fn group_state(path: &Path, group: u32) -> io::Result<Option<u8>> {
    match fs::read(path) {
        Ok(stat) => parse_group_state(&stat, group),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn parse_group_state(stat: &[u8], group: u32) -> io::Result<Option<u8>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid /proc task stat");
    // comm can contain arbitrary bytes and parentheses; only its suffix is ASCII.
    let end = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or_else(invalid)?;
    let fields = std::str::from_utf8(&stat[end + 1..]).map_err(|_| invalid())?;
    let mut fields = fields.split_whitespace();
    let state = fields
        .next()
        .and_then(|s| s.as_bytes().first())
        .copied()
        .ok_or_else(invalid)?;
    // Retiring Linux tasks can report PGID -1 while losing their sighand.
    let pgid = fields
        .nth(1)
        .ok_or_else(invalid)?
        .parse::<libc::pid_t>()
        .map_err(|_| invalid())?;
    Ok((u32::try_from(pgid).ok() == Some(group)).then_some(state))
}

fn group_stopped(group: u32, deadline: Instant) -> io::Result<bool> {
    let mut scanned = 0;
    for entry in fs::read_dir("/proc")? {
        charge_scan(&mut scanned, deadline)?;
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        match group_state(&entry.path().join("stat"), group)? {
            None => continue,
            Some(b'Z' | b'X') => {}
            Some(_) => return Ok(false),
        }
        // A zombie thread-group leader can still have running sibling threads.
        let tasks = match fs::read_dir(entry.path().join("task")) {
            Ok(tasks) => tasks,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for task in tasks {
            charge_scan(&mut scanned, deadline)?;
            if !matches!(
                group_state(&task?.path().join("stat"), group)?,
                None | Some(b'Z' | b'X')
            ) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::parse_group_state;

    #[test]
    fn proc_stat_accepts_retiring_tasks_and_non_utf8_names() {
        assert_eq!(
            parse_group_state(b"123 (retiring) R 0 -1 -1 0 -1", 321).unwrap(),
            None
        );
        assert_eq!(
            parse_group_state(b"123 (own\xff)ed) Z 1 321 321 0 -1", 321).unwrap(),
            Some(b'Z')
        );
        assert_eq!(
            parse_group_state(b"123 (other) R 1 456 456 0 -1", 321).unwrap(),
            None
        );
        assert!(parse_group_state(b"invalid", 321).is_err());
    }
}
