use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use asc_foundation_types::ResourceId;
use asc_pap::{BindingReconcileEnqueuer, EnqueueError, PapError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Entry {
    /// Ready for pickup; retries counts automatic reschedules since the last notification.
    Queued { retries: u32 },
    /// One call owns this ID; dirty records a new notification received during the call.
    Running { dirty: bool, retries: u32 },
    /// Waiting for a deadline in the shared monotonic millisecond clock domain.
    WaitingRetry { retry_at: u64, retries: u32 },
    /// Terminalization could not be confirmed after retry exhaustion or panic.
    /// Retained until a new notification so scanning
    /// cannot restart it; currently still occupies one capacity slot.
    Exhausted,
}

/// Process-local scheduling state protected by `WorkQueue`'s mutex.
/// Binding intent, status/error and deployment records live in the Repository.
/// Attempt counts and monotonic deadlines are owned here and never persisted.
#[derive(Default)]
pub(super) struct State {
    /// FIFO of distinct IDs ready for pickup. Each has exactly one Queued entry;
    /// running, waiting and exhausted IDs are absent from this deque.
    pub ready: VecDeque<ResourceId>,
    /// One scheduling entry per Binding ID, including running/waiting/exhausted work.
    /// `entries.len()`, not `ready.len()`, is compared against `WorkQueue`'s capacity.
    pub entries: BTreeMap<ResourceId, Entry>,
    /// One progress value per entry; checked out by its exclusive running worker.
    /// Removed with the entry, retained through waiting, dirty and exhausted states.
    pub schedules: BTreeMap<ResourceId, asc_pcp::AttemptSchedule>,
    /// Stops admission, new pickup and scanning. Existing synchronous calls still
    /// finish and must be joined; setting this flag does not cancel Client I/O.
    pub stopped: bool,
    /// Scheduling code/scanner panicked or a thread could not start. Also sets stopped;
    /// retained so shutdown and the daemon can distinguish failure from a normal stop.
    pub fatal: bool,
    /// The latest compensation scan failed; cleared by a successful scan.
    /// Degrades health without closing Binding write admission.
    pub scan_failed: bool,
    /// Saturating cumulative count of rejected new-ID insertions (notifications
    /// or scan candidates) at capacity, plus enqueue calls after stop. Not a count
    /// of lost Repository intents or distinct IDs; currently read only by tests.
    pub overflow_count: u64,
}

/// Bounded, coalescing queue. All callbacks and I/O happen outside its mutex.
pub struct WorkQueue {
    pub(super) state: Mutex<State>,
    /// Wakes workers to recheck ready/stopped, and wakes the timer on shutdown.
    pub(super) wake: Condvar,
    /// Maximum number of distinct IDs in entries across all states (default 65,536).
    /// Notifications for an existing ID need no additional slot, even when full.
    capacity: usize,
    /// Automatic retries allowed after the initial call (default 4).
    max_auto_retries: u32,
}

impl WorkQueue {
    pub(super) fn new(capacity: usize, max_auto_retries: u32) -> Self {
        Self {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            capacity,
            max_auto_retries,
        }
    }

    /// Merges one bounded scan page under a single lock without changing existing work.
    pub(super) fn discover_many(
        &self,
        candidates: impl IntoIterator<Item = (ResourceId, Option<u64>)>,
        now: u64,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.stopped {
            return;
        }
        let mut ready_added = false;
        for (id, deadline) in candidates {
            if !state.entries.contains_key(&id) {
                ready_added |= self.insert(&mut state, id, deadline.filter(|at| *at > now));
            }
        }
        if ready_added {
            self.wake.notify_all();
        }
    }

    /// Returns whether a new ready entry was added; the caller batches wake-ups.
    fn insert(&self, state: &mut State, id: ResourceId, deadline: Option<u64>) -> bool {
        if state.entries.len() >= self.capacity {
            state.overflow_count = state.overflow_count.saturating_add(1);
            return false;
        }
        if let Some(retry_at) = deadline {
            state.entries.insert(
                id,
                Entry::WaitingRetry {
                    retry_at,
                    retries: 0,
                },
            );
        } else {
            state
                .entries
                .insert(id.clone(), Entry::Queued { retries: 0 });
            state.ready.push_back(id);
        }
        deadline.is_none()
    }

    pub(super) fn take_schedule(&self, id: &ResourceId) -> asc_pcp::AttemptSchedule {
        self.state
            .lock()
            .unwrap()
            .schedules
            .remove(id)
            .unwrap_or_default()
    }
    pub(super) fn save_schedule(&self, id: &ResourceId, schedule: asc_pcp::AttemptSchedule) {
        let mut state = self.state.lock().unwrap();
        assert!(matches!(state.entries.get(id), Some(Entry::Running { .. })));
        state.schedules.insert(id.clone(), schedule);
    }

    pub(super) fn take(&self) -> Option<ResourceId> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.stopped {
                return None;
            }
            if let Some(id) = state.ready.pop_front() {
                let Some(Entry::Queued { retries }) = state.entries.get(&id).copied() else {
                    unreachable!("ready entry must be queued");
                };
                state.entries.insert(
                    id.clone(),
                    Entry::Running {
                        dirty: false,
                        retries,
                    },
                );
                return Some(id);
            }
            state = self.wake.wait(state).unwrap();
        }
    }

    pub(super) fn is_dirty(&self, id: &ResourceId) -> bool {
        matches!(
            self.state.lock().unwrap().entries.get(id),
            Some(Entry::Running { dirty: true, .. })
        )
    }

    /// Returns true with Running retained when the automatic retry budget is exhausted.
    /// A new notification supersedes this outcome and starts a fresh queue budget.
    pub(super) fn finish(&self, id: ResourceId, deadline: Option<u64>, auto_retry: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(Entry::Running { dirty, retries }) = state.entries.get(&id).copied() else {
            unreachable!("only a running entry can finish");
        };
        let exhausted = !dirty && auto_retry && retries >= self.max_auto_retries;
        if dirty {
            state
                .entries
                .insert(id.clone(), Entry::Queued { retries: 0 });
            state.ready.push_back(id);
        } else if exhausted {
            // The worker retains Running while it conditionally persists Failed.
            // finish_terminalization releases only after confirmation.
        } else if let Some(retry_at) = deadline {
            state.entries.insert(
                id,
                Entry::WaitingRetry {
                    retry_at,
                    retries: retries + u32::from(auto_retry),
                },
            );
        } else {
            state.entries.remove(&id);
            state.schedules.remove(&id);
        }
        self.wake.notify_all();
        exhausted
    }

    pub(super) fn tick(&self, now: u64) {
        let mut state = self.state.lock().unwrap();
        let due: Vec<_> = state
            .entries
            .iter()
            .filter_map(|(id, entry)| match entry {
                Entry::WaitingRetry { retry_at, retries } if *retry_at <= now => {
                    Some((id.clone(), *retries))
                }
                _ => None,
            })
            .collect();
        for (id, retries) in due {
            state.entries.insert(id.clone(), Entry::Queued { retries });
            state.ready.push_back(id);
        }
        self.wake.notify_all();
    }

    /// Finish a terminalization attempt without stopping other work. Repository I/O
    /// has already finished outside the lock; notifications still take precedence.
    pub(super) fn finish_terminalization(&self, id: ResourceId, terminal: bool) {
        let mut state = self.state.lock().unwrap();
        let Some(Entry::Running { dirty, .. }) = state.entries.get(&id).copied() else {
            unreachable!("only a running entry can finish");
        };
        if dirty {
            state
                .entries
                .insert(id.clone(), Entry::Queued { retries: 0 });
            state.ready.push_back(id);
        } else if terminal {
            state.entries.remove(&id);
            state.schedules.remove(&id);
        } else {
            state.entries.insert(id, Entry::Exhausted);
        }
        self.wake.notify_all();
    }

    pub(super) fn wait_tick(&self, interval: Duration) -> bool {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .wake
            .wait_timeout_while(state, interval, |state| !state.stopped)
            .unwrap();
        !state.stopped
    }

    /// # Panics
    /// Panics if an internal queue mutation poisoned its mutex.
    pub fn stop(&self) {
        self.state.lock().unwrap().stopped = true;
        self.wake.notify_all();
    }

    pub(super) fn fail(&self) {
        let mut state = self.state.lock().unwrap();
        state.fatal = true;
        state.stopped = true;
        self.wake.notify_all();
    }

    /// # Panics
    /// Panics if an internal queue mutation poisoned its mutex.
    pub fn is_healthy(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.stopped && !state.fatal && !state.scan_failed
    }
    /// Returns true after an execution thread failed.
    /// # Panics
    /// Panics if an internal queue mutation poisoned its mutex.
    pub fn has_failed(&self) -> bool {
        self.state.lock().unwrap().fatal
    }
}

impl BindingReconcileEnqueuer for WorkQueue {
    fn check_ready(&self) -> Result<(), PapError> {
        let state = self.state.lock().unwrap();
        // Admission depends on live scheduling, not the outcome of other work.
        if !state.stopped && !state.fatal {
            Ok(())
        } else {
            Err(PapError::Unavailable)
        }
    }
    fn enqueue(&self, id: &ResourceId) -> Result<(), EnqueueError> {
        let mut state = self.state.lock().unwrap();
        if state.stopped {
            state.overflow_count = state.overflow_count.saturating_add(1);
            return Err(EnqueueError::Stopped);
        }
        match state.entries.get_mut(id) {
            Some(Entry::Queued { retries }) => *retries = 0,
            Some(Entry::Running { dirty, .. }) => *dirty = true,
            Some(entry @ (Entry::WaitingRetry { .. } | Entry::Exhausted)) => {
                *entry = Entry::Queued { retries: 0 };
                state.ready.push_back(id.clone());
                self.wake.notify_all();
            }
            None => {
                if self.insert(&mut state, id.clone(), None) {
                    self.wake.notify_all();
                } else {
                    return Err(EnqueueError::Full);
                }
            }
        }
        Ok(())
    }
}
