//! Production lifecycle fixtures for the daemon's security-event `SQLite`
//! retention service - `DAEMON_JOB_CONTRACT_zh.md` section 11.5 (DJOB-RET-*).
//!
//! The fixtures drive the real `agent-sec-daemon` binary against a real
//! security-events database seeded with a hard-kill backlog, exactly the
//! deployment shape the periodic service exists for.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use asc_daemon::retention::{
    RetentionPass, RetentionReport, RetentionSchedule, RetentionTask, start,
};
use asc_event_sink::MaintenanceOutcome;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

/// One scripted step of a lifecycle fixture's retention pass.
enum Step {
    /// The pass returns this outcome.
    Outcome(MaintenanceOutcome),
    /// The pass panics inside `spawn_blocking`.
    Panic,
}

/// A scripted pass: returns the queued steps in order, then idles with `NotDue`.
struct Script {
    steps: Mutex<std::collections::VecDeque<Step>>,
    calls: AtomicUsize,
}

impl Script {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn pass(self: &Arc<Self>) -> RetentionPass {
        let script = Arc::clone(self);
        Arc::new(move |_now: f64| {
            script.calls.fetch_add(1, Ordering::SeqCst);
            // The step leaves the lock before the match: a scripted panic
            // must not poison the shared script mutex.
            let step = script
                .steps
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front();
            match step {
                Some(Step::Outcome(outcome)) => outcome,
                Some(Step::Panic) => panic!("scripted retention pass panic"),
                None => MaintenanceOutcome::NotDue,
            }
        })
    }
}

/// Collects the lifecycle diagnostics a fixture's reporter receives.
#[derive(Default)]
struct Diagnostics {
    lines: Mutex<Vec<String>>,
}

impl Diagnostics {
    fn reporter(self: &Arc<Self>) -> RetentionReport {
        let diagnostics = Arc::clone(self);
        Arc::new(move |message: &str| {
            diagnostics.lines.lock().unwrap().push(message.to_owned());
        })
    }

    fn joined(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }
}

/// Waits until the script has recorded `expected` pass calls.
async fn await_calls(script: &Script, expected: usize, budget: Duration) {
    tokio::time::timeout(budget, async {
        while script.calls() < expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {expected} retention pass calls within {budget:?}, saw {}",
            script.calls()
        )
    });
}

// DJOB-RET-001: the startup catch-up runs exactly one pass immediately, then
// the periodic task follows the regular cadence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_catch_up_runs_once_then_follows_the_cadence() {
    let script = Script::new(vec![
        Step::Outcome(MaintenanceOutcome::Ran),
        Step::Outcome(MaintenanceOutcome::Ran),
    ]);
    let diagnostics = Arc::new(Diagnostics::default());
    let task = start(
        script.pass(),
        RetentionSchedule::new(Duration::from_millis(120), Duration::from_millis(40)),
        diagnostics.reporter(),
    )
    .await;

    assert_eq!(
        script.calls(),
        1,
        "the startup catch-up must run exactly one pass before start returns"
    );
    assert!(
        diagnostics
            .joined()
            .contains("retention catch-up pass completed")
    );

    await_calls(&script, 2, Duration::from_secs(2)).await;
    assert!(diagnostics.joined().contains("retention pass completed"));
    assert!(task.shutdown().await);
}

// DJOB-RET-002: a failed catch-up does not block admission (start still
// returns); the retry is scheduled on the failure backoff, not the cadence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_catch_up_schedules_the_backoff_retry() {
    let script = Script::new(vec![
        Step::Outcome(MaintenanceOutcome::Failed("disk full".to_owned())),
        Step::Outcome(MaintenanceOutcome::Ran),
    ]);
    let diagnostics = Arc::new(Diagnostics::default());
    let task = start(
        script.pass(),
        RetentionSchedule::new(Duration::from_secs(10), Duration::from_millis(40)),
        diagnostics.reporter(),
    )
    .await;

    assert_eq!(script.calls(), 1, "the catch-up itself ran and failed");
    assert!(
        diagnostics
            .joined()
            .contains("retention catch-up failed, retrying: disk full")
    );

    // The retry must arrive on the 40ms backoff, well before the 10s cadence.
    await_calls(&script, 2, Duration::from_secs(2)).await;
    // Join before asserting: the awaited call has only *entered* at this
    // point, and its `Ran` report is emitted after it returns.
    assert!(task.shutdown().await);
    assert!(diagnostics.joined().contains("retention pass completed"));
}

