//! Contains profile-probe descendants that escape their original process group.
//!
//! This scope runs only inside the dedicated profile-probe supervisor process,
//! so every adopted child is structurally owned by the disposable probe.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command;
use std::time::Duration;

use nix::pty::Winsize;

use super::{BootstrapPathProbeError, BootstrapPathProbeIo};

const HELPER_ARG: &str = "--cosh-internal-profile-probe";
const HELPER_ENV: &str = "COSH_SHELL_INTERNAL_PROFILE_PROBE";
const HELPER_ARG0_ENV: &str = "COSH_SHELL_PROFILE_PROBE_ARG0";
const HELPER_ERROR_PREFIX: &str = "__COSH_PROFILE_PROBE_HELPER_ERROR__";
const HELPER_FAILURE: i32 = 125;
#[cfg(not(test))]
pub(super) const SUPERVISOR_GRACE: Duration = Duration::from_millis(1_250);

#[cfg(test)]
pub(super) fn run_supervised_profile_probe(
    mut command: Command,
    argv0: Option<&OsStr>,
    timeout: Duration,
    winsize: &Winsize,
) -> Result<String, BootstrapPathProbeError> {
    // Binary unit tests run under libtest rather than the normal main entry.
    // Integration tests exercise the real supervisor process boundary.
    if let Some(argv0) = argv0 {
        command.arg0(argv0);
    }
    super::run_bootstrap_path_probe_direct(
        command,
        timeout,
        BootstrapPathProbeIo::Pty,
        winsize,
        false,
    )
}

#[cfg(not(test))]
pub(super) fn run_supervised_profile_probe(
    command: Command,
    argv0: Option<&OsStr>,
    timeout: Duration,
    winsize: &Winsize,
) -> Result<String, BootstrapPathProbeError> {
    let helper = supervisor_command(&command, argv0, timeout, winsize)
        .map_err(BootstrapPathProbeError::Supervisor)?;
    let outer_timeout = timeout + SUPERVISOR_GRACE;
    let output = super::execute_bootstrap_path_probe(
        helper,
        outer_timeout,
        BootstrapPathProbeIo::Pipes,
        winsize,
        false,
    )
    .map_err(|error| match error {
        BootstrapPathProbeError::TimedOut(_) => BootstrapPathProbeError::TimedOut(timeout),
        other => other,
    })?;
    let stdout = output
        .streams
        .first()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let stderr = output.streams.get(1).map(Vec::as_slice).unwrap_or_default();
    if !output.status.success() {
        if let Some(error) = decode_helper_error(stderr, timeout) {
            return Err(error);
        }
        return Err(BootstrapPathProbeError::Failed(output.status));
    }
    let text = String::from_utf8_lossy(stdout);
    super::extract_bootstrap_path(&text).ok_or(BootstrapPathProbeError::MissingMarker)
}

#[cfg(not(test))]
pub(super) fn supervisor_command(
    command: &Command,
    argv0: Option<&OsStr>,
    timeout: Duration,
    winsize: &Winsize,
) -> io::Result<Command> {
    let mut helper = Command::new(std::env::current_exe()?);
    helper
        .arg(HELPER_ARG)
        .arg(timeout.as_millis().to_string())
        .arg(winsize.ws_row.to_string())
        .arg(winsize.ws_col.to_string())
        .arg(winsize.ws_xpixel.to_string())
        .arg(winsize.ws_ypixel.to_string())
        .arg("--")
        .arg(command.get_program())
        .args(command.get_args());
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => {
                helper.env(name, value);
            }
            None => {
                helper.env_remove(name);
            }
        }
    }
    if let Some(current_dir) = command.get_current_dir() {
        helper.current_dir(current_dir);
    }
    match argv0 {
        Some(argv0) => {
            helper.env(HELPER_ARG0_ENV, argv0);
        }
        None => {
            helper.env_remove(HELPER_ARG0_ENV);
        }
    }
    helper.env(HELPER_ENV, "1");
    Ok(helper)
}

pub(super) fn cancel_pending(
    descendants: Option<ProfileProbeDescendants>,
) -> Result<(), BootstrapPathProbeError> {
    if let Some(descendants) = descendants {
        descendants
            .cancel()
            .map_err(BootstrapPathProbeError::Containment)?;
    }
    Ok(())
}

pub(super) fn finish_pending<T>(
    descendants: Option<ProfileProbeDescendants>,
    result: Result<T, BootstrapPathProbeError>,
) -> Result<T, BootstrapPathProbeError> {
    if let Some(descendants) = descendants {
        descendants
            .finish()
            .map_err(BootstrapPathProbeError::Containment)?;
    }
    result
}

struct HelperSpec {
    command: Command,
    timeout: Duration,
    winsize: Winsize,
}

