//! Deadline-bound output collection for an RTK process tree owned by a Windows job.

use std::io::{self, Read};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

use crate::RuntimeError;

const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Collect RTK output within the deadline and terminate its process tree.
pub(crate) fn run(
    mut command: Command,
    path: &Path,
    timeout: Duration,
) -> Result<(ExitStatus, String), RuntimeError> {
    let spawn_error = |source| RuntimeError::RtkSpawn {
        path: path.to_owned(),
        source,
    };
    // SAFETY: null attributes and name create a private, non-inheritable job.
    let raw_job = unsafe { CreateJobObjectW(null(), null()) };
    if raw_job.is_null() {
        return Err(spawn_error(io::Error::last_os_error()));
    }
    // SAFETY: the newly created handle is exclusively owned here.
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the buffer has the size and layout required by the information class.
    if unsafe {
        SetInformationJobObject(
            raw_job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(spawn_error(io::Error::last_os_error()));
    }
    // Suspend before assignment so no descendant can escape the job at startup.
    let child = command
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
        .spawn()
        .map_err(spawn_error)?;
    let mut owned = OwnedChild { child, job };
    // SAFETY: both handles remain live throughout the assignment.
    if unsafe { AssignProcessToJobObject(raw_job, owned.child.as_raw_handle()) } == 0 {
        let error = io::Error::last_os_error();
        let _ = owned.child.kill();
        return Err(spawn_error(error));
    }
    resume_main_thread(owned.child.id()).map_err(spawn_error)?;
    let result = collect_output(&mut owned.child, timeout);
    let cleanup = owned.cleanup();
    let (status, bytes) = result?;
    cleanup.map_err(RuntimeError::RtkWait)?;
    let stdout = String::from_utf8(bytes).map_err(|error| {
        RuntimeError::RtkOutput(io::Error::new(io::ErrorKind::InvalidData, error))
    })?;
    Ok((status, stdout))
}

fn resume_main_thread(pid: u32) -> io::Result<()> {
    // std::process::Child does not expose the initial thread on stable Rust 1.93.
    // Before its initial thread runs, the process cannot create its own threads.
    // External thread injection is outside this contract; resuming every thread
    // would change suspend counts owned by a debugger or security software.
    // SAFETY: snapshot creation has no pointer arguments.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this function owns the returned snapshot handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: entry is a valid, correctly sized output buffer.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: the owned suspended process keeps this thread alive.
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if raw_thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the newly opened handle is exclusively owned here.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw_thread) };
            // SAFETY: this is the primary thread suspended by our spawn.
            if unsafe { ResumeThread(handle.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: snapshot and output buffer remain valid.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "suspended RTK thread not found",
    ))
}

fn collect_output(
    child: &mut Child,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>), RuntimeError> {
    let mut pipe = child.stdout.take().ok_or_else(|| {
        RuntimeError::RtkOutput(io::Error::other("RTK stdout pipe was not created"))
    })?;
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut buffer = [0; 16 * 1024];
    let mut eof = false;
    loop {
        if started.elapsed() >= timeout {
            return Err(RuntimeError::RtkTimeout);
        }
        if !eof {
            let mut available = 0;
            // SAFETY: the pipe is live and available points to a valid output slot.
            let success = unsafe {
                PeekNamedPipe(
                    pipe.as_raw_handle(),
                    null_mut(),
                    0,
                    null_mut(),
                    &mut available,
                    null_mut(),
                )
            };
            if success == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    eof = true;
                } else {
                    return Err(RuntimeError::RtkOutput(error));
                }
            } else if available > 0 {
                let size = buffer.len().min(available as usize);
                let count = pipe
                    .read(&mut buffer[..size])
                    .map_err(RuntimeError::RtkOutput)?;
                bytes.extend_from_slice(&buffer[..count]);
                continue;
            }
        }
        if let Some(status) = child.try_wait().map_err(RuntimeError::RtkWait)?
            && eof
        {
            return Ok((status, bytes));
        }
        thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
    }
}

struct OwnedChild {
    child: Child,
    job: OwnedHandle,
}

impl OwnedChild {
    fn cleanup(&mut self) -> io::Result<()> {
        // SAFETY: the job belongs exclusively to this invocation, including descendants.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let started = Instant::now();
        while self.child.try_wait()?.is_none() {
            if started.elapsed() >= Duration::from_secs(1) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "RTK process cleanup timed out",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
        Ok(())
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Closing the owned job kills every remaining member even on early errors.
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn powershell(script: &str) -> Command {
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn collects_large_output_and_preserves_exit_code() {
        let (status, output) = run(
            powershell("[Console]::Write('x' * 262144); exit 3"),
            Path::new("powershell.exe"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(status.code(), Some(3));
        assert_eq!(output, "x".repeat(262144));
    }

    #[test]
    fn drains_buffered_stdout_after_writer_exits() {
        let mut child = powershell("[Console]::Write('buffered-output'); exit 3")
            .spawn()
            .unwrap();
        // Wait before collecting so the write handle is closed with bytes still buffered.
        assert_eq!(child.wait().unwrap().code(), Some(3));
        let (status, bytes) = collect_output(&mut child, Duration::from_secs(5)).unwrap();
        assert_eq!(status.code(), Some(3));
        assert_eq!(bytes, b"buffered-output");
    }

    #[test]
    fn rejects_invalid_utf8_and_missing_executable() {
        let result = run(
            powershell("[Console]::OpenStandardOutput().WriteByte(255)"),
            Path::new("powershell.exe"),
            Duration::from_secs(10),
        );
        assert!(matches!(result, Err(RuntimeError::RtkOutput(_))));
        let result = run(
            Command::new("tokenless-missing-rtk.exe"),
            Path::new("tokenless-missing-rtk.exe"),
            Duration::from_secs(1),
        );
        assert!(matches!(result, Err(RuntimeError::RtkSpawn { .. })));
    }

    #[test]
    fn timeout_kills_descendants_even_after_parent_exit() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let script = format!(
            "$p = Start-Process powershell.exe -ArgumentList '-NoProfile', '-Command', 'Start-Sleep 30' -NoNewWindow -PassThru; [IO.File]::WriteAllText('{}', [string]$p.Id); exit 0",
            pid_file.to_string_lossy().replace('\'', "''")
        );
        let started = Instant::now();
        let result = run(
            powershell(&script),
            Path::new("powershell.exe"),
            Duration::from_secs(3),
        );
        assert!(matches!(result, Err(RuntimeError::RtkTimeout)));
        assert!(started.elapsed() < Duration::from_secs(6));
        let pid: u32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        // SAFETY: query-only access to the recorded child PID; no mutation occurs.
        let handle = unsafe {
            windows_sys::Win32::System::Threading::OpenProcess(
                windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if !handle.is_null() {
            // SAFETY: own the successful query handle and wait at most one second.
            let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
            assert_eq!(
                unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(
                        handle.as_raw_handle(),
                        1000,
                    )
                },
                0
            );
        }
    }
}
