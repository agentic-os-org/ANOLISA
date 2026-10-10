//! Periodic security-event `SQLite` retention for the long-lived daemon.
//!
//! v1 ran retention when short-lived CLI processes exited through `atexit`,
//! and the daemon only ran the gated pass in `event_sinks.close()` after its
//! main loop returned - a daemon that keeps running never pruned, and a hard
//! kill skipped the shutdown pass entirely, so expired events survived until
//! some orderly exit. The retention task gives the daemon its own lifecycle:
//!
//! - the startup catch-up ([`run_startup_catchup`]) runs one gated pass
//!   **before UDS admission begins**, so the potentially large first prune
//!   over a historical backlog never overlaps request audit writes; a stop
//!   signal that arrives while it is still running abandons admission (the
//!   daemon never serves) instead of making systemd wait out `TimeoutStopSec`
//!   and SIGKILL the process mid-catch-up;
//! - after admission, the task re-checks the shared cross-process gate once
//!   per [`RetentionSchedule::check_interval`]; the gate still decides whether
//!   anything actually runs;
//! - failures and contention surface through the daemon's diagnostics and
//!   retry after [`RetentionSchedule::failure_retry`] instead of waiting for
//!   the next daily window;
//! - shutdown is cooperative: [`RetentionTask::shutdown`] stops scheduling,
//!   then joins the task - and through it any in-flight `spawn_blocking`
//!   pass - so the bounded final pass in `event_sinks.close()` runs only
//!   after the task has terminated and can never contend with residual
//!   maintenance on the same `SqliteStore` mutex. The cancellation
//!   transition and new-pass submission are serialized through a shared
//!   gate, so a signal that arrives while the loop sits between its
//!   cancellation check and its `spawn_blocking` submission still prevents
//!   that pass.
//!
//! The service contract (trigger, readiness, health, logging, retry,
//! cancellation, shutdown, and fixtures) is
//! `src/agent-sec-core/docs/design/DAEMON_JOB_CONTRACT_zh.md` section 11.5.

use std::sync::Arc;
use std::time::Duration;

use asc_daemon_service::ShutdownToken;
use asc_event_sink::MaintenanceOutcome;
use tokio::sync::{Mutex, oneshot, watch};

/// How often a running daemon re-checks the security-event `SQLite`
/// retention gate. The gate itself stays the shared daily window; this is
/// only the cadence at which a long-lived daemon looks at it.
pub const RETENTION_CHECK_INTERVAL_SECONDS: u64 = 3600;

/// How quickly a failed or contended retention pass is retried. The gate does
/// not advance over a failure, so the retry is bounded only by this backoff.
pub const RETENTION_FAILURE_RETRY_SECONDS: u64 = 60;

/// Terminal-join budget for the retention task at shutdown, mirroring the
/// skill-worker drain budget in the composition root.
pub const RETENTION_JOIN_TIMEOUT: Duration = Duration::from_secs(65);

/// One blocking maintenance attempt against the security-events store.
///
/// The production pass is `ConfiguredSecurityEventSinks::run_sqlite_retention`;
/// the seam keeps the lifecycle fixtures in `tests/retention_lifecycle.rs`
/// production-shaped without a live database.
pub type RetentionPass = Arc<dyn Fn(f64) -> MaintenanceOutcome + Send + Sync>;

/// Where lifecycle diagnostics go (the daemon's telemetry reporter).
pub type RetentionReport = Arc<dyn Fn(&str) + Send + Sync>;

/// The retention task's timing knobs.
#[derive(Debug, Clone, Copy)]
pub struct RetentionSchedule {
    /// Cadence between gate checks once the daemon is serving.
    pub check_interval: Duration,
    /// Backoff after a failed or contended pass before the next attempt.
    pub failure_retry: Duration,
}

impl Default for RetentionSchedule {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(RETENTION_CHECK_INTERVAL_SECONDS),
            failure_retry: Duration::from_secs(RETENTION_FAILURE_RETRY_SECONDS),
        }
    }
}