pub(super) fn run_helper_if_requested() -> Option<i32> {
    if std::env::var(HELPER_ENV).as_deref() != Ok("1") {
        return None;
    }
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.get(1).and_then(|arg| arg.to_str()) != Some(HELPER_ARG) {
        return None;
    }
    Some(match parse_helper_spec(&args) {
        Ok(spec) => run_helper(spec),
        Err(error) => {
            write_helper_error(&BootstrapPathProbeError::Supervisor(error));
            HELPER_FAILURE
        }
    })
}

fn parse_helper_spec(args: &[OsString]) -> io::Result<HelperSpec> {
    if args.len() < 9 || args.get(7).and_then(|arg| arg.to_str()) != Some("--") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid profile probe helper arguments",
        ));
    }
    let timeout_ms = parse_helper_number(args, 2, "timeout")?;
    let ws_row = parse_helper_number(args, 3, "terminal rows")?;
    let ws_col = parse_helper_number(args, 4, "terminal columns")?;
    let ws_xpixel = parse_helper_number(args, 5, "terminal pixel width")?;
    let ws_ypixel = parse_helper_number(args, 6, "terminal pixel height")?;
    let program = args.get(8).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile probe helper is missing its shell program",
        )
    })?;
    let mut command = Command::new(program);
    if let Some(argv0) = std::env::var_os(HELPER_ARG0_ENV) {
        command.arg0(argv0);
    }
    command
        .args(&args[9..])
        .env_remove(HELPER_ENV)
        .env_remove(HELPER_ARG0_ENV);
    Ok(HelperSpec {
        command,
        timeout: Duration::from_millis(timeout_ms),
        winsize: Winsize {
            ws_row: bounded_u16(ws_row, "terminal rows")?,
            ws_col: bounded_u16(ws_col, "terminal columns")?,
            ws_xpixel: bounded_u16(ws_xpixel, "terminal pixel width")?,
            ws_ypixel: bounded_u16(ws_ypixel, "terminal pixel height")?,
        },
    })
}

fn parse_helper_number(args: &[OsString], index: usize, label: &str) -> io::Result<u64> {
    let value = args
        .get(index)
        .and_then(|arg| arg.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("profile probe helper is missing {label}"),
            )
        })?;
    value.parse::<u64>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid profile probe helper {label} {value:?}: {error}"),
        )
    })
}

fn bounded_u16(value: u64, label: &str) -> io::Result<u16> {
    u16::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("profile probe helper {label} exceeds u16"),
        )
    })
}

fn run_helper(spec: HelperSpec) -> i32 {
    match super::run_bootstrap_path_probe_direct(
        spec.command,
        spec.timeout,
        BootstrapPathProbeIo::Pty,
        &spec.winsize,
        true,
    ) {
        Ok(path) => {
            let result = writeln!(io::stdout(), "\n__COSH_PATH_BEGIN__{path}__COSH_PATH_END__");
            if let Err(error) = result {
                write_helper_error(&BootstrapPathProbeError::Supervisor(error));
                HELPER_FAILURE
            } else {
                0
            }
        }
        Err(BootstrapPathProbeError::Failed(status)) => status
            .code()
            .or_else(|| status.signal().map(|signal| 128 + signal))
            .unwrap_or(1)
            .clamp(1, 255),
        Err(error) => {
            write_helper_error(&error);
            HELPER_FAILURE
        }
    }
}

fn write_helper_error(error: &BootstrapPathProbeError) {
    let tag = match error {
        BootstrapPathProbeError::Containment(_) => "containment",
        BootstrapPathProbeError::Supervisor(_) => "supervisor",
        BootstrapPathProbeError::Spawn(_) => "spawn",
        BootstrapPathProbeError::Wait(_) => "wait",
        BootstrapPathProbeError::Read(_) => "read",
        BootstrapPathProbeError::Failed(_) => "failed",
        BootstrapPathProbeError::TimedOut(_) => "timed-out",
        BootstrapPathProbeError::OutputDidNotClose(_) => "output-did-not-close",
        BootstrapPathProbeError::OutputTooLarge => "output-too-large",
        BootstrapPathProbeError::MissingMarker => "missing-marker",
    };
    eprintln!("{HELPER_ERROR_PREFIX}{tag}\n{error}");
}

