//! Single-threaded scheduling and status tracking for maintenance jobs.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

use crate::{LifecycleError, MaintenanceLock, MaintenanceLockAcquire};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const LOCK_BUSY_BACKOFF: Duration = Duration::from_secs(1);
const ERROR_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// One schema-neutral operation scheduled by [`MaintenanceWorker`].
///
/// Implementations own all schema-specific behavior. The worker only schedules
/// calls and serializes them across processes with [`MaintenanceLock`].
pub trait MaintenanceJob: Send + 'static {
    /// Returns the stable identifier used in worker status.
    fn id(&self) -> &str;

    /// Returns the database whose maintenance lock protects this job.
    fn db_path(&self) -> &Path;

    /// Returns the delay between completed attempts.
    fn interval(&self) -> Duration;

    /// Performs one maintenance attempt.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle error when the attempt cannot complete. The worker
    /// records the failure and continues scheduling all jobs.
    fn run(&mut self) -> Result<(), LifecycleError>;
}

/// Categorized outcome of the most recent job attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceJobResult {
    /// The job completed successfully.
    Success,
    /// The job or lock setup returned an error.
    Error,
    /// Another process held the database maintenance lock.
    LockBusy,
    /// The job panicked; the worker contained the unwind.
    Panicked,
}

/// Observable state for one registered maintenance job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceJobState {
    /// Stable job identifier; database paths are deliberately omitted.
    pub id: String,
    /// Unix timestamp in milliseconds for the latest attempt.
    pub last_attempt_unix_ms: Option<u64>,
    /// Unix timestamp in milliseconds for the latest successful attempt.
    pub last_success_unix_ms: Option<u64>,
    /// Categorized result of the latest attempt.
    pub last_result: Option<MaintenanceJobResult>,
    /// Number of unsuccessful attempts since the latest success.
    pub consecutive_failures: u64,
    /// Unix timestamp in milliseconds for the next scheduled attempt.
    pub next_run_unix_ms: u64,
}

/// Snapshot of maintenance worker health and per-job progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceWorkerState {
    /// Whether the worker thread is active.
    pub running: bool,
    /// Unix timestamp in milliseconds of the latest scheduler heartbeat.
    pub heartbeat_unix_ms: u64,
    /// Job states in registration order.
    pub jobs: Vec<MaintenanceJobState>,
}

/// Single-threaded scheduler for schema-neutral SQLite maintenance jobs.
///
/// All registered jobs execute sequentially on one dedicated thread. Dropping
/// the worker requests shutdown and joins that thread.
pub struct MaintenanceWorker {
    stop_sender: mpsc::Sender<()>,
    stop_requested: bool,
    thread: Option<thread::JoinHandle<()>>,
    state: Arc<Mutex<MaintenanceWorkerState>>,
}