impl RetentionSchedule {
    /// Builds an explicit schedule.
    #[must_use]
    pub const fn new(check_interval: Duration, failure_retry: Duration) -> Self {
        Self {
            check_interval,
            failure_retry,
        }
    }
}

/// The handle owning the retention task's lifecycle.
///
/// Dropping the handle cancels the task; [`RetentionTask::shutdown`] cancels
/// and joins it. The task is never aborted: an in-flight `spawn_blocking`
/// pass cannot be cancelled (see `runtime.rs`), so ownership is expressed
/// through the cooperative cancel signal plus a terminal join.
pub struct RetentionTask {
    join: tokio::task::JoinHandle<()>,
    cancel: watch::Sender<bool>,
    /// Serializes the cancellation transition with new-pass submission.
    ///
    /// `watch` makes the flag observable, but a check-then-spawn sequence is
    /// not atomic on its own: a signal can land after a check returned
    /// `false` and before the `spawn_blocking` submission, starting a pass
    /// the signal already forbade. Both [`RetentionTask::cancel`] and the
    /// loop's check-and-submit section hold this lock, so a pass is either
    /// submitted before the signal or never started.
    gate: Arc<Mutex<()>>,
}

impl RetentionTask {
    /// Starts the periodic task.
    ///
    /// `first_check` is the delay before the first gate check: the composition
    /// root passes [`RetentionSchedule::failure_retry`] when the startup
    /// catch-up failed or was contended, and
    /// [`RetentionSchedule::check_interval`] otherwise.
    #[must_use]
    pub fn spawn(
        pass: RetentionPass,
        schedule: RetentionSchedule,
        report: RetentionReport,
        first_check: Duration,
    ) -> Self {
        let (cancel, cancelled) = watch::channel(false);
        let gate = Arc::new(Mutex::new(()));
        let loop_gate = Arc::clone(&gate);
        let join = tokio::spawn(async move {
            run_periodic(pass, schedule, report, first_check, cancelled, loop_gate).await;
        });
        Self { join, cancel, gate }
    }

    /// Starts the periodic task behind a startup catch-up that races the
    /// shutdown signal.
    ///
    /// `admitted` resolves to `true` once the catch-up completed and the
    /// periodic loop is about to take over, and to `false` when shutdown won
    /// the race (the composition root then skips `serve` entirely). In both
    /// cases the in-flight pass stays owned by the task, so the terminal
    /// join still covers it.
    pub fn spawn_with_startup(
        pass: RetentionPass,
        schedule: RetentionSchedule,
        report: RetentionReport,
        shutdown: ShutdownToken,
        admitted: oneshot::Sender<bool>,
    ) -> Self {
        let (cancel, cancelled) = watch::channel(false);
        let gate = Arc::new(Mutex::new(()));
        let loop_gate = Arc::clone(&gate);
        let join = tokio::spawn(async move {
            run_with_startup(
                pass, schedule, report, cancelled, loop_gate, shutdown, admitted,
            )
            .await;
        });
        Self { join, cancel, gate }
    }

    /// Requests cancellation, serialized with pass submission.
    ///
    /// The task stops scheduling new passes; an in-flight `spawn_blocking`
    /// pass still runs to completion, because a blocking closure cannot be
    /// cancelled. The gate serializes this transition with the loop's
    /// check-and-submit section: once this returns, no further pass can be
    /// submitted - a pass is either submitted before the signal or never
    /// started (section 11.5's cancellation contract).
    pub async fn cancel(&self) {
        let _serialized = self.gate.lock().await;
        let _ = self.cancel.send(true);
    }

    /// Returns whether the task has terminated.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// Cancels and joins the task, sharing `stop` with the other runtime
    /// drains.
    ///
    /// The composition root runs the retention terminal join concurrently
    /// with the worker drains under one deadline, so the whole shutdown
    /// stays inside the deployment stop budget instead of stacking two full
    /// join budgets back to back. A `stop` that has already elapsed reports
    /// `false` immediately.
    pub async fn shutdown_by(self, stop: tokio::time::Instant) -> bool {
        self.cancel().await;
        tokio::time::timeout_at(stop, self.join).await.is_ok()
    }