#[cfg(not(test))]
pub(super) fn decode_helper_error(
    stderr: &[u8],
    timeout: Duration,
) -> Option<BootstrapPathProbeError> {
    let text = String::from_utf8_lossy(stderr);
    let payload = text.strip_prefix(HELPER_ERROR_PREFIX)?;
    let (tag, message) = payload.split_once('\n').unwrap_or((payload, payload));
    let error = || io::Error::other(message.trim().to_string());
    Some(match tag.trim() {
        "containment" => BootstrapPathProbeError::Containment(error()),
        "spawn" => BootstrapPathProbeError::Spawn(error()),
        "wait" => BootstrapPathProbeError::Wait(error()),
        "read" => BootstrapPathProbeError::Read(error()),
        "timed-out" => BootstrapPathProbeError::TimedOut(timeout),
        "output-did-not-close" => BootstrapPathProbeError::OutputDidNotClose(timeout),
        "output-too-large" => BootstrapPathProbeError::OutputTooLarge,
        "missing-marker" => BootstrapPathProbeError::MissingMarker,
        _ => BootstrapPathProbeError::Supervisor(error()),
    })
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::io;
    use std::time::{Duration, Instant};

    use nix::libc;

    const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
    const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(10);
    const CLEANUP_QUIET_PERIOD: Duration = Duration::from_millis(100);

    pub(in crate::runtime::startup) struct ProfileProbeDescendants {
        previous_subreaper: libc::c_int,
        armed: bool,
    }

    impl ProfileProbeDescendants {
        pub(in crate::runtime::startup) fn enter() -> io::Result<Self> {
            let mut previous_subreaper = 0;
            if unsafe {
                libc::prctl(
                    libc::PR_GET_CHILD_SUBREAPER,
                    &mut previous_subreaper as *mut libc::c_int,
                    0,
                    0,
                    0,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            if previous_subreaper == 0
                && unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            match direct_children() {
                Ok(children) if children.is_empty() => {}
                Ok(children) => {
                    let error = io::Error::other(format!(
                        "profile probe supervisor already owns children: {children:?}"
                    ));
                    if previous_subreaper == 0
                        && unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0) } < 0
                    {
                        let restore_error = io::Error::last_os_error();
                        return Err(io::Error::new(
                            error.kind(),
                            format!(
                                "{error}; could not restore child subreaper state: {restore_error}"
                            ),
                        ));
                    }
                    return Err(error);
                }
                Err(error) => {
                    if previous_subreaper == 0
                        && unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0) } < 0
                    {
                        let restore_error = io::Error::last_os_error();
                        return Err(io::Error::new(
                            error.kind(),
                            format!(
                                "{error}; could not restore child subreaper state: {restore_error}"
                            ),
                        ));
                    }
                    return Err(error);
                }
            }
            Ok(Self {
                previous_subreaper,
                armed: true,
            })
        }

        pub(in crate::runtime::startup) fn finish(mut self) -> io::Result<()> {
            self.cleanup()?;
            self.restore()?;
            self.armed = false;
            Ok(())
        }

        pub(in crate::runtime::startup) fn cancel(mut self) -> io::Result<()> {
            self.restore()?;
            self.armed = false;
            Ok(())
        }

        fn cleanup(&self) -> io::Result<()> {
            let deadline = Instant::now() + CLEANUP_TIMEOUT;
            let mut empty_since = None;
            loop {
                let owned = direct_children()?;
                if owned.is_empty() {
                    let now = Instant::now();
                    let quiet_since = empty_since.get_or_insert(now);
                    if now.duration_since(*quiet_since) >= CLEANUP_QUIET_PERIOD {
                        return Ok(());
                    }
                } else {
                    empty_since = None;
                    for pid in owned {
                        kill_child(pid)?;
                        reap_child(pid)?;
                    }
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "profile probe descendants did not exit within 1s",
                    ));
                }
                std::thread::sleep(CLEANUP_POLL_INTERVAL);
            }
        }

        fn restore(&mut self) -> io::Result<()> {
            if self.previous_subreaper == 0
                && unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for ProfileProbeDescendants {
        fn drop(&mut self) {
            if self.armed {
                if let Err(error) = self.cleanup() {
                    tracing::warn!(%error, "profile probe descendant cleanup failed");
                }
                if let Err(error) = self.restore() {
                    tracing::warn!(%error, "profile probe subreaper restore failed");
                }
            }
        }
    }

    fn direct_children() -> io::Result<Vec<i32>> {
        // Some virtualized Linux kernels expose an empty task/children file;
        // PPid from proc status remains authoritative there.
        let parent = std::process::id();
        let mut children = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            let status = match fs::read(entry.path().join("status")) {
                Ok(status) => status,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if parent_pid_from_status(&status) == Some(parent) {
                children.push(pid);
            }
        }
        children.sort_unstable();
        Ok(children)
    }

    fn parent_pid_from_status(status: &[u8]) -> Option<u32> {
        status
            .split(|byte| *byte == b'\n')
            .find_map(|line| line.strip_prefix(b"PPid:"))
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| value.trim().parse().ok())
    }

    fn kill_child(pid: i32) -> io::Result<()> {
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn reap_child(pid: i32) -> io::Result<()> {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            Ok(())
        } else {
            Err(error)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parent_pid_parser_ignores_non_utf8_process_names() {
            let status = b"Name:\tprobe\xffname\nState:\tS (sleeping)\nPPid:\t1234\n";
            assert_eq!(parent_pid_from_status(status), Some(1234));
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) use platform::ProfileProbeDescendants;

#[cfg(not(target_os = "linux"))]
pub(super) struct ProfileProbeDescendants;

#[cfg(not(target_os = "linux"))]
impl ProfileProbeDescendants {
    pub(super) fn enter() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "profile probe descendant containment requires Linux",
        ))
    }

    pub(super) fn finish(self) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn cancel(self) -> io::Result<()> {
        Ok(())
    }
}