// DJOB-RET-003: a failed periodic pass retries after the backoff and reports
// every failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_pass_retries_after_the_backoff() {
    let script = Script::new(vec![
        Step::Outcome(MaintenanceOutcome::NotDue),
        Step::Outcome(MaintenanceOutcome::Failed("prune boom".to_owned())),
        Step::Outcome(MaintenanceOutcome::Failed("prune boom".to_owned())),
        Step::Outcome(MaintenanceOutcome::Ran),
    ]);
    let diagnostics = Arc::new(Diagnostics::default());
    let task = start(
        script.pass(),
        RetentionSchedule::new(Duration::from_millis(40), Duration::from_millis(30)),
        diagnostics.reporter(),
    )
    .await;

    await_calls(&script, 4, Duration::from_secs(2)).await;
    // Join before asserting: the awaited call has only *entered* at this
    // point, and its report is emitted after it returns.
    assert!(task.shutdown().await);
    let joined = diagnostics.joined();
    assert_eq!(
        joined
            .matches("retention failed, retrying: prune boom")
            .count(),
        2,
        "{joined}"
    );
    assert!(joined.contains("retention pass completed"), "{joined}");
}

// DJOB-RET-004: a contended pass is reported and retried on the backoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_contended_pass_retries_after_the_backoff() {
    let script = Script::new(vec![
        Step::Outcome(MaintenanceOutcome::NotDue),
        Step::Outcome(MaintenanceOutcome::Contended),
        Step::Outcome(MaintenanceOutcome::Ran),
    ]);
    let diagnostics = Arc::new(Diagnostics::default());
    let task = start(
        script.pass(),
        RetentionSchedule::new(Duration::from_millis(40), Duration::from_millis(30)),
        diagnostics.reporter(),
    )
    .await;

    await_calls(&script, 3, Duration::from_secs(2)).await;
    // Join before asserting: the awaited call has only *entered* at this
    // point, and its report is emitted after it returns.
    assert!(task.shutdown().await);
    let joined = diagnostics.joined();
    assert!(
        joined.contains("retention lock is held by another process, retrying"),
        "{joined}"
    );
    assert!(joined.contains("retention pass completed"), "{joined}");
}

// DJOB-RET-005: a panicking pass is reported once, the task survives, and the
// next attempt waits for the regular cadence instead of the backoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_pass_is_reported_and_the_task_survives() {
    let script = Script::new(vec![
        Step::Outcome(MaintenanceOutcome::NotDue),
        Step::Panic,
        Step::Outcome(MaintenanceOutcome::Ran),
    ]);
    let diagnostics = Arc::new(Diagnostics::default());
    let task = start(
        script.pass(),
        RetentionSchedule::new(Duration::from_millis(60), Duration::from_millis(20)),
        diagnostics.reporter(),
    )
    .await;

    // Wait for the panic to be reported.
    tokio::time::timeout(Duration::from_secs(2), async {
        while !diagnostics.joined().contains("retention task panicked") {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the panic must be reported");
    assert!(
        !task.is_finished(),
        "a panicking pass must not kill the retention task"
    );

    await_calls(&script, 3, Duration::from_secs(2)).await;
    // Join before asserting: the awaited call has only *entered* at this
    // point, and its report is emitted after it returns.
    assert!(task.shutdown().await);
    assert!(
        diagnostics.joined().contains("retention pass completed"),
        "{}",
        diagnostics.joined()
    );
}

// DJOB-RET-007: shutdown cancels scheduling, then joins the task - and with it
// the in-flight `spawn_blocking` pass. It must not return while the pass is
// still running, so the final pass can never contend with residual work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_joins_an_in_flight_pass_before_returning() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let entered = Mutex::new(entered_tx);
    let release = Mutex::new(release_rx);
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let exited_flag = Arc::clone(&exited);
    let pass: RetentionPass = Arc::new(move |_now: f64| {
        entered.lock().unwrap().send(()).expect("pass entered");
        release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .expect("release");
        exited_flag.store(true, Ordering::Release);
        MaintenanceOutcome::Ran
    });
    let diagnostics = Arc::new(Diagnostics::default());
    let task = RetentionTask::spawn(
        pass,
        RetentionSchedule::new(Duration::from_secs(30), Duration::from_millis(20)),
        diagnostics.reporter(),
        Duration::from_millis(10),
    );
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the periodic pass must start");

    let shutdown = task.shutdown();
    tokio::pin!(shutdown);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut shutdown)
            .await
            .is_err(),
        "shutdown must wait for the in-flight pass, not abandon it"
    );
    assert!(
        !exited.load(Ordering::Acquire),
        "the pass must still be running while shutdown waits"
    );

    release_tx.send(()).expect("release the pass");
    assert!(
        shutdown.await,
        "shutdown must report termination once the pass exits"
    );
    assert!(
        exited.load(Ordering::Acquire),
        "the in-flight pass must have terminated before shutdown returned"
    );
}

