use crate::utils::procfs::proc_pid_entry;
use std::{
    collections::HashMap,
    fs, io,
    time::{Duration, Instant},
};

const MAX_RETRIES: usize = 64;
const MAX_RETRIES_PER_TICK: usize = 4;
const INITIAL_BACKOFF: Duration = Duration::from_millis(10);
const MAX_BACKOFF: Duration = Duration::from_secs(1);
const RETRY_TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessIdentity {
    pub(super) pid: u32,
    pub(super) start_time: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdentityStatus {
    Current,
    Stale,
    Unavailable,
}

impl ProcessIdentity {
    pub(super) fn read(pid: i32) -> io::Result<Self> {
        if pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid process id",
            ));
        }
        let path = proc_pid_entry(pid, "stat");
        let stat = fs::read(path)?;
        let start_time = parse_start_time(&stat)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?;
        Ok(Self {
            pid: pid as u32,
            start_time,
        })
    }

    pub(super) fn status(self) -> IdentityStatus {
        match Self::read(self.pid as i32) {
            Ok(current) if current == self => IdentityStatus::Current,
            Ok(_) => IdentityStatus::Stale,
            Err(error) if error.kind() == io::ErrorKind::NotFound => IdentityStatus::Stale,
            Err(_) => IdentityStatus::Unavailable,
        }
    }
}

