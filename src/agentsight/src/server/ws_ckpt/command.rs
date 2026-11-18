//! Bounded execution of the local ws-ckpt CLI.

use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{ErrorKind, Read, Write},
    os::fd::{AsRawFd, OwnedFd},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
// CLI input contract: src/ws-ckpt/docs/design/guarded-checkpoint-v2.md.
const MAX_CONFIRMATION_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub(super) enum BeforeExecution {
    #[error("ws-ckpt confirmation exceeds the {MAX_CONFIRMATION_BYTES}-byte input limit")]
    ConfirmationTooLarge,
    #[error("could not start ws-ckpt: {0}")]
    SpawnFailed(#[source] std::io::Error),
}

pub(super) fn run(
    binary: &Path,
    socket: &str,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<Output> {
    // A confirmation fits in one Linux pipe buffer even if the CLI exits or stalls before reading.
    ensure!(
        input.is_none_or(|bytes| bytes.len() <= MAX_CONFIRMATION_BYTES),
        BeforeExecution::ConfirmationTooLarge
    );
    let mut child = Command::new(binary)
        .args(args)
        .env("WS_CKPT_SOCKET", socket)
        .env("WS_CKPT_REQUIRE_TRUSTED_PEER", "1")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(BeforeExecution::SpawnFailed)?;
    let group = child.id() as libc::pid_t;
    let deadline = Instant::now() + TIMEOUT;
    let output = (|| {
        if let Some(input) = input {
            let write = child
                .stdin
                .take()
                .context("missing ws-ckpt stdin")?
                .write_all(input);
            // An early CLI rejection may close stdin before writing its response.
            if let Err(error) = write
                && error.kind() != ErrorKind::BrokenPipe
            {
                return Err(error.into());
            }
        }
        let stdout: OwnedFd = child
            .stdout
            .take()
            .context("missing ws-ckpt stdout")?
            .into();
        let stderr: OwnedFd = child
            .stderr
            .take()
            .context("missing ws-ckpt stderr")?
            .into();
        let [stdout, stderr] = read_pipes([File::from(stdout), File::from(stderr)], deadline)?;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "ws-ckpt command timed out");
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    })();
    if output.is_err() {
        // The child has not been reaped on this path and still owns its process group.
        unsafe { libc::kill(-group, libc::SIGKILL) };
        let _ = child.wait();
    }
    output
}

fn read_pipes(mut pipes: [File; 2], deadline: Instant) -> Result<[Vec<u8>; 2]> {
    let mut poll_fds = pipes.each_ref().map(|pipe| libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    let mut output = [Vec::new(), Vec::new()];
    let mut chunk = [0; 8192];
    // Alternate across ready pipes so neither can block the child behind a full buffer.
    while poll_fds.iter().any(|fd| fd.fd >= 0) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(!remaining.is_zero(), "ws-ckpt command timed out");
        // Both descriptors remain owned by pipes for the duration of poll.
        let ready = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                2,
                remaining.as_millis() as libc::c_int,
            )
        };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != ErrorKind::Interrupted {
                return Err(error.into());
            }
            continue;
        }
        for (index, fd) in poll_fds.iter_mut().enumerate() {
            if fd.revents == 0 {
                continue;
            }
            let count = pipes[index].read(&mut chunk)?;
            if count == 0 {
                fd.fd = -1;
            } else {
                ensure!(
                    output[index].len() + count <= MAX_OUTPUT_BYTES,
                    "ws-ckpt command output exceeded limit"
                );
                output[index].extend_from_slice(&chunk[..count]);
            }
        }
    }
    Ok(output)
}