// DJOB-RET-008: a pass stuck past the join budget times the terminal join out,
// telling the caller to skip the final pass rather than contend.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stuck_pass_times_out_the_terminal_join() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let entered = Mutex::new(entered_tx);
    let release = Mutex::new(release_rx);
    let pass: RetentionPass = Arc::new(move |_now: f64| {
        entered.lock().unwrap().send(()).expect("pass entered");
        release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .expect("release");
        MaintenanceOutcome::Ran
    });
    let diagnostics = Arc::new(Diagnostics::default());
    let task = RetentionTask::spawn(
        pass,
        RetentionSchedule::new(Duration::from_secs(30), Duration::from_millis(20)),
        diagnostics.reporter(),
        Duration::from_millis(10),
    );
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the periodic pass must start");

    assert!(
        !task.shutdown_within(Duration::from_millis(100)).await,
        "a stuck pass must time the terminal join out"
    );

    release_tx.send(()).expect("unblock the detached pass");
}

// DJOB-RET-009: when the tick and the cancellation flag are ready in the
// same poll - the interleave where an unbiased `select!` may legally pick
// the tick - no new `spawn_blocking` pass is scheduled. Cancellation has
// priority before any new work, per section 11.5's cancellation contract:
// after the signal, a new pass would extend shutdown and eat the 65s
// terminal-join budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tick_and_cancellation_ready_together_schedule_no_new_pass() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let entered = Mutex::new(entered_tx);
    let release = Mutex::new(release_rx);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let pass: RetentionPass = Arc::new(move |_now: f64| {
        counted.fetch_add(1, Ordering::SeqCst);
        entered.lock().unwrap().send(()).expect("pass entered");
        release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .expect("release");
        MaintenanceOutcome::Ran
    });
    let diagnostics = Arc::new(Diagnostics::default());
    // The cadence is short enough that the next tick is due while the first
    // pass is still blocking, so both wakeups are pending together.
    let task = RetentionTask::spawn(
        pass,
        RetentionSchedule::new(Duration::from_millis(50), Duration::from_millis(20)),
        diagnostics.reporter(),
        Duration::from_millis(10),
    );
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the first pass must start");

    // Signal cancellation, then let the cadence tick elapse: when the
    // in-flight pass returns, the scheduler faces a tick and a cancellation
    // that are ready simultaneously.
    task.cancel().await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    release_tx.send(()).expect("release the first pass");

    let shutdown = task.shutdown();
    tokio::pin!(shutdown);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), &mut shutdown)
            .await
            .is_ok(),
        "shutdown must not wait for a second pass that must never start"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no new pass may be scheduled once cancellation is signalled, even with a tick ready"
    );
}

// Readiness includes the startup retention catch-up over the seeded backlog.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
// Match the deployment stop budget, including drain, persistence, and runtime cleanup.
const EXIT_TIMEOUT: Duration = Duration::from_secs(45);