fn parse_start_time(stat: &[u8]) -> Option<u64> {
    let stat = String::from_utf8_lossy(stat);
    let open = stat.find('(')?;
    let close = stat.rfind(')').filter(|close| *close > open)?;
    stat[close + 1..].split_whitespace().nth(19)?.parse().ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RetryRequest {
    pub(super) identity: ProcessIdentity,
    pub(super) admit: bool,
}

struct Retry {
    request: RetryRequest,
    deadline: Instant,
    expires_at: Instant,
    backoff: Duration,
}

#[derive(Default)]
pub(super) struct RetryBatch {
    pub(super) due: Vec<RetryRequest>,
    pub(super) expired: Vec<RetryRequest>,
}

#[derive(Default)]
pub(super) struct LateRetries {
    entries: HashMap<u32, Retry>,
}

impl LateRetries {
    /// Adds an immediately due retry, or returns false when the queue is full.
    pub(super) fn schedule(
        &mut self,
        identity: ProcessIdentity,
        admit: bool,
        now: Instant,
    ) -> bool {
        if let Some(existing) = self.entries.get_mut(&identity.pid) {
            if existing.request.identity == identity {
                existing.request.admit |= admit;
                return true;
            }
            self.entries.remove(&identity.pid);
        } else if self.entries.len() >= MAX_RETRIES {
            return false;
        }

        self.entries.insert(
            identity.pid,
            Retry {
                request: RetryRequest { identity, admit },
                deadline: now,
                expires_at: now + RETRY_TTL,
                backoff: INITIAL_BACKOFF,
            },
        );
        true
    }

    pub(super) fn poll(&mut self, now: Instant) -> RetryBatch {
        let expired_pids: Vec<u32> = self
            .entries
            .iter()
            .filter(|(_, retry)| now >= retry.expires_at)
            .map(|(pid, _)| *pid)
            .collect();
        let expired = expired_pids
            .into_iter()
            .filter_map(|pid| self.entries.remove(&pid).map(|retry| retry.request))
            .collect();

        let mut due: Vec<(Instant, RetryRequest)> = self
            .entries
            .values()
            .filter(|retry| retry.deadline <= now)
            .map(|retry| (retry.deadline, retry.request))
            .collect();
        due.sort_by_key(|(deadline, request)| (*deadline, request.identity.pid));
        RetryBatch {
            due: due
                .into_iter()
                .take(MAX_RETRIES_PER_TICK)
                .map(|(_, request)| request)
                .collect(),
            expired,
        }
    }

    /// Returns false when the retry expired before it could be rescheduled.
    pub(super) fn reschedule(&mut self, identity: ProcessIdentity, now: Instant) -> bool {
        let Some(retry) = self.entries.get_mut(&identity.pid) else {
            return false;
        };
        if retry.request.identity != identity {
            return false;
        }
        if now >= retry.expires_at {
            self.entries.remove(&identity.pid);
            return false;
        }
        retry.deadline = now + retry.backoff;
        retry.backoff = (retry.backoff * 2).min(MAX_BACKOFF);
        true
    }

    pub(super) fn identity(&self, pid: u32) -> Option<ProcessIdentity> {
        self.entries.get(&pid).map(|retry| retry.request.identity)
    }

    pub(super) fn remove(&mut self, pid: u32) {
        self.entries.remove(&pid);
    }

    pub(super) fn remove_if(&mut self, identity: ProcessIdentity) {
        if self.identity(identity.pid) == Some(identity) {
            self.remove(identity.pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_time: 1000 + u64::from(pid),
        }
    }

    #[test]
    fn start_time_follows_the_last_parenthesis() {
        let fields = (0..20)
            .map(|index| if index == 19 { "98765" } else { "1" })
            .collect::<Vec<_>>()
            .join(" ");
        let stat = format!("4242 (agent) odd) name) {fields}\n");
        assert_eq!(parse_start_time(stat.as_bytes()), Some(98765));
        assert_eq!(parse_start_time(b"42 (missing fields) S 1 2"), None);
        assert_eq!(parse_start_time(b"not a stat"), None);
    }

    #[test]
    fn invalid_process_identity_is_unavailable() {
        assert_eq!(identity(0).status(), IdentityStatus::Unavailable);
    }

    #[test]
    fn retries_are_prompt_bounded_and_ordered() {
        let now = Instant::now();
        let mut retries = LateRetries::default();
        for pid in (1..=5).rev() {
            retries.schedule(identity(pid), false, now);
        }
        let batch = retries.poll(now);
        let due: Vec<u32> = batch
            .due
            .iter()
            .map(|request| request.identity.pid)
            .collect();
        assert_eq!(due, vec![1, 2, 3, 4]);
        assert_eq!(
            retries.poll(now).due.len(),
            4,
            "due entries remain until completed"
        );
    }

    #[test]
    fn failed_attempts_back_off_to_one_second_and_expire() {
        let now = Instant::now();
        let id = identity(7);
        let mut retries = LateRetries::default();
        retries.schedule(id, true, now);
        assert!(retries.reschedule(id, now));
        assert!(retries.poll(now + Duration::from_millis(9)).due.is_empty());
        assert_eq!(retries.poll(now + INITIAL_BACKOFF).due.len(), 1);
        for attempt in 0..20 {
            assert!(retries.reschedule(id, now + Duration::from_secs(attempt)));
        }
        assert_eq!(retries.entries[&id.pid].backoff, MAX_BACKOFF);
        let batch = retries.poll(now + RETRY_TTL);
        assert!(batch.due.is_empty());
        assert_eq!(batch.expired.len(), 1);
        assert_eq!(batch.expired[0].identity, id);
        assert!(batch.expired[0].admit);
        assert!(retries.entries.is_empty());
    }

    #[test]
    fn capacity_rejects_new_work_and_pid_reuse_replaces_identity() {
        let now = Instant::now();
        let mut retries = LateRetries::default();
        for pid in 1..=MAX_RETRIES as u32 {
            let at = now + Duration::from_millis(u64::from(pid));
            assert!(retries.schedule(identity(pid), false, at));
        }
        let new = identity(MAX_RETRIES as u32 + 1);
        assert!(!retries.schedule(new, false, now + Duration::from_secs(1)));
        assert_eq!(retries.entries.len(), MAX_RETRIES);
        assert_eq!(retries.identity(1), Some(identity(1)));
        let replacement = ProcessIdentity {
            start_time: 99999,
            ..identity(10)
        };
        assert!(retries.schedule(replacement, true, now + Duration::from_secs(2)));
        assert!(!retries.reschedule(identity(10), now + Duration::from_secs(3)));
        assert_eq!(retries.identity(10), Some(replacement));
        assert!(retries.entries[&10].request.admit);
        retries.remove_if(identity(10));
        assert_eq!(retries.identity(10), Some(replacement));
        retries.remove_if(replacement);
        assert_eq!(retries.identity(10), None);
    }
}
