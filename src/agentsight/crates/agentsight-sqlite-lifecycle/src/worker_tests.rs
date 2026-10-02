//! Tests for maintenance worker scheduling and isolation.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    LifecycleError, MaintenanceJob, MaintenanceJobResult, MaintenanceLock, MaintenanceLockAcquire,
    MaintenanceWorker, MaintenanceWorkerState,
};

type JobAction = Box<dyn FnMut() -> Result<(), LifecycleError> + Send>;

struct TestJob {
    id: &'static str,
    path: PathBuf,
    interval: Duration,
    action: JobAction,
}

impl MaintenanceJob for TestJob {
    fn id(&self) -> &str {
        self.id
    }

    fn db_path(&self) -> &Path {
        &self.path
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn run(&mut self) -> Result<(), LifecycleError> {
        (self.action)()
    }
}

fn test_path(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system time must follow Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agentsight-worker-{name}-{}-{nonce}.db",
        std::process::id()
    ))
}

fn job(id: &'static str, action: JobAction) -> Box<dyn MaintenanceJob> {
    Box::new(TestJob {
        id,
        path: test_path(id),
        interval: Duration::from_secs(60),
        action,
    })
}

fn wait_for_state(
    worker: &MaintenanceWorker,
    predicate: impl Fn(&MaintenanceWorkerState) -> bool,
) -> MaintenanceWorkerState {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = worker.state().unwrap();
        if predicate(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "worker state did not converge");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn all_jobs_run_on_one_worker_thread() {
    let thread_ids = Arc::new(Mutex::new(Vec::new()));
    let first_ids = Arc::clone(&thread_ids);
    let second_ids = Arc::clone(&thread_ids);
    let jobs = vec![
        job(
            "first",
            Box::new(move || {
                first_ids.lock().unwrap().push(thread::current().id());
                Ok(())
            }),
        ),
        job(
            "second",
            Box::new(move || {
                second_ids.lock().unwrap().push(thread::current().id());
                Ok(())
            }),
        ),
    ];

    let mut worker = MaintenanceWorker::start(jobs).unwrap();
    wait_for_state(&worker, |state| {
        state
            .jobs
            .iter()
            .all(|job| job.last_result == Some(MaintenanceJobResult::Success))
    });
    worker.stop();
    worker.join().unwrap();

    let ids = thread_ids.lock().unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
    assert_ne!(ids[0], thread::current().id());
}

#[test]
fn job_error_does_not_stop_later_jobs() {
    let successful_runs = Arc::new(AtomicUsize::new(0));
    let runs = Arc::clone(&successful_runs);
    let jobs = vec![
        job(
            "error",
            Box::new(|| Err(LifecycleError::InvalidPolicy("test failure"))),
        ),
        job(
            "success",
            Box::new(move || {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        ),
    ];

    let mut worker = MaintenanceWorker::start(jobs).unwrap();
    let state = wait_for_state(&worker, |state| {
        state.jobs[1].last_result == Some(MaintenanceJobResult::Success)
    });
    worker.stop();
    worker.join().unwrap();

    assert_eq!(state.jobs[0].last_result, Some(MaintenanceJobResult::Error));
    assert_eq!(state.jobs[0].consecutive_failures, 1);
    assert_eq!(successful_runs.load(Ordering::SeqCst), 1);
}

#[test]
fn job_panic_is_contained_and_later_jobs_run() {
    let successful_runs = Arc::new(AtomicUsize::new(0));
    let runs = Arc::clone(&successful_runs);
    let jobs = vec![
        job("panic", Box::new(|| panic!("intentional job panic"))),
        job(
            "after-panic",
            Box::new(move || {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        ),
    ];

    let mut worker = MaintenanceWorker::start(jobs).unwrap();
    let state = wait_for_state(&worker, |state| {
        state.jobs[1].last_result == Some(MaintenanceJobResult::Success)
    });
    worker.stop();
    worker.join().unwrap();

    assert_eq!(
        state.jobs[0].last_result,
        Some(MaintenanceJobResult::Panicked)
    );
    assert_eq!(successful_runs.load(Ordering::SeqCst), 1);
}

#[test]
fn lock_contention_is_recorded_without_running_the_job() {
    let path = test_path("busy");
    let lock = MaintenanceLock::try_acquire(&path).unwrap();
    let MaintenanceLockAcquire::Acquired(_guard) = lock else {
        panic!("test must acquire the maintenance lock");
    };
    let runs = Arc::new(AtomicUsize::new(0));
    let job_runs = Arc::clone(&runs);
    let busy_job = TestJob {
        id: "busy",
        path,
        interval: Duration::from_secs(60),
        action: Box::new(move || {
            job_runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    };

    let mut worker = MaintenanceWorker::start(vec![Box::new(busy_job)]).unwrap();
    let state = wait_for_state(&worker, |state| {
        state.jobs[0].last_result == Some(MaintenanceJobResult::LockBusy)
    });
    worker.stop();
    worker.join().unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(state.jobs[0].consecutive_failures, 1);
    assert!(state.jobs[0].last_attempt_unix_ms.is_some());
    assert!(state.jobs[0].last_success_unix_ms.is_none());
}

#[test]
fn empty_job_list_is_rejected() {
    assert!(matches!(
        MaintenanceWorker::start(Vec::new()),
        Err(LifecycleError::InvalidMaintenanceJob(_))
    ));
}

#[test]
fn stop_and_join_are_idempotent_and_interrupt_idle_wait() {
    let runs = Arc::new(AtomicUsize::new(0));
    let job_runs = Arc::clone(&runs);
    let mut worker = MaintenanceWorker::start(vec![job(
        "stoppable",
        Box::new(move || {
            job_runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    )])
    .unwrap();
    wait_for_state(&worker, |state| {
        state.jobs[0].last_result == Some(MaintenanceJobResult::Success)
    });

    let started = Instant::now();
    worker.join().unwrap();
    worker.stop();
    worker.join().unwrap();

    assert!(started.elapsed() < Duration::from_millis(250));
    assert!(!worker.state().unwrap().running);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}