/// Expired rows in the seeded backlog. Large enough that the prune pass is
/// measurably slower than socket admission, which is what the ordering
/// fixture asserts.
const BACKLOG_ROWS: usize = 60_000;

struct RunningBinary {
    child: Child,
    directory: PathBuf,
    socket_path: PathBuf,
}

impl Drop for RunningBinary {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn create_runtime_directory() -> PathBuf {
    // Ignore TMPDIR: runner-owned ancestors may not satisfy the daemon contract.
    let directory = Path::new("/tmp").join(format!(
        "asc-daemon-retention-{}-{}",
        std::process::id(),
        DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    directory
}

fn configured_command(directory: &Path) -> Command {
    // Process tests never open the machine's production SkillSec key store or configuration.
    let config = directory.join("skillsec.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "stateDir": directory.join("state")
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-sec-daemon"));
    command.arg("--skillsec-config").arg(config);
    command
}

fn stderr_log(directory: &Path) -> Stdio {
    std::fs::File::create(directory.join("stderr.log"))
        .unwrap()
        .into()
}

fn read_stderr(directory: &Path) -> String {
    std::fs::read_to_string(directory.join("stderr.log")).unwrap()
}

async fn rejected_without_root(running: &mut RunningBinary) -> bool {
    if rustix::process::geteuid().as_raw() == 0 {
        return false;
    }
    assert!(!wait_for_exit(running).await.success());
    let stderr = read_stderr(&running.directory);
    assert!(
        stderr.contains("SkillSec system daemon must run as root"),
        "{stderr}"
    );
    assert!(!running.socket_path.exists());
    true
}

/// Starts the daemon against `data_dir`; the caller owns the `RunningBinary`.
fn start_daemon(directory: &Path, data_dir: &Path) -> RunningBinary {
    let socket_path = directory.join("daemon.sock");
    let child = configured_command(directory)
        .env("AGENT_SEC_DATA_DIR", data_dir)
        .args(["serve", "--socket"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_log(directory))
        .spawn()
        .unwrap();
    RunningBinary {
        child,
        directory: directory.to_path_buf(),
        socket_path,
    }
}

async fn wait_for_socket(running: &mut RunningBinary) {
    let started = Instant::now();
    let result = tokio::time::timeout(STARTUP_TIMEOUT, async {
        loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                panic!(
                    "daemon exited before accepting connections ({status}): {}",
                    read_stderr(&running.directory)
                );
            }
            match UnixStream::connect(&running.socket_path).await {
                Ok(stream) => {
                    drop(stream);
                    return;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => {
                    panic!(
                        "daemon bootstrap connection failed: {error}; stderr: {}",
                        read_stderr(&running.directory)
                    );
                }
            }
        }
    })
    .await;
    if result.is_err() {
        let elapsed = started.elapsed();
        let _ = running.child.kill();
        let status = running.child.wait().unwrap();
        panic!(
            "daemon bootstrap timed out after {elapsed:?}; pid: {}; socket: {}; status after cleanup: {status}; stderr: {}",
            running.child.id(),
            running.socket_path.display(),
            read_stderr(&running.directory)
        );
    }
}

async fn wait_for_exit(running: &mut RunningBinary) -> std::process::ExitStatus {
    tokio::time::timeout(EXIT_TIMEOUT, async {
        loop {
            if let Some(status) = running.child.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let _ = running.child.kill();
        let status = running.child.wait().unwrap();
        panic!(
            "daemon exit timed out; pid: {}; status after cleanup: {status}; stderr: {}",
            running.child.id(),
            read_stderr(&running.directory)
        );
    })
}

async fn request(path: &Path, payload: &[u8]) -> Value {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream.write_all(payload).await.unwrap();
    let mut response = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut response)
        .await
        .unwrap();
    assert_eq!(response.pop(), Some(b'\n'));
    serde_json::from_slice(&response).unwrap()
}

fn marker_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".maintenance");
    PathBuf::from(name)
}

/// Seeds the security-events database with a hard-kill backlog.
///
/// The schema comes from the production writer; the bulk of the rows is
/// inserted in one transaction so the fixture itself stays fast. Every backlog
/// row carries a 2020 timestamp, far outside the 30-day retention window.
fn seed_backlog(db: &Path) {
    let writer =
        asc_persistence_sqlite::security_events::SqliteEventWriter::new(db).expect("writer");
    writer.probe().expect("probe");
    drop(writer);

    let mut conn = rusqlite::Connection::open(db).expect("open seeded db");
    conn.pragma_update(None, "busy_timeout", 200).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let tx = conn.transaction().expect("seed transaction");
    {
        let mut expired = tx
            .prepare(
                "INSERT INTO security_events
                    (event_id, event_type, category, timestamp, timestamp_epoch, pid, uid, details)
                 VALUES (?1, 'code_scan', 'code_scan', '2020-01-01T00:00:00Z', 1577836800.0, 1, 0, ?2)",
            )
            .expect("prepare expired");
        let payload = "x".repeat(2048);
        for index in 0..BACKLOG_ROWS {
            expired
                .execute(rusqlite::params![
                    format!("backlog-{index}"),
                    payload.as_str()
                ])
                .expect("insert backlog row");
        }
        let mut fresh = tx
            .prepare(
                "INSERT INTO security_events
                    (event_id, event_type, category, timestamp, timestamp_epoch, pid, uid, details)
                 VALUES (?1, 'code_scan', 'code_scan', '2026-01-01T00:00:00Z', ?2, 1, 0, '{}')",
            )
            .expect("prepare fresh");
        for index in 0..2 {
            fresh
                .execute(rusqlite::params![format!("fresh-{index}"), now,])
                .expect("insert fresh row");
        }
    }
    tx.commit().expect("commit seed");
}

fn row_count(db: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.query_row("SELECT COUNT(*) FROM security_events", [], |row| row.get(0))
        .expect("count")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_catch_up_prunes_the_backlog_before_admission() {
    let directory = create_runtime_directory();
    let data_dir = directory.join("data");
    std::fs::create_dir(&data_dir).unwrap();
    // The policy-state lease (main's SqlitePolicyRepository) requires an
    // owner-private data directory (0700); plain `create_dir` inherits the
    // umask instead.
    std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = data_dir.join("security-events.db");
    seed_backlog(&db);
    assert_eq!(row_count(&db), i64::try_from(BACKLOG_ROWS).unwrap() + 2);
    assert!(!marker_path(&db).exists());

    let mut running = start_daemon(&directory, &data_dir);
    if rejected_without_root(&mut running).await {
        return;
    }

    // DJOB-RET-006: admission starts only after the startup catch-up pass, so
    // the first successful connect already observes the pruned, marked store.
    wait_for_socket(&mut running).await;
    let marker = marker_path(&db);
    assert!(
        marker.exists(),
        "the startup catch-up pass must complete before the socket accepts connections"
    );
    assert_eq!(
        row_count(&db),
        2,
        "the hard-kill backlog must be pruned before admission"
    );
    let marker_time: f64 = std::fs::read_to_string(&marker)
        .expect("marker")
        .trim()
        .parse()
        .expect("marker timestamp");
    assert!(marker_time > 1_700_000_000.0, "the marker must be fresh");

    // Request audit writes proceed against the post-catch-up store.
    let scan = request(
        &running.socket_path,
        b"{\"method\":\"action.code_scan\",\"params\":{\"code\":\"echo post-catchup\",\"language\":\"bash\",\"mode\":\"regex\"}}\n",
    )
    .await;
    assert_eq!(scan["result"]["verdict"], "pass");

    // DJOB-RET-007: graceful shutdown terminates the retention task before the
    // bounded final pass; exit stays clean.
    let signal = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(running.child.id().to_string())
        .status()
        .unwrap();
    assert!(signal.success());
    let status = wait_for_exit(&mut running).await;
    assert!(
        status.success(),
        "stderr: {}",
        read_stderr(&running.directory)
    );
    assert!(!running.socket_path.exists());
}
