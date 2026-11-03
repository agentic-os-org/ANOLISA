//! Bounded startup owns its child until readiness is verified or cleanup finishes.

use super::{arguments, connect, files, ServicePaths};
use crate::{rejected, Client, Error, Result};
use std::{
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| rejected("service_startup_deadline"))
}

pub(super) fn wait(child: &mut Child, deadline: Instant) -> Result<()> {
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(5).min(remaining(deadline)?));
    }
}

pub(super) fn start(
    executable: &Path,
    paths: &ServicePaths,
    snapshot: &Path,
    deadline: Instant,
) -> Result<(Client, u32, Child)> {
    remaining(deadline)?;
    let mut command = Command::new(executable);
    command
        .args(arguments(paths, snapshot))
        .current_dir(&paths.state_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(files::log(&paths.state_dir)?);
    // Only the async-signal-safe setsid call runs between fork and exec. Our files
    // retain CLOEXEC; redirected standard descriptors cannot keep a terminal open.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Starting(Some(command.spawn()?));
    let owned = child
        .0
        .as_mut()
        .ok_or_else(|| rejected("service_child_missing"))?;
    let pid = owned.id();
    let result = ready(owned, paths, deadline);
    match result {
        Ok(client) => {
            let process = child
                .0
                .take()
                .ok_or_else(|| rejected("service_child_missing"))?;
            Ok((client, pid, process))
        }
        Err(error) => {
            if let Some(process) = child.0.as_mut() {
                terminate(process)?;
            }
            child.0.take();
            Err(error)
        }
    }
}

fn ready(child: &mut Child, paths: &ServicePaths, deadline: Instant) -> Result<Client> {
    loop {
        let budget = remaining(deadline)?;
        if child.try_wait()?.is_some() {
            return Err(rejected("service_startup_failed"));
        }
        let probe = deadline.min(Instant::now() + Duration::from_millis(200));
        match connect(paths, probe) {
            Ok((client, pid)) => {
                if pid != child.id() {
                    return Err(rejected("service_startup_pid_mismatch"));
                }
                remaining(deadline)?;
                if child.try_wait()?.is_some() {
                    return Err(rejected("service_startup_failed"));
                }
                return Ok(client);
            }
            Err(Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                ) => {}
            Err(error) => return Err(error),
        }
        thread::sleep(Duration::from_millis(5).min(budget));
    }
}

fn terminate(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    // An unreaped Child retains this exact PID; no pattern or unrelated group is
    // signalled. Graceful shutdown lets the service release its own socket.
    if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(5));
    }
    if let Err(error) = child.kill() {
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    child.wait()?;
    Ok(())
}

struct Starting(Option<Child>);
impl Drop for Starting {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = terminate(child);
        }
    }
}
