//! Parent-side supervision of the privileged login worker.
//!
//! Keeps the terminal responsive while sudo waits for a password, separates
//! "authorization withdrawn, nothing written" from "a commit may already have
//! landed", and never leaves a root writer behind.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::FileTypeExt;
use std::process::Child;
use std::time::{Duration, Instant};

use super::{
    classify, Outcome, COMMIT_LIMIT, MAX_STREAM_BYTES, POLL_INTERVAL, STOP_GRACE, VERSION,
};

/// The privileged child, narrowed to what supervision needs so the deadline,
/// truncation and reap guards can be exercised without spawning a process.
pub(super) trait Worker {
    fn poll(&mut self) -> Poll;
    fn request_stop(&mut self);
    fn force_stop(&mut self, worker_pid: Option<u32>);
}

pub(super) enum Poll {
    Running,
    Exited,
    Unreadable,
}

pub(super) struct ChildWorker<'a> {
    child: &'a mut Child,
}

impl<'a> ChildWorker<'a> {
    pub(super) fn new(child: &'a mut Child) -> Self {
        Self { child }
    }
}

impl Worker for ChildWorker<'_> {
    fn poll(&mut self) -> Poll {
        match self.child.try_wait() {
            Ok(Some(_)) => Poll::Exited,
            Ok(None) => Poll::Running,
            Err(_) => Poll::Unreadable,
        }
    }

    fn request_stop(&mut self) {
        unsafe {
            nix::libc::kill(self.child.id() as i32, nix::libc::SIGTERM);
        }
    }

    fn force_stop(&mut self, worker_pid: Option<u32>) {
        if let Some(pid) = worker_pid {
            // Only signal a group this worker actually leads; a recycled pid
            // must never turn into a group kill.
            if unsafe { nix::libc::getpgid(pid as i32) } == pid as i32 {
                unsafe {
                    nix::libc::kill(-(pid as i32), nix::libc::SIGKILL);
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Environment hooks the loop reads. Injecting them keeps the guards testable
/// without touching the process-wide signal slot or the real terminal.
pub(super) struct Probes {
    pub(super) terminal_lost: fn() -> bool,
    pub(super) take_signal: fn() -> Option<i32>,
    pub(super) publish_signal: fn(i32),
}

impl Probes {
    fn production() -> Self {
        Self {
            terminal_lost,
            take_signal: crate::runtime::terminal::take_guide_signal,
            publish_signal: crate::runtime::terminal::publish_guide_signal,
        }
    }
}

pub(super) fn supervise(
    child: &mut Child,
    result: &mut tempfile::NamedTempFile,
    grant: File,
    operation_id: &str,
    username: &str,
    expected_uid: u32,
    target_shell: &str,
) -> Outcome {
    supervise_with_budget(
        &mut ChildWorker::new(child),
        result,
        grant,
        operation_id,
        username,
        expected_uid,
        target_shell,
        COMMIT_LIMIT,
        STOP_GRACE,
        POLL_INTERVAL,
        &Probes::production(),
    )
}

/// Same loop with an explicit budget so the deadline and reap paths can be
/// exercised deterministically instead of waiting out the production limits.
#[allow(clippy::too_many_arguments)]
fn supervise_with_budget(
    worker: &mut dyn Worker,
    result: &mut tempfile::NamedTempFile,
    mut grant: File,
    operation_id: &str,
    username: &str,
    expected_uid: u32,
    target_shell: &str,
    commit_limit: Duration,
    stop_grace: Duration,
    poll_interval: Duration,
    probes: &Probes,
) -> Outcome {
    let mut buffer = String::new();
    let mut chunk = [0u8; 1024];
    let mut granted = false;
    let mut cancelled = false;
    let mut terminate: Option<i32> = None;
    let mut worker_pid: Option<u32> = None;
    let mut commit_started: Option<Instant> = None;
    let mut final_reply: Option<String> = None;
    let mut exited = false;
    // The result file is shared with the worker through `dup`, so both ends
    // share one file offset. Read at a private offset instead of consuming it.
    let is_fifo = result
        .as_file()
        .metadata()
        .map(|metadata| metadata.file_type().is_fifo())
        .unwrap_or(false);
    let mut offset = 0u64;
    let read_flags = unsafe { nix::libc::fcntl(result.as_raw_fd(), nix::libc::F_GETFL) };
    if read_flags >= 0 {
        unsafe {
            nix::libc::fcntl(
                result.as_raw_fd(),
                nix::libc::F_SETFL,
                read_flags | nix::libc::O_NONBLOCK,
            );
        }
    }
    loop {
        let read_attempt = if is_fifo {
            result.read(&mut chunk)
        } else {
            result.as_file().read_at(&mut chunk, offset)
        };
        match read_attempt {
            Ok(0) => {}
            Ok(read) => {
                offset += read as u64;
                buffer.push_str(&String::from_utf8_lossy(&chunk[..read]));
                if buffer.len() > MAX_STREAM_BYTES {
                    stop_worker_with_grace(worker, worker_pid, stop_grace, poll_interval);
                    republish(terminate, probes);
                    return Outcome::Unconfirmed;
                }
                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim().to_string();
                    buffer.drain(..=index);
                    if line.is_empty() {
                        continue;
                    }
                    if let Some(pid) = ready_worker(&line, operation_id) {
                        worker_pid = pid;
                        // A cancel that landed before Ready arrived must still
                        // hold: granting here would start a commit the user
                        // already withdrew.
                        if cancelled
                            || pending_exit(&mut terminate, probes)
                            || (probes.terminal_lost)()
                        {
                            cancelled = true;
                            let _ = grant.write_all(b"0");
                            let _ = grant.flush();
                        } else {
                            granted = true;
                            commit_started = Some(Instant::now());
                            let _ = grant.write_all(b"1");
                            let _ = grant.flush();
                        }
                    } else {
                        final_reply = Some(line);
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
        if final_reply.is_some() {
            break;
        }
        if !granted
            && !cancelled
            && (pending_exit(&mut terminate, probes) || (probes.terminal_lost)())
        {
            cancelled = true;
            let _ = grant.write_all(b"0");
            let _ = grant.flush();
        }
        if granted {
            // A commit in flight is never interrupted as a plain cancel; the
            // owner deadline decides, and the account state is re-read after.
            let _ = pending_exit(&mut terminate, probes);
            if commit_started.is_some_and(|started| started.elapsed() >= commit_limit) {
                stop_worker_with_grace(worker, worker_pid, stop_grace, poll_interval);
                republish(terminate, probes);
                return Outcome::Unconfirmed;
            }
        }
        match worker.poll() {
            Poll::Exited => {
                exited = true;
                break;
            }
            Poll::Running => {}
            Poll::Unreadable => break,
        }
        std::thread::sleep(poll_interval);
    }
    drop(grant);
    if !exited {
        if granted {
            // A permanent read error must not turn into an unbounded wait on a
            // commit that can no longer report its result.
            stop_worker_with_grace(worker, worker_pid, stop_grace, poll_interval);
        } else {
            worker.force_stop(worker_pid);
        }
    }
    // The worker can write its result and exit within one tick. Drain what is
    // left before deciding, otherwise a completed change reads as unconfirmed.
    loop {
        let read_attempt = if is_fifo {
            result.read(&mut chunk)
        } else {
            result.as_file().read_at(&mut chunk, offset)
        };
        let Ok(read) = read_attempt else { break };
        if read == 0 {
            break;
        }
        offset += read as u64;
        buffer.push_str(&String::from_utf8_lossy(&chunk[..read]));
        if buffer.len() > MAX_STREAM_BYTES {
            break;
        }
        while let Some(index) = buffer.find('\n') {
            let line = buffer[..index].trim().to_string();
            buffer.drain(..=index);
            if !line.is_empty() && ready_worker(&line, operation_id).is_none() {
                final_reply = Some(line);
            }
        }
    }
    if let Some(reply) = final_reply {
        republish(terminate, probes);
        return classify(Some(&reply), username, expected_uid, target_shell);
    }
    if cancelled && !granted {
        republish(terminate, probes);
        return Outcome::Cancelled;
    }
    republish(terminate, probes);
    Outcome::Unconfirmed
}

/// Records the exit request without consuming it: TERM/HUP must still end the
/// whole wizard after the privileged task converged.
fn pending_exit(terminate: &mut Option<i32>, probes: &Probes) -> bool {
    match (probes.take_signal)() {
        Some(nix::libc::SIGINT) => true,
        Some(signal) => {
            *terminate = Some(signal);
            true
        }
        None => terminate.is_some(),
    }
}

fn republish(terminate: Option<i32>, probes: &Probes) {
    if let Some(signal) = terminate {
        (probes.publish_signal)(signal);
    }
}

/// Last resort after the owner deadline: ask sudo to forward TERM, then stop
/// the worker's own process group so no root writer outlives the wizard.
fn stop_worker_with_grace(
    worker: &mut dyn Worker,
    worker_pid: Option<u32>,
    grace: Duration,
    poll_interval: Duration,
) {
    worker.request_stop();
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(worker.poll(), Poll::Exited) {
            return;
        }
        std::thread::sleep(poll_interval);
    }
    worker.force_stop(worker_pid);
}

/// A Ready record only counts when it matches this protocol version and the
/// operation the wizard actually started.
pub(in crate::runtime::login) fn ready_worker(
    line: &str,
    operation_id: &str,
) -> Option<Option<u32>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return None;
    };
    if value.get("kind").and_then(serde_json::Value::as_str) != Some("ready") {
        return None;
    }
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(VERSION))
        || value
            .get("operation_id")
            .and_then(serde_json::Value::as_str)
            != Some(operation_id)
    {
        return None;
    }
    Some(
        value
            .get("worker_pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
    )
}

/// The caller's terminal is gone: no tty, or the descriptor reports hangup.
fn terminal_lost() -> bool {
    if unsafe { nix::libc::isatty(nix::libc::STDIN_FILENO) } != 1 {
        return true;
    }
    let mut descriptor = nix::libc::pollfd {
        fd: nix::libc::STDIN_FILENO,
        events: 0,
        revents: 0,
    };
    if unsafe { nix::libc::poll(&mut descriptor, 1, 0) } < 0 {
        return false;
    }
    descriptor.revents & (nix::libc::POLLHUP | nix::libc::POLLERR | nix::libc::POLLNVAL) != 0
}

#[cfg(test)]
mod tests;
