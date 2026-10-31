//! Choose resumable maintenance work independently of CPU admission and wall-clock sampling.

use std::time::{Duration, Instant};

/// Failed or unadmitted operations leave their scheduled work due.
pub(super) struct CheckpointSchedule {
    paced: bool,
    cleanup_interval: Duration,
    last_cleanup: Option<Instant>,
    last_checkpoint: Option<Instant>,
    continuation: bool,
    checkpoint_since_cleanup: bool,
    checkpoint_wal_bytes: u64,
}

impl CheckpointSchedule {
    /// Keep the configured cleanup cadence while checkpointing paced writers more often.
    pub(super) fn new(paced: bool, cleanup_interval: Duration) -> Self {
        Self {
            paced,
            cleanup_interval,
            last_cleanup: None,
            last_checkpoint: None,
            continuation: false,
            checkpoint_since_cleanup: false,
            checkpoint_wal_bytes: 0,
        }
    }

    /// True requests a cleanup step; false requests a passive checkpoint.
    pub(super) fn next(&self, now: Instant, wal_bytes: u64) -> Option<bool> {
        let cleanup = self.continuation
            || self
                .last_cleanup
                .is_none_or(|last| now.duration_since(last) >= self.cleanup_interval);
        let interval = if self.paced {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(1)
        };
        let checkpoint_due = self
            .last_checkpoint
            .is_none_or(|last| now.duration_since(last) >= interval)
            || wal_bytes.saturating_sub(self.checkpoint_wal_bytes) >= 1024 * 1024;
        if self.paced && !cleanup && !checkpoint_due {
            return None;
        }
        if self.paced && self.continuation && checkpoint_due && !self.checkpoint_since_cleanup {
            return Some(false);
        }
        Some(cleanup)
    }

    /// Advance only after the admitted operation succeeds.
    pub(super) fn completed(
        &mut self,
        cleanup: bool,
        complete: bool,
        now: Instant,
        wal_bytes: u64,
    ) {
        if !cleanup && self.continuation {
            self.last_checkpoint = Some(now);
            self.checkpoint_wal_bytes = wal_bytes;
            self.checkpoint_since_cleanup = true;
            return;
        }
        self.continuation = cleanup && !complete;
        self.checkpoint_since_cleanup = false;
        if !cleanup || self.last_checkpoint.is_none() {
            self.last_checkpoint = Some(now);
            self.checkpoint_wal_bytes = wal_bytes;
        }
        if cleanup && complete {
            self.last_cleanup = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_growth_requests_copying_before_the_periodic_deadline() {
        let now = Instant::now();
        let mut schedule = CheckpointSchedule::new(true, Duration::from_secs(60));
        assert_eq!(schedule.next(now, 0), Some(true));
        // A deferred or failed operation has not advanced the pass.
        assert_eq!(schedule.next(now, 0), Some(true));
        schedule.completed(true, true, now, 0);
        let early = now + Duration::from_millis(1);
        assert_eq!(schedule.next(early, 1024 * 1024 - 1), None);
        assert_eq!(schedule.next(early, 1024 * 1024), Some(false));
        schedule.completed(false, true, early, 1024 * 1024);
        assert_eq!(schedule.next(early, 1024 * 1024), None);
        assert_eq!(
            schedule.next(early + Duration::from_millis(50), 1024 * 1024),
            Some(false)
        );
    }

    #[test]
    fn checkpoints_and_cleanup_cannot_starve_each_other() {
        let now = Instant::now();
        let mut schedule = CheckpointSchedule::new(true, Duration::from_secs(60));
        assert_eq!(schedule.next(now, 0), Some(true));
        schedule.completed(true, false, now, 0);
        let later = now + Duration::from_millis(60);
        assert_eq!(schedule.next(later, 2 * 1024 * 1024), Some(false));
        schedule.completed(false, true, later, 2 * 1024 * 1024);
        assert_eq!(schedule.next(later, 4 * 1024 * 1024), Some(true));
        schedule.completed(true, false, later, 4 * 1024 * 1024);
        assert_eq!(schedule.next(later, 4 * 1024 * 1024), Some(false));
        schedule.completed(false, true, later, 4 * 1024 * 1024);
        assert_eq!(schedule.next(later, 4 * 1024 * 1024), Some(true));
        schedule.completed(true, true, later, 4 * 1024 * 1024);
        assert_eq!(schedule.next(later, 4 * 1024 * 1024), None);
    }

    #[test]
    fn unpaced_jobs_preserve_cleanup_cadence_and_resume_yielded_passes() {
        let now = Instant::now();
        let mut schedule = CheckpointSchedule::new(false, Duration::from_secs(60));
        assert_eq!(schedule.next(now, 0), Some(true));
        schedule.completed(true, false, now, 0);
        assert_eq!(schedule.next(now + Duration::from_secs(2), 0), Some(true));
        schedule.completed(true, true, now, 0);
        assert_eq!(schedule.next(now + Duration::from_secs(1), 0), Some(false));
        assert_eq!(schedule.next(now + Duration::from_secs(60), 0), Some(true));
    }
}