impl MaintenanceWorker {
    /// Starts a worker and schedules every job for immediate execution.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or duplicate identifiers, zero intervals, or
    /// when the operating system cannot create the worker thread.
    pub fn start(jobs: Vec<Box<dyn MaintenanceJob>>) -> Result<Self, LifecycleError> {
        if jobs.is_empty() {
            return Err(LifecycleError::InvalidMaintenanceJob(
                "at least one maintenance job is required".to_owned(),
            ));
        }
        let now = Instant::now();
        let now_unix_ms = unix_ms();
        let mut ids = HashSet::with_capacity(jobs.len());
        let mut scheduled = Vec::with_capacity(jobs.len());
        let mut job_states = Vec::with_capacity(jobs.len());

        for job in jobs {
            let id = job.id().to_owned();
            if id.is_empty() {
                return Err(LifecycleError::InvalidMaintenanceJob(
                    "job identifier must not be empty".to_owned(),
                ));
            }
            if !ids.insert(id.clone()) {
                return Err(LifecycleError::InvalidMaintenanceJob(format!(
                    "duplicate job identifier: {id}"
                )));
            }
            let interval = job.interval();
            if interval.is_zero() {
                return Err(LifecycleError::InvalidMaintenanceJob(format!(
                    "job interval must be non-zero: {id}"
                )));
            }
            scheduled.push(ScheduledJob {
                db_path: job.db_path().to_path_buf(),
                interval,
                next_run: now,
                consecutive_failures: 0,
                job,
            });
            job_states.push(MaintenanceJobState {
                id,
                last_attempt_unix_ms: None,
                last_success_unix_ms: None,
                last_result: None,
                consecutive_failures: 0,
                next_run_unix_ms: now_unix_ms,
            });
        }

        let state = Arc::new(Mutex::new(MaintenanceWorkerState {
            running: true,
            heartbeat_unix_ms: now_unix_ms,
            jobs: job_states,
        }));
        let thread_state = Arc::clone(&state);
        let (stop_sender, stop_receiver) = mpsc::channel();
        let worker_thread = thread::Builder::new()
            .name("sqlite-maintenance".to_owned())
            .spawn(move || worker_loop(scheduled, stop_receiver, &thread_state))
            .map_err(LifecycleError::MaintenanceWorkerStart)?;

        Ok(Self {
            stop_sender,
            stop_requested: false,
            thread: Some(worker_thread),
            state,
        })
    }

    /// Returns a point-in-time copy of worker and job status.
    ///
    /// # Errors
    ///
    /// Returns an error if a previous panic poisoned the state lock.
    pub fn state(&self) -> Result<MaintenanceWorkerState, LifecycleError> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| LifecycleError::MaintenanceWorkerStatePoisoned)
    }

    /// Requests worker shutdown without waiting for a running job.
    ///
    /// Repeated calls are safe and do not enqueue duplicate stop messages.
    pub fn stop(&mut self) {
        if !self.stop_requested {
            self.stop_requested = true;
            let _ = self.stop_sender.send(());
        }
    }

    /// Joins the worker thread after shutdown has been requested.
    ///
    /// Calling this method again after a successful join is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker thread panicked outside a job invocation.
    pub fn join(&mut self) -> Result<(), LifecycleError> {
        self.stop();
        let Some(worker_thread) = self.thread.take() else {
            return Ok(());
        };
        match worker_thread.join() {
            Ok(()) => Ok(()),
            Err(_) => {
                set_running(&self.state, false);
                Err(LifecycleError::MaintenanceWorkerPanicked)
            }
        }
    }
}

impl Drop for MaintenanceWorker {
    fn drop(&mut self) {
        self.stop();
        let _ = self.join();
    }
}

struct ScheduledJob {
    db_path: PathBuf,
    interval: Duration,
    next_run: Instant,
    consecutive_failures: u32,
    job: Box<dyn MaintenanceJob>,
}

