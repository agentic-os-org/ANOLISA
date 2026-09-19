//! Production diagnostics must not strand reconciliation workers on stderr I/O.
use std::io::{ErrorKind, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use asc_foundation_types::ResourceId;
use asc_pap::BindingReconcileEnqueuer as _;
use asc_pcp::{AttemptSchedule, Clock, Disposition};
use asc_policy_repository::{
    BindingReconcileCatalog, BindingStateRepository, BindingStateSnapshot, BindingStateWrite,
    ReconcileCandidate, StoreError, WriteResult,
};
use asc_policy_runtime::reconciliation::{
    MonotonicClock, ReconcileAttempt, ReconciliationRuntime, RuntimeConfig,
};

#[derive(Default)]
struct UnavailableRepository(AtomicUsize);
impl BindingStateRepository for UnavailableRepository {
    fn get_binding_state(
        &self,
        _: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(StoreError::Unavailable)
    }
    fn compare_exchange_binding_state(
        &self,
        _: &BindingStateSnapshot,
        _: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        unreachable!("failed reads cannot write state")
    }
}
impl BindingReconcileCatalog for UnavailableRepository {
    fn scan_reconciliation(
        &self,
        _: Option<&ResourceId>,
        _: usize,
    ) -> Result<Vec<ReconcileCandidate>, StoreError> {
        Ok(vec![])
    }
}
impl ReconcileAttempt for UnavailableRepository {
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(MonotonicClock::default())
    }
    fn reconcile(
        &self,
        _: &ResourceId,
        _: &mut AttemptSchedule,
    ) -> Result<Disposition, StoreError> {
        unreachable!("failed reads cannot invoke a target")
    }
}

#[test]
fn blocked_stderr_does_not_prevent_worker_terminalization_or_join() {
    const CHILD_ENV: &str = "ASC_DIAGNOSTIC_WORKER_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let telemetry = asc_observability::init_runtime("asc-daemon-test").unwrap();
        let repository = Arc::new(UnavailableRepository::default());
        let runtime = ReconciliationRuntime::start(
            repository.clone(),
            repository.clone(),
            RuntimeConfig {
                workers: 1,
                max_auto_retries: 0,
                ..RuntimeConfig::default()
            },
        )
        .unwrap();
        runtime
            .enqueuer()
            .enqueue(&ResourceId::new("10000000-0000-4000-8000-000000000001").unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        // Both the failed read diagnostic and unconfirmed terminalization must run.
        while repository.0.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline, "worker stopped progressing");
            std::thread::yield_now();
        }
        runtime.shutdown().unwrap();
        telemetry.shutdown(Duration::from_millis(50));
        return;
    }
    for filter in ["info", "off"] {
        let (_reader, mut writer) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        loop {
            match writer.write(&[b'x'; 4096]) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill stderr: {error}"),
            }
        }
        writer.set_nonblocking(false).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "blocked_stderr_does_not_prevent_worker_terminalization_or_join",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env("RUST_LOG", filter)
            .stderr(Stdio::from(OwnedFd::from(writer)))
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "worker child failed: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("RUST_LOG={filter}: worker shutdown blocked on diagnostics");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