    /// Cancels and joins the task within `timeout`.
    ///
    /// Returns `true` when the task terminated. Because the task always
    /// awaits its in-flight `spawn_blocking` pass before returning, a `true`
    /// result also means that pass finished - the shutdown final pass can
    /// then run without ever sharing the store mutex with residual
    /// maintenance.
    ///
    /// Returns `false` when the join times out (a pathological pass stuck on
    /// the filesystem); the caller must then skip the final pass rather than
    /// contend with the orphaned work.
    ///
    /// # Panics
    ///
    /// Never panics: a top-level task panic also counts as terminated.
    pub async fn shutdown_within(self, timeout: Duration) -> bool {
        self.shutdown_by(tokio::time::Instant::now() + timeout)
            .await
    }

    /// Cancels and joins the task within [`RETENTION_JOIN_TIMEOUT`].
    pub async fn shutdown(self) -> bool {
        self.shutdown_within(RETENTION_JOIN_TIMEOUT).await
    }
}

/// Starts the retention lifecycle: the startup catch-up, then the periodic
/// task.
///
/// The composition root awaits this before admitting requests. The task's
/// first gate check is scheduled after [`RetentionSchedule::failure_retry`]
/// when the catch-up did not complete, and after
/// [`RetentionSchedule::check_interval`] otherwise.
#[must_use]
pub async fn start(
    pass: RetentionPass,
    schedule: RetentionSchedule,
    report: RetentionReport,
) -> RetentionTask {
    let first_check = match run_startup_catchup(&pass, &report).await {
        MaintenanceOutcome::Failed(_) | MaintenanceOutcome::Contended => schedule.failure_retry,
        MaintenanceOutcome::Ran | MaintenanceOutcome::NotDue => schedule.check_interval,
    };
    RetentionTask::spawn(pass, schedule, report, first_check)
}

/// Starts the retention lifecycle with the startup catch-up racing the
/// shutdown signal.
///
/// Returns the task handle plus whether admission may begin. A stop signal
/// during the catch-up does not abandon the in-flight pass: the task awaits
/// it to the end (a `spawn_blocking` pass cannot be cancelled, and detaching
/// it would leave the final pass able to contend with it for the store
/// mutex), reports the abandonment, and ends without ever scheduling the
/// periodic loop.
pub async fn start_with_shutdown(
    pass: RetentionPass,
    schedule: RetentionSchedule,
    report: RetentionReport,
    shutdown: ShutdownToken,
) -> (RetentionTask, bool) {
    let (admitted_tx, admitted_rx) = oneshot::channel::<bool>();
    let task = RetentionTask::spawn_with_startup(pass, schedule, report, shutdown, admitted_tx);
    let admitted = admitted_rx.await.unwrap_or(false);
    (task, admitted)
}

/// Runs one gated pass before UDS admission begins.
///
/// The first pass after a hard kill can prune a large historical backlog, and
/// the daemon's request audit writes share the store's connection mutex, so
/// the composition root awaits this before calling `serve`. A failed,
/// contended, or panicked catch-up does not block admission: the periodic
/// task retries it after [`RetentionSchedule::failure_retry`].
pub async fn run_startup_catchup(
    pass: &RetentionPass,
    report: &RetentionReport,
) -> MaintenanceOutcome {
    let pass = Arc::clone(pass);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |delta| delta.as_secs_f64());
    match tokio::task::spawn_blocking(move || pass(now)).await {
        Ok(MaintenanceOutcome::Failed(error)) => {
            report(&format!(
                "agent-sec-daemon: security event sqlite retention catch-up failed, retrying: {error}"
            ));
            MaintenanceOutcome::Failed(error)
        }
        Ok(MaintenanceOutcome::Contended) => {
            report(
                "agent-sec-daemon: security event sqlite retention catch-up found the gate lock held, retrying",
            );
            MaintenanceOutcome::Contended
        }
        Ok(MaintenanceOutcome::Ran) => {
            report("agent-sec-daemon: security event sqlite retention catch-up pass completed");
            MaintenanceOutcome::Ran
        }
        Ok(MaintenanceOutcome::NotDue) => MaintenanceOutcome::NotDue,
        Err(problem) => {
            report(&format!(
                "agent-sec-daemon: security event sqlite retention catch-up panicked: {problem}"
            ));
            MaintenanceOutcome::Failed(format!("startup catch-up panicked: {problem}"))
        }
    }
}

