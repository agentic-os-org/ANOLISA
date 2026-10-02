//! Generation-owned lifecycle for the background containment reconciler.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{ContainmentCoordinator, ContainmentError, now_ns};

/// Serializes worker generations and prevents stale teardown from clearing a successor.
pub(super) struct ReconcilerControl {
    state: Mutex<ReconcilerState>,
}

struct ReconcilerState {
    next_generation: u64,
    active: Option<ActiveGeneration>,
}

struct ActiveGeneration {
    id: u64,
    stop: Arc<AtomicBool>,
}

struct GenerationGuard {
    control: Arc<ReconcilerControl>,
    id: u64,
}

impl ReconcilerControl {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(ReconcilerState {
                next_generation: 0,
                active: None,
            }),
        }
    }

    fn install_generation(&self) -> Result<(u64, Arc<AtomicBool>), ContainmentError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .active
            .as_ref()
            .is_some_and(|active| !active.stop.load(Ordering::Acquire))
        {
            return Err(ContainmentError::AlreadyRunning);
        }
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        let id = state.next_generation;
        let stop = Arc::new(AtomicBool::new(false));
        state.active = Some(ActiveGeneration {
            id,
            stop: Arc::clone(&stop),
        });
        Ok((id, stop))
    }

    fn clear_generation(&self, id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active.as_ref().is_some_and(|active| active.id == id) {
            state.active = None;
        }
    }

    fn stop_active(&self) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(active) = &state.active {
            active.stop.store(true, Ordering::Release);
        }
    }
}

impl Drop for GenerationGuard {
    fn drop(&mut self) {
        self.control.clear_generation(self.id);
    }
}

impl ContainmentCoordinator {
    /// Starts one stoppable background reconciliation worker generation.
    ///
    /// A stopped generation may be replaced before its thread finishes; its
    /// teardown cannot clear or stop the replacement.
    ///
    /// # Errors
    ///
    /// Returns [`ContainmentError::AlreadyRunning`] for an active generation or
    /// [`ContainmentError::ReconcilerThread`] when spawning the thread fails.
    pub fn start_reconciler(&self, interval: Duration) -> Result<JoinHandle<()>, ContainmentError> {
        let (generation, stop) = self.worker.install_generation()?;
        let interval = interval.max(Duration::from_millis(10));
        let store = Arc::clone(&self.store);
        let enforcer = Arc::clone(&self.enforcer);
        let control = Arc::clone(&self.worker);
        thread::Builder::new()
            .name("agentsight-containment-reconciler".into())
            .spawn(move || {
                let _guard = GenerationGuard {
                    control,
                    id: generation,
                };
                while !stop.load(Ordering::Acquire) {
                    if let Err(error) =
                        super::reconciler::reconcile_batch(&store, enforcer.as_ref(), now_ns())
                    {
                        log::error!("containment reconciliation failed: {error}");
                    }
                    sleep_until_stopped(&stop, interval);
                }
            })
            .map_err(|error| {
                self.worker.clear_generation(generation);
                ContainmentError::ReconcilerThread(error)
            })
    }

    /// Requests the active background worker generation to stop.
    pub fn stop(&self) {
        self.worker.stop_active();
    }
}

impl Drop for ContainmentCoordinator {
    fn drop(&mut self) {
        self.stop();
    }
}

fn sleep_until_stopped(stop: &AtomicBool, duration: Duration) {
    let mut remaining = duration;
    while !stop.load(Ordering::Acquire) && !remaining.is_zero() {
        let step = remaining.min(Duration::from_millis(50));
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
}
