//! Event-driven wakeups and driver lifecycle checks for the raw relay.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::Child;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;

use super::eof_shutdown::{advance_eof_shutdown, request_eof_shutdown, EofShutdown};

const RELAY_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(1);

pub(in super::super) struct RawActionWatchdog {
    grace: Duration,
}

impl RawActionWatchdog {
    pub(in super::super) fn new(grace: Duration) -> Self {
        Self { grace }
    }

    pub(super) fn expired(&self, driver_completed_at: Option<Instant>) -> bool {
        driver_completed_at.is_some_and(|done| done.elapsed() > self.grace)
    }

    fn remaining(&self, driver_completed_at: Option<Instant>) -> Option<Duration> {
        driver_completed_at.map(|done| self.grace.saturating_sub(done.elapsed()))
    }
}

pub(in super::super) struct DriverCompletion {
    pub(in super::super) result: io::Result<()>,
    pub(in super::super) completed_at: Instant,
}

pub(super) fn relay_wait_timeout(
    watchdog: Option<&RawActionWatchdog>,
    driver_completed_at: Option<Instant>,
    eof_shutdown: Option<&EofShutdown>,
    runtime_poll_pending: bool,
    foreground_command_active: bool,
    prompt_replay_remaining: Option<Duration>,
) -> Duration {
    let mut timeout = RELAY_MAINTENANCE_INTERVAL;
    if let Some(remaining) = watchdog.and_then(|watchdog| watchdog.remaining(driver_completed_at)) {
        timeout = timeout.min(remaining);
    }
    // Scripted action drivers advance on their own timers. Keep checking
    // child liveness between actions so a prompt-level Ctrl-D wins the race
    // against the next scheduled write.
    if watchdog.is_some() && driver_completed_at.is_none() {
        timeout = timeout.min(Duration::from_millis(10));
    }
    if let Some(shutdown) = eof_shutdown {
        timeout = timeout.min(shutdown.remaining());
    }
    if runtime_poll_pending {
        timeout = timeout.min(Duration::from_millis(10));
    }
    // Foreground commands can make observer-owned timeouts actionable
    // without producing PTY bytes; 10 Hz preserves them without the old
    // steady 100 Hz relay baseline.
    if foreground_command_active {
        timeout = timeout.min(Duration::from_millis(100));
    }
    if let Some(remaining) = prompt_replay_remaining {
        timeout = timeout.min(remaining);
    }
    timeout
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RelayActivity {
    pub(super) pty: bool,
    pub(super) wake: bool,
    pub(super) resize: bool,
}

pub(super) fn wait_for_relay_activity(
    master_fd: RawFd,
    wake: &mut UnixStream,
    resize: &mut UnixStream,
    timeout: Duration,
) -> io::Result<RelayActivity> {
    let wake_fd = wake.as_raw_fd();
    let resize_fd = resize.as_raw_fd();
    let mut poll_fds = [
        libc::pollfd {
            fd: master_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
        libc::pollfd {
            fd: resize_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
    ];
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = if remaining.is_zero() {
            0
        } else {
            remaining
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32
        };
        let result = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, timeout_ms) };
        if result >= 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }

    let pty = poll_fds[0].revents != 0;
    let resize_ready = poll_fds[1].revents != 0;
    let wake_ready = poll_fds[2].revents != 0;
    if wake_ready {
        drain_wake_stream(wake)?;
    }
    if resize_ready {
        drain_wake_stream(resize)?;
    }
    Ok(RelayActivity {
        pty,
        wake: wake_ready,
        resize: resize_ready,
    })
}

fn drain_wake_stream(wake: &mut UnixStream) -> io::Result<()> {
    let mut buffer = [0_u8; 256];
    loop {
        match wake.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn poll_driver_completion(
    receiver: &Receiver<DriverCompletion>,
    master: &File,
    terminal: &File,
    child: &mut Child,
    completed_at: &mut Option<Instant>,
) -> io::Result<()> {
    let Ok(completion) = receiver.try_recv() else {
        return Ok(());
    };
    *completed_at = Some(completion.completed_at);
    if let Err(error) = completion.result {
        shutdown_and_reap_child(master, terminal, child);
        return Err(error);
    }
    Ok(())
}

fn shutdown_and_reap_child(master: &File, terminal: &File, child: &mut Child) {
    let mut shutdown = None;
    if request_eof_shutdown(master, terminal, child, &mut shutdown).is_ok() {
        while let Ok(false) = advance_eof_shutdown(&mut shutdown) {
            thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_relay_uses_low_frequency_maintenance() {
        assert_eq!(
            relay_wait_timeout(None, None, None, false, false, None),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn foreground_command_uses_ten_hertz_timeout_checks() {
        assert_eq!(
            relay_wait_timeout(None, None, None, false, true, None),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn active_runtime_keeps_streaming_poll_cadence() {
        assert_eq!(
            relay_wait_timeout(None, None, None, true, false, None),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn scripted_driver_checks_child_liveness_between_actions() {
        let watchdog = RawActionWatchdog::new(Duration::from_secs(1));
        assert_eq!(
            relay_wait_timeout(Some(&watchdog), None, None, false, false, None),
            Duration::from_millis(10)
        );
    }
}