/// The startup phase of [`RetentionTask::spawn_with_startup`]: the catch-up
/// pass races the shutdown signal, then either hands over to the periodic
/// loop or terminates after owning the abandoned pass to its end.
async fn run_with_startup(
    pass: RetentionPass,
    schedule: RetentionSchedule,
    report: RetentionReport,
    cancelled: watch::Receiver<bool>,
    gate: Arc<Mutex<()>>,
    shutdown: ShutdownToken,
    admitted: oneshot::Sender<bool>,
) {
    // The catch-up runs as a child task so a shutdown signal cannot detach
    // it: on cancellation its `JoinHandle` is still awaited below, keeping
    // the pass owned through the terminal join.
    let catchup_pass = Arc::clone(&pass);
    let catchup_report = Arc::clone(&report);
    let mut catchup =
        tokio::spawn(async move { run_startup_catchup(&catchup_pass, &catchup_report).await });
    let outcome = tokio::select! {
        outcome = &mut catchup => Some(outcome.expect("startup catch-up task")),
        () = shutdown.cancelled() => None,
    };
    let first_check = match outcome {
        Some(MaintenanceOutcome::Failed(_) | MaintenanceOutcome::Contended) => {
            schedule.failure_retry
        }
        Some(MaintenanceOutcome::Ran | MaintenanceOutcome::NotDue) => schedule.check_interval,
        None => {
            report(
                "agent-sec-daemon: security event sqlite retention catch-up abandoned: shutdown requested",
            );
            // Own the in-flight pass through to its end: dropping the handle
            // here would detach it from the terminal join, and the final
            // pass could then contend with it for the store mutex.
            if let Err(problem) = (&mut catchup).await {
                report(&format!(
                    "agent-sec-daemon: security event sqlite retention catch-up panicked: {problem}"
                ));
            }
            let _ = admitted.send(false);
            return;
        }
    };
    let _ = admitted.send(true);
    run_periodic(pass, schedule, report, first_check, cancelled, gate).await;
}

async fn run_periodic(
    pass: RetentionPass,
    schedule: RetentionSchedule,
    report: RetentionReport,
    first_check: Duration,
    mut cancelled: watch::Receiver<bool>,
    gate: Arc<Mutex<()>>,
) {
    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + first_check,
        schedule.check_interval,
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = cancelled.changed() => break,
            _ = ticker.tick() => {}
        }
        // `biased` fixes the poll order, not the state: the tick arm can
        // still be the one that runs when both wakeups became ready
        // together (or the signal arrived between the poll and the new
        // pass). The flag is therefore re-confirmed before any new pass is
        // scheduled; the authoritative re-confirmation happens under the
        // gate below, which also closes the check-to-spawn window - a
        // cancellation that arrives between the check and the submission
        // still prevents that pass (section 11.5's cancellation contract).
        if *cancelled.borrow() {
            break;
        }
        loop {
            // The gate serializes this check-and-submit section with
            // `RetentionTask::cancel`: the cancellation transition cannot
            // interleave between the re-confirmation and the
            // `spawn_blocking` submission, so once the signal has been sent
            // no new pass may start. The lock is asynchronous - waiting for
            // it parks the task, never a runtime worker - and the guard
            // drops before the await below, so it is never held across an
            // await point.
            let attempt = {
                let _serialized = gate.lock().await;
                if *cancelled.borrow() {
                    return;
                }
                let pass = Arc::clone(&pass);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |delta| delta.as_secs_f64());
                tokio::task::spawn_blocking(move || pass(now))
            }
            .await;
            match attempt {
                Ok(MaintenanceOutcome::Failed(error)) => {
                    report(&format!(
                        "agent-sec-daemon: security event sqlite retention failed, retrying: {error}"
                    ));
                    if sleep_or_cancel(&mut cancelled, schedule.failure_retry).await {
                        return;
                    }
                }
                Ok(MaintenanceOutcome::Contended) => {
                    report(
                        "agent-sec-daemon: security event sqlite retention lock is held by another process, retrying",
                    );
                    if sleep_or_cancel(&mut cancelled, schedule.failure_retry).await {
                        return;
                    }
                }
                Ok(MaintenanceOutcome::Ran) => {
                    report("agent-sec-daemon: security event sqlite retention pass completed");
                    break;
                }
                Ok(MaintenanceOutcome::NotDue) => break,
                Err(problem) => {
                    report(&format!(
                        "agent-sec-daemon: security event sqlite retention task panicked: {problem}"
                    ));
                    // A panic is a bug, not a retryable failure: the next
                    // attempt waits for the regular cadence instead of
                    // hammering a broken pass every backoff.
                    break;
                }
            }
        }
    }
}

