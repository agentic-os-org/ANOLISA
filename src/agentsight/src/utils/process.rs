//! Process signalling seam.
//!
//! Wraps the one place AgentSight sends OS signals (DeadLoop auto-kill) behind a
//! trait so the escalation logic can be unit-tested with a recording fake instead
//! of actually killing processes. `LibcProcessKiller` is the only place the
//! `unsafe libc::kill` call lives.

use std::io;

/// Signals AgentSight may send to a runaway agent process.
///
/// Intentionally models only the two signals the auto-kill ladder uses; not a
/// general POSIX signal set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Graceful termination (SIGTERM).
    Term,
    /// Forceful kill (SIGKILL).
    Kill,
}

impl Signal {
    fn as_raw(self) -> i32 {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// Sends a signal to a process. Injected so tests can record calls instead of
/// signalling real processes.
pub(crate) trait ProcessKiller: Send + Sync {
    fn kill(&self, pid: i32, signal: Signal) -> io::Result<()>;
}

/// Production `ProcessKiller` backed by `libc::kill`.
pub(crate) struct LibcProcessKiller;

impl ProcessKiller for LibcProcessKiller {
    fn kill(&self, pid: i32, signal: Signal) -> io::Result<()> {
        // SAFETY: libc::kill is a thin syscall wrapper; pid/signal are plain
        // ints. The only place an unsafe signal send lives in agentsight.
        let ret = unsafe { libc::kill(pid, signal.as_raw()) };
        if ret != 0 {
            // Capture errno immediately, before any allocation/logging.
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records (pid, signal) calls instead of signalling; for escalation tests.
    pub(crate) struct RecordingKiller {
        pub calls: Mutex<Vec<(i32, Signal)>>,
    }

    impl RecordingKiller {
        pub fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProcessKiller for RecordingKiller {
        fn kill(&self, pid: i32, signal: Signal) -> io::Result<()> {
            self.calls.lock().unwrap().push((pid, signal));
            Ok(())
        }
    }

    /// Always fails with ESRCH; used to exercise execute_kill_action's error arms.
    pub(crate) struct FailingKiller;

    impl ProcessKiller for FailingKiller {
        fn kill(&self, _pid: i32, _signal: Signal) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::ESRCH))
        }
    }

    #[test]
    fn signal_as_raw_maps_to_libc() {
        assert_eq!(Signal::Term.as_raw(), libc::SIGTERM);
        assert_eq!(Signal::Kill.as_raw(), libc::SIGKILL);
    }

    #[test]
    fn libc_killer_signals_real_child() {
        // Spawn a child that sleeps, SIGTERM it via the production killer, and
        // confirm it dies — exercises the unsafe path + Ok branch.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let killer = LibcProcessKiller;
        killer
            .kill(child.id() as i32, Signal::Term)
            .expect("SIGTERM should succeed on a live child");
        let status = child.wait().expect("wait child");
        assert!(!status.success(), "child terminated by signal");
    }

    #[test]
    fn libc_killer_errors_on_missing_pid() {
        // Use a pid that can never exist (i32::MAX > /proc/sys/kernel/pid_max),
        // so there is no PID-reuse race and we can assert the specific errno.
        // A reaped real pid would be racy: the kernel may recycle it between
        // wait() and kill(), making the call succeed (or hit an unrelated proc).
        let killer = LibcProcessKiller;
        let err = killer
            .kill(i32::MAX, Signal::Term)
            .expect_err("signalling a nonexistent pid must error");
        assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
    }
}
