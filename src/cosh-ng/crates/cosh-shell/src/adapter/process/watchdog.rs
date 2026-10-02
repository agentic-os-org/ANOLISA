use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AgentProcessTimeouts {
    start_timeout: Duration,
    idle_timeout: Duration,
    approval_wait_timeout: Duration,
    hard_timeout: Duration,
    pub(super) cancel_grace: Duration,
    pub(super) stderr_tail_bytes: usize,
}

impl AgentProcessTimeouts {
    pub(super) fn from_env() -> Self {
        Self {
            start_timeout: Duration::from_secs(env_u64("COSH_AGENT_START_TIMEOUT_SECS", 20)),
            idle_timeout: Duration::from_secs(env_u64("COSH_AGENT_IDLE_TIMEOUT_SECS", 90)),
            approval_wait_timeout: Duration::from_secs(env_u64(
                "COSH_AGENT_APPROVAL_WAIT_TIMEOUT_SECS",
                600,
            )),
            hard_timeout: Duration::from_secs(env_u64("COSH_AGENT_HARD_TIMEOUT_SECS", 600)),
            cancel_grace: Duration::from_millis(env_u64("COSH_AGENT_CANCEL_GRACE_MS", 2000)),
            stderr_tail_bytes: env_usize("COSH_AGENT_STDERR_TAIL_BYTES", 4096),
        }
    }
}

impl Default for AgentProcessTimeouts {
    fn default() -> Self {
        Self::from_env()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentProcessTimeoutKind {
    Start,
    Idle,
    ApprovalWait,
    Hard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AgentProcessTimeout {
    pub(super) kind: AgentProcessTimeoutKind,
    pub(super) elapsed: Duration,
    pub(super) last_activity_age: Duration,
    pub(super) limit: Duration,
}

#[derive(Debug)]
pub(super) struct AgentProcessWatchdog {
    started_at: Instant,
    last_stdout_at: Option<Instant>,
    approval_wait_started_at: Option<Instant>,
    timeouts: AgentProcessTimeouts,
}

impl AgentProcessWatchdog {
    pub(super) fn new(timeouts: AgentProcessTimeouts, now: Instant) -> Self {
        Self {
            started_at: now,
            last_stdout_at: None,
            approval_wait_started_at: None,
            timeouts,
        }
    }

    pub(super) fn record_stdout(&mut self, now: Instant) {
        self.last_stdout_at = Some(now);
        self.approval_wait_started_at = None;
    }

    pub(super) fn record_approval_wait(&mut self, now: Instant) {
        self.last_stdout_at = Some(now);
        self.approval_wait_started_at = Some(now);
    }

    pub(super) fn timeout(&self, now: Instant) -> Option<AgentProcessTimeout> {
        let elapsed = now.saturating_duration_since(self.started_at);
        if elapsed >= self.timeouts.hard_timeout {
            return Some(AgentProcessTimeout {
                kind: AgentProcessTimeoutKind::Hard,
                elapsed,
                last_activity_age: self.last_activity_age(now),
                limit: self.timeouts.hard_timeout,
            });
        }

        if let Some(approval_wait_started_at) = self.approval_wait_started_at {
            let age = now.saturating_duration_since(approval_wait_started_at);
            return (age >= self.timeouts.approval_wait_timeout).then_some(AgentProcessTimeout {
                kind: AgentProcessTimeoutKind::ApprovalWait,
                elapsed,
                last_activity_age: age,
                limit: self.timeouts.approval_wait_timeout,
            });
        }

        match self.last_stdout_at {
            Some(last_stdout_at) => {
                let age = now.saturating_duration_since(last_stdout_at);
                (age >= self.timeouts.idle_timeout).then_some(AgentProcessTimeout {
                    kind: AgentProcessTimeoutKind::Idle,
                    elapsed,
                    last_activity_age: age,
                    limit: self.timeouts.idle_timeout,
                })
            }
            None => (elapsed >= self.timeouts.start_timeout).then_some(AgentProcessTimeout {
                kind: AgentProcessTimeoutKind::Start,
                elapsed,
                last_activity_age: elapsed,
                limit: self.timeouts.start_timeout,
            }),
        }
    }

    fn last_activity_age(&self, now: Instant) -> Duration {
        self.last_stdout_at
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or_else(|| now.saturating_duration_since(self.started_at))
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_timeouts() -> AgentProcessTimeouts {
        AgentProcessTimeouts {
            start_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(3),
            approval_wait_timeout: Duration::from_secs(5),
            hard_timeout: Duration::from_secs(10),
            cancel_grace: Duration::from_millis(200),
            stderr_tail_bytes: 8,
        }
    }

    #[test]
    fn watchdog_times_out_before_first_stdout() {
        let started = Instant::now();
        let watchdog = AgentProcessWatchdog::new(test_timeouts(), started);
        let timeout = watchdog
            .timeout(started + Duration::from_secs(2))
            .expect("start timeout");
        assert_eq!(timeout.kind, AgentProcessTimeoutKind::Start);
        assert_eq!(timeout.limit, Duration::from_secs(2));
    }

    #[test]
    fn watchdog_resets_idle_after_stdout() {
        let started = Instant::now();
        let mut watchdog = AgentProcessWatchdog::new(test_timeouts(), started);
        watchdog.record_stdout(started + Duration::from_secs(1));
        assert!(watchdog.timeout(started + Duration::from_secs(3)).is_none());
        let timeout = watchdog
            .timeout(started + Duration::from_secs(4))
            .expect("idle timeout");
        assert_eq!(timeout.kind, AgentProcessTimeoutKind::Idle);
    }

    #[test]
    fn watchdog_uses_approval_wait_timeout_instead_of_idle_timeout() {
        let started = Instant::now();
        let mut watchdog = AgentProcessWatchdog::new(test_timeouts(), started);
        watchdog.record_approval_wait(started + Duration::from_secs(1));
        assert!(watchdog.timeout(started + Duration::from_secs(4)).is_none());
        let timeout = watchdog
            .timeout(started + Duration::from_secs(6))
            .expect("approval wait timeout");
        assert_eq!(timeout.kind, AgentProcessTimeoutKind::ApprovalWait);
        assert_eq!(timeout.limit, Duration::from_secs(5));
    }

    #[test]
    fn watchdog_hard_timeout_wins() {
        let started = Instant::now();
        let mut watchdog = AgentProcessWatchdog::new(test_timeouts(), started);
        watchdog.record_stdout(started + Duration::from_secs(9));
        let timeout = watchdog
            .timeout(started + Duration::from_secs(10))
            .expect("hard timeout");
        assert_eq!(timeout.kind, AgentProcessTimeoutKind::Hard);
    }
}