/// Sleeps for `delay`, returning `true` when cancellation won the race.
///
/// The cancellation arm is polled first and the flag re-confirmed after the
/// sleep, so a signalled cancellation is never outrun by the retry timer.
async fn sleep_or_cancel(cancelled: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        biased;
        _ = cancelled.changed() => true,
        () = tokio::time::sleep(delay) => *cancelled.borrow(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as ChannelMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn counting_pass(
        calls: &Arc<AtomicUsize>,
    ) -> (RetentionPass, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let entered = ChannelMutex::new(entered_tx);
        let release = ChannelMutex::new(release_rx);
        let counted = Arc::clone(calls);
        let pass: RetentionPass = Arc::new(move |_now: f64| {
            counted.fetch_add(1, Ordering::SeqCst);
            entered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send(())
                .expect("pass entered");
            release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(10))
                .expect("release");
            MaintenanceOutcome::Ran
        });
        (pass, entered_rx, release_tx)
    }

    /// A pass that returns immediately, counting its calls.
    fn immediate_pass(calls: &Arc<AtomicUsize>) -> RetentionPass {
        let counted = Arc::clone(calls);
        Arc::new(move |_now: f64| {
            counted.fetch_add(1, Ordering::SeqCst);
            MaintenanceOutcome::NotDue
        })
    }

    fn quiet_report() -> RetentionReport {
        Arc::new(|_message: &str| {})
    }

    // DJOB-RET-014: a shutdown signal that arrives while the startup
    // catch-up pass is still running abandons admission (the composition
    // root never calls `serve`), and the in-flight pass stays owned: while
    // the pass is still running the task has not terminated and the
    // admission verdict has not been delivered, and once the pass completes
    // the verdict is `false` and the terminal join succeeds - the final
    // pass can never contend with the abandoned pass.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shutdown_during_the_startup_catchup_abandons_admission_and_joins_the_pass() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (pass, entered_rx, release_tx) = counting_pass(&calls);
        let shutdown = ShutdownToken::new();
        let (admitted_tx, mut admitted_rx) = oneshot::channel::<bool>();
        let task = RetentionTask::spawn_with_startup(
            pass,
            RetentionSchedule::default(),
            quiet_report(),
            shutdown.clone(),
            admitted_tx,
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("catch-up pass entered");

        shutdown.request();

        // The pass is still in flight, so the task still owns it: no verdict
        // has been delivered and the task has not terminated.
        assert!(
            admitted_rx.try_recv().is_err(),
            "no admission verdict while the abandoned pass is still running"
        );
        assert!(
            !task.is_finished(),
            "the task still owns the in-flight pass"
        );

        release_tx.send(()).expect("release the pass");
        let admitted = admitted_rx.await.expect("admission verdict");
        assert!(
            !admitted,
            "shutdown won the catch-up race: admission must be abandoned"
        );
        let joined = task.shutdown_within(Duration::from_secs(2)).await;
        assert!(joined, "the terminal join covers the abandoned pass");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // DJOB-RET-010: a cancellation that arrives while the loop sits between
    // its cancellation check and its `spawn_blocking` submission - the
    // check-to-spawn interleave of section 11.5 - still prevents that pass.
    // The gate parks the attempt at the start of its submission section,
    // which widens the previously unobservable window into a state the
    // fixture can aim at: the raw `send` below lands while the attempt is
    // parked, after the outer check has already returned `false`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_signal_inside_the_submission_window_still_prevents_the_pass() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (pass, entered_rx, release_tx) = counting_pass(&calls);
        let task = RetentionTask::spawn(
            pass,
            RetentionSchedule::new(Duration::from_millis(20), Duration::from_millis(20)),
            quiet_report(),
            Duration::from_millis(10),
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the first pass must start");

        // The loop now awaits the in-flight pass, so the gate is free. Hold
        // it: once the released pass returns and the cadence ticks, the next
        // attempt parks at the start of its submission section - past its
        // outer check, before its submission. Waiting for the asynchronous
        // gate parks the task; no runtime worker is blocked.
        let parked = task.gate.lock().await;
        release_tx.send(()).expect("release the first pass");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the parked attempt must not have submitted a second pass"
        );

        // The adversarial injection: the signal is delivered while the
        // attempt is parked between its check and its submission. Production
        // `cancel` cannot land here (it takes the same gate); the raw send
        // is exactly the arrival the in-gate re-confirmation must survive.
        let _ = task.cancel.send(true);
        drop(parked);

        let joined = tokio::time::timeout(Duration::from_secs(2), task.join).await;
        assert!(joined.is_ok(), "the task must terminate after the signal");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a signal inside the submission window must still prevent the pass"
        );
    }

    // DJOB-RET-011: `cancel` itself takes the gate, so the cancellation
    // transition can never interleave with a submission attempt. While the
    // section holds the gate, `cancel` must not complete.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_serializes_with_the_submission_section() {
        let calls = Arc::new(AtomicUsize::new(0));
        let task = RetentionTask::spawn(
            immediate_pass(&calls),
            RetentionSchedule::new(Duration::from_millis(20), Duration::from_millis(20)),
            quiet_report(),
            Duration::from_millis(10),
        );
        // Let the first pass run so the loop is live, then hold the gate on
        // the submission section's behalf across an await.
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the first pass must run");
        let gate = Arc::clone(&task.gate);
        let parked = gate.lock().await;
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let canceller = tokio::spawn(async move {
            task.cancel().await;
            done_tx.send(()).expect("done signal");
            task
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            done_rx.try_recv().is_err(),
            "cancel must not complete while the submission section holds the gate"
        );

        drop(parked);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cancel must complete once the gate is free");
        let task = canceller.await.expect("canceller joins");

        // Whichever side won the gate, the invariant after `cancel` returned
        // is the same: a pass submitted afterwards would have to pass the
        // in-gate re-confirmation against the already-sent signal.
        let settled = calls.load(Ordering::SeqCst);
        let joined = tokio::time::timeout(Duration::from_secs(2), task.join).await;
        assert!(joined.is_ok(), "the task must terminate after cancellation");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            settled,
            "no pass may be scheduled once cancellation has been signalled"
        );
    }

    // DJOB-RET-012: `shutdown_by` enforces the shared stop deadline. When the
    // concurrent runtime drains have already consumed the deadline, the
    // retention join reports `false` immediately instead of waiting out a
    // second full budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stuck_pass_misses_a_shared_deadline_promptly() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (pass, entered_rx, release_tx) = counting_pass(&calls);
        let task = RetentionTask::spawn(
            pass,
            RetentionSchedule::new(Duration::from_secs(30), Duration::from_millis(20)),
            quiet_report(),
            Duration::from_millis(10),
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the periodic pass must start");

        let started = std::time::Instant::now();
        let stop = tokio::time::Instant::now();
        assert!(
            !task.shutdown_by(stop).await,
            "a stuck pass must miss an already-elapsed shared deadline"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a missed deadline must be reported without waiting a second budget"
        );

        release_tx.send(()).expect("unblock the detached pass");
    }
}