fn worker_loop(
    mut jobs: Vec<ScheduledJob>,
    stop_receiver: mpsc::Receiver<()>,
    state: &Arc<Mutex<MaintenanceWorkerState>>,
) {
    loop {
        if stop_receiver.try_recv().is_ok() {
            break;
        }

        let now = Instant::now();
        for (index, scheduled) in jobs.iter_mut().enumerate() {
            if scheduled.next_run > now {
                continue;
            }
            if stop_receiver.try_recv().is_ok() {
                set_running(state, false);
                return;
            }
            run_job(index, scheduled, state);
        }

        heartbeat(state);
        let timeout = jobs
            .iter()
            .map(|job| job.next_run.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(HEARTBEAT_INTERVAL)
            .min(HEARTBEAT_INTERVAL);
        match stop_receiver.recv_timeout(timeout) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    set_running(state, false);
}

fn run_job(index: usize, scheduled: &mut ScheduledJob, state: &Arc<Mutex<MaintenanceWorkerState>>) {
    let attempt_unix_ms = unix_ms();
    update_state(state, |worker_state| {
        worker_state.heartbeat_unix_ms = attempt_unix_ms;
        if let Some(job_state) = worker_state.jobs.get_mut(index) {
            job_state.last_attempt_unix_ms = Some(attempt_unix_ms);
        }
    });

    let job_id = scheduled.job.id().to_owned();
    let result = match MaintenanceLock::try_acquire(&scheduled.db_path) {
        Ok(MaintenanceLockAcquire::Acquired(_guard)) => {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scheduled.job.run()));
            match outcome {
                Ok(Ok(())) => MaintenanceJobResult::Success,
                Ok(Err(error)) => {
                    log::warn!("SQLite maintenance job {job_id} failed: {error}");
                    MaintenanceJobResult::Error
                }
                Err(_) => {
                    log::error!("SQLite maintenance job {job_id} panicked");
                    MaintenanceJobResult::Panicked
                }
            }
        }
        Ok(MaintenanceLockAcquire::Busy) => {
            log::debug!("SQLite maintenance job {job_id} skipped because its lock is busy");
            MaintenanceJobResult::LockBusy
        }
        Err(error) => {
            log::warn!("SQLite maintenance job {job_id} could not acquire its lock: {error}");
            MaintenanceJobResult::Error
        }
    };
    let delay = match result {
        MaintenanceJobResult::Success => {
            scheduled.consecutive_failures = 0;
            scheduled.interval
        }
        MaintenanceJobResult::LockBusy => {
            scheduled.consecutive_failures = scheduled.consecutive_failures.saturating_add(1);
            retry_delay(
                LOCK_BUSY_BACKOFF,
                scheduled.consecutive_failures,
                scheduled.interval,
            )
        }
        MaintenanceJobResult::Error | MaintenanceJobResult::Panicked => {
            scheduled.consecutive_failures = scheduled.consecutive_failures.saturating_add(1);
            retry_delay(
                ERROR_BACKOFF,
                scheduled.consecutive_failures,
                scheduled.interval,
            )
        }
    };

    let completed_unix_ms = unix_ms();
    scheduled.next_run = Instant::now()
        .checked_add(delay)
        .unwrap_or_else(|| Instant::now() + HEARTBEAT_INTERVAL);
    let next_run_unix_ms = completed_unix_ms.saturating_add(duration_ms(delay));
    update_state(state, |worker_state| {
        worker_state.heartbeat_unix_ms = completed_unix_ms;
        if let Some(job_state) = worker_state.jobs.get_mut(index) {
            job_state.last_result = Some(result);
            job_state.next_run_unix_ms = next_run_unix_ms;
            if result == MaintenanceJobResult::Success {
                job_state.last_success_unix_ms = Some(completed_unix_ms);
            }
            job_state.consecutive_failures = u64::from(scheduled.consecutive_failures);
        }
    });
}

fn retry_delay(base: Duration, failures: u32, interval: Duration) -> Duration {
    let exponent = failures.saturating_sub(1).min(8);
    base.saturating_mul(1_u32 << exponent)
        .min(MAX_BACKOFF)
        .min(interval)
}

fn heartbeat(state: &Arc<Mutex<MaintenanceWorkerState>>) {
    update_state(state, |worker_state| {
        worker_state.heartbeat_unix_ms = unix_ms();
    });
}

fn set_running(state: &Arc<Mutex<MaintenanceWorkerState>>, running: bool) {
    update_state(state, |worker_state| {
        worker_state.running = running;
        worker_state.heartbeat_unix_ms = unix_ms();
    });
}

fn update_state(
    state: &Arc<Mutex<MaintenanceWorkerState>>,
    update: impl FnOnce(&mut MaintenanceWorkerState),
) {
    match state.lock() {
        Ok(mut worker_state) => update(&mut worker_state),
        Err(poisoned) => {
            let mut worker_state = poisoned.into_inner();
            update(&mut worker_state);
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(duration_ms)
        .unwrap_or(0)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
