//! Startup reconcile retry integration tests.
//!
//! Regression coverage for the startup race between SkillFS and sec-core:
//! SkillFS mounts, enumerates its skills, and queues one reconcile per
//! skill *before* the daemon has necessarily created its notify socket.
//! The reconcile delivery must survive that window, because nothing else
//! can recover it — the activation watcher only consumes activation state
//! the daemon has already written, so a skill the daemon has never scanned
//! stays hidden indefinitely until some reconcile actually lands.
//!
//! These tests drive the real `UnixSocketNotifyClient` against a socket
//! that appears late or is recreated mid-flight, and the real
//! `ActivationReloadController` for the hidden-to-visible transition.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use skillfs_fuse::security::{
    ActivationReloadController, ActiveSkillResolver, ActiveTarget, CapturedNotify,
    InMemoryProtocolEventWriter, NotifyChangeEvent, NotifyClient, NotifyController, NotifyError,
    NotifyEventKind, UnixSocketNotifyClient,
};

const ACK: &[u8] = br#"{"ok":true,"data":{"schemaVersion":2,"accepted":true}}"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Accept exactly `count` notify requests, ACK each, and return the
/// `skillId` values seen. Runs on the caller's thread.
fn serve_acks(listener: &UnixListener, count: usize) -> Vec<String> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    listener
        .set_nonblocking(true)
        .expect("set notify listener nonblocking");
    while seen.len() < count && Instant::now() < deadline {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(error) => panic!("accept notify connection: {error}"),
        };
        let mut request = String::new();
        BufReader::new(&stream)
            .read_line(&mut request)
            .expect("read notify request");
        let parsed: serde_json::Value =
            serde_json::from_str(request.trim()).expect("notify request is JSON");
        seen.push(parsed["params"]["skillId"].as_str().unwrap().to_string());
        let mut writer = std::io::BufWriter::new(&stream);
        writer.write_all(ACK).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
    }
    assert_eq!(seen.len(), count, "timed out waiting for notify requests");
    seen
}

/// Bind a listener at `path`, removing any stale socket first.
fn bind(path: &Path) -> UnixListener {
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).expect("bind notify socket")
}

/// Controller with the real background worker, for tests that exercise the
/// production dispatch path end to end.
fn reconcile_controller(socket_path: &Path, canonical_root: &Path) -> Arc<NotifyController> {
    NotifyController::new(
        Arc::new(UnixSocketNotifyClient::new(
            socket_path,
            Duration::from_millis(500),
        )),
        canonical_root.to_path_buf(),
        Duration::from_millis(20),
        500,
    )
}

/// Wait for `predicate` to hold, polling until `timeout` elapses.
fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    predicate()
}

fn seed_skill(root: &Path, skill: &str) {
    std::fs::create_dir_all(
        root.join(skill)
            .join(".skill-meta/versions/v000001.snapshot"),
    )
    .expect("create snapshot dir");
}

/// Write `<root>/<skill>/.skill-meta/activation.json` and keep rewriting
/// until its mtime observably advances, so the reload poll cannot miss it
/// on a coarse-granularity filesystem.
fn write_activation_fresh(root: &Path, skill: &str) {
    let meta = root.join(skill).join(".skill-meta");
    std::fs::create_dir_all(&meta).unwrap();
    let path = meta.join("activation.json");
    let before = std::fs::metadata(&path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(15));
        std::fs::write(
            &path,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .expect("write activation.json");
        let after = std::fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        if before.map_or(after.is_some(), |before| {
            after.is_some_and(|after| after > before)
        }) {
            return;
        }
    }
    panic!("activation.json mtime did not advance");
}

// ---------------------------------------------------------------------------
// Late socket
// ---------------------------------------------------------------------------

/// The regression case from the issue: SkillFS wins the startup race, so
/// the notify socket does not exist when the reconcile is queued. The
/// reconcile must not be dropped — it must keep retrying until the daemon
/// binds the socket and acknowledges.
#[test]
fn reconcile_survives_a_socket_that_does_not_exist_yet() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");

    let ctrl = reconcile_controller(&sock_path, root.path());
    assert_eq!(ctrl.enqueue_startup_reconcile(&["alpha".to_string()]), 1);

    // No socket yet: the attempt fails as `Connect(NotFound)`, which is
    // transient, so the skill stays queued.
    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().failed >= 1),
        "the live worker must attempt delivery while the socket is absent"
    );
    let metrics = ctrl.metrics();
    assert!(metrics.attempted >= 1);
    assert!(metrics.failed >= 1);
    assert_eq!(metrics.succeeded, 0);

    // The daemon starts up and binds its socket.
    let listener = bind(&sock_path);
    let server = std::thread::spawn(move || serve_acks(&listener, 1));

    let seen = server.join().expect("server thread");
    assert_eq!(seen, vec!["alpha"], "daemon must receive the reconcile");
    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().succeeded == 1),
        "ACK must be reflected in delivery metrics"
    );
    ctrl.shutdown();
}

/// A socket that exists but has no listener yet (`ConnectionRefused`) is
/// the other half of the same startup race.
#[test]
fn reconcile_survives_a_socket_with_no_listener() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");

    // Bind then drop: the socket file remains, nothing is accepting.
    drop(bind(&sock_path));
    assert!(sock_path.exists());

    let ctrl = reconcile_controller(&sock_path, root.path());
    ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().failed >= 1),
        "connection refused must be retried"
    );

    let listener = bind(&sock_path);
    let server = std::thread::spawn(move || serve_acks(&listener, 1));

    assert_eq!(server.join().unwrap(), vec!["alpha"]);
    assert!(wait_for(Duration::from_secs(5), || ctrl
        .metrics()
        .succeeded
        == 1));
    ctrl.shutdown();
}

/// The daemon can accept the business request and restart before writing its
/// newline-delimited acknowledgement. EOF in that window is transport churn,
/// not a complete malformed response, so the startup reconcile must retry.
#[test]
fn reconcile_survives_eof_before_an_unauthenticated_ack() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");
    let listener = bind(&sock_path);

    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut dropped_first = false;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    let mut request = String::new();
                    BufReader::new(&stream).read_line(&mut request).unwrap();
                    dropped_first = true;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("accept first notify request: {error}"),
            }
        }
        assert!(dropped_first, "timed out waiting for the first request");
        serve_acks(&listener, 1)
    });

    let ctrl = reconcile_controller(&sock_path, root.path());
    ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);

    assert_eq!(server.join().unwrap(), vec!["alpha"]);
    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().succeeded == 1),
        "the retry must converge after the daemon starts acknowledging"
    );
    let metrics = ctrl.metrics();
    assert_eq!(metrics.attempted, 2);
    assert_eq!(metrics.failed, 1);
    ctrl.shutdown();
}

/// The daemon restarts during the startup window: its socket is unlinked
/// and re-bound between two reconcile attempts. Convergence must not
/// depend on catching the socket in a single instant.
#[test]
fn reconcile_survives_a_socket_deleted_and_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");

    let first = bind(&sock_path);
    let ctrl = reconcile_controller(&sock_path, root.path());
    ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);

    // Daemon goes away mid-startup, taking its socket with it.
    drop(first);
    std::fs::remove_file(&sock_path).unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().failed >= 1),
        "a reconcile lost to a socket teardown must be retried"
    );

    // Daemon comes back with a fresh socket at the same path.
    let second = bind(&sock_path);
    let server = std::thread::spawn(move || serve_acks(&second, 1));

    assert_eq!(server.join().unwrap(), vec!["alpha"]);
    assert!(wait_for(Duration::from_secs(5), || ctrl
        .metrics()
        .succeeded
        == 1));
    ctrl.shutdown();
}

/// Each skill converges on its own and is deduplicated by canonical Skill
/// identity, including nested Hermes-style ids.
#[test]
fn multiple_skills_converge_independently_over_a_real_socket() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");

    let ctrl = reconcile_controller(&sock_path, root.path());
    let names = vec![
        "alpha".to_string(),
        "category/beta".to_string(),
        "category/gamma".to_string(),
    ];
    // Enqueue twice: identity dedup must not double the work.
    ctrl.enqueue_startup_reconcile(&names);
    ctrl.enqueue_startup_reconcile(&names);
    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().failed >= 3),
        "all three skills must be attempted while the endpoint is absent"
    );

    let listener = bind(&sock_path);
    let server = std::thread::spawn(move || serve_acks(&listener, 3));

    let mut seen = server.join().unwrap();
    seen.sort();
    assert_eq!(seen, vec!["alpha", "category/beta", "category/gamma"]);
    assert!(wait_for(Duration::from_secs(5), || ctrl
        .metrics()
        .succeeded
        == 3));
    ctrl.shutdown();
}

/// A daemon that answers `ok:false` has made a decision. Retrying cannot
/// change it, so the attempt happens exactly once and does not linger.
#[test]
fn a_rejecting_daemon_is_not_retried_over_a_real_socket() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");
    let listener = bind(&sock_path);

    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        let mut writer = std::io::BufWriter::new(&stream);
        writer
            .write_all(br#"{"ok":false,"error":{"code":"unknown_skill"}}"#)
            .unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
    });

    let ctrl = reconcile_controller(&sock_path, root.path());
    ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
    server.join().unwrap();

    assert!(wait_for(Duration::from_secs(5), || ctrl.metrics().failed == 1));
    let attempted = ctrl.metrics().attempted;
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(attempted, 1, "a daemon rejection is permanent");
    assert_eq!(ctrl.metrics().attempted, attempted, "must not retry");
    ctrl.shutdown();
}

/// Client that exposes a transport outage first, then inconclusive
/// authentication, and finally recovers.
struct PhasedRetryNotifyClient {
    transient_failures: u64,
    ambiguous_failures: u64,
    attempts: AtomicU64,
    events: Mutex<Vec<CapturedNotify>>,
}

impl PhasedRetryNotifyClient {
    fn new(transient_failures: u64, ambiguous_failures: u64) -> Self {
        Self {
            transient_failures,
            ambiguous_failures,
            attempts: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::Relaxed)
    }
}

impl NotifyClient for PhasedRetryNotifyClient {
    fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
        if attempt < self.transient_failures {
            return Err(NotifyError::EndpointUnavailable(
                "test: endpoint is not listening".to_string(),
            ));
        }
        if attempt < self.transient_failures + self.ambiguous_failures {
            return Err(NotifyError::AuthInconclusive(
                "test: endpoint closed during authentication".to_string(),
            ));
        }
        self.events.lock().push(CapturedNotify {
            schema_version: event.params.schema_version,
            skill_id: event.params.skill_id.clone(),
            event_kind: event.params.event_kind.clone(),
            paths: event.params.paths.clone(),
            canonical_skill_dir: event.params.canonical_skill_dir.clone(),
        });
        Ok(())
    }
}

/// The live worker must preserve convergence when transport recovery is
/// followed by inconclusive authentication. The deterministic unit suite
/// separately drives the full endpoint budget without production sleeps.
#[test]
fn transient_then_ambiguous_failures_still_converge() {
    let root = tempfile::tempdir().unwrap();
    // Keep transport retry depth independent of the ambiguous-auth budget:
    // changing one policy must not silently lengthen this test or alter the
    // other policy's coverage.
    let transient_failures = 4;
    let ambiguous_failures = 2;
    let client = Arc::new(PhasedRetryNotifyClient::new(
        transient_failures,
        ambiguous_failures,
    ));
    let ctrl = NotifyController::new(
        client.clone(),
        root.path().to_path_buf(),
        Duration::from_millis(20),
        500,
    );

    ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);

    assert!(
        wait_for(Duration::from_secs(20), || ctrl.metrics().succeeded == 1),
        "reconcile must survive transport failures followed by ambiguous auth; metrics: {:?}",
        ctrl.metrics()
    );
    assert_eq!(
        client.attempts(),
        transient_failures + ambiguous_failures + 1
    );
    ctrl.shutdown();
}

// ---------------------------------------------------------------------------
// Convergence of the mounted view
// ---------------------------------------------------------------------------

/// Notify client that fails its first `failures` attempts and, on the
/// first success, writes `activation.json` for the notified skill — the
/// daemon's side of a reconcile.
struct LateDaemonNotifyClient {
    daemon_root: PathBuf,
    failures: u32,
    attempts: AtomicU64,
    events: Mutex<Vec<CapturedNotify>>,
}

impl LateDaemonNotifyClient {
    fn new(daemon_root: &Path, failures: u32) -> Self {
        Self {
            daemon_root: daemon_root.to_path_buf(),
            failures,
            attempts: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::Relaxed)
    }

    fn events(&self) -> Vec<CapturedNotify> {
        self.events.lock().clone()
    }
}

impl NotifyClient for LateDaemonNotifyClient {
    fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
        if attempt < u64::from(self.failures) {
            return Err(NotifyError::EndpointUnavailable(
                "test: daemon socket not created yet".to_string(),
            ));
        }
        self.events.lock().push(CapturedNotify {
            schema_version: event.params.schema_version,
            skill_id: event.params.skill_id.clone(),
            event_kind: event.params.event_kind.clone(),
            paths: event.params.paths.clone(),
            canonical_skill_dir: event.params.canonical_skill_dir.clone(),
        });
        write_activation_fresh(&self.daemon_root, &event.params.skill_id);
        Ok(())
    }
}

/// End-to-end: a skill with no activation is hidden at mount time. The
/// daemon is not up yet, so the first reconcile attempts fail. Once the
/// retried reconcile lands and the daemon writes activation, the mounted
/// view flips to visible — with no further filesystem event to trigger it.
///
/// This is the property the old one-shot reconcile lost: the activation
/// watcher can only observe activation the daemon has already written, so
/// without a surviving reconcile there is nothing to make the daemon scan
/// a skill it has never seen.
#[test]
fn a_new_skill_becomes_visible_without_any_extra_filesystem_event() {
    let dir = tempfile::tempdir().unwrap();
    seed_skill(dir.path(), "alpha");

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload = Arc::new(ActivationReloadController::new(
        dir.path(),
        resolver.clone(),
        Duration::from_millis(20),
        Duration::from_secs(2),
    ));
    let writer = Arc::new(InMemoryProtocolEventWriter::new());
    let client = Arc::new(LateDaemonNotifyClient::new(dir.path(), 2));
    let ctrl = NotifyController::new_with_reload(
        client.clone(),
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Duration::from_millis(50),
        500,
        writer.clone(),
        reload,
    );

    ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);

    // First attempt: daemon unreachable, so the skill is still unknown to
    // it and must remain fail-safe hidden.
    assert!(wait_for(Duration::from_secs(5), || client.attempts() >= 1));
    assert!(
        matches!(
            resolver.get("alpha"),
            None | Some(ActiveTarget::Hidden { .. })
        ),
        "a skill the daemon has never scanned must stay hidden, got {:?}",
        resolver.get("alpha")
    );

    // The worker retries; the third attempt reaches the daemon, which writes
    // activation.json.
    assert!(
        wait_for(Duration::from_secs(10), || ctrl.metrics().succeeded == 1),
        "the live worker must deliver after the daemon recovers"
    );

    assert_eq!(client.attempts(), 3, "two failures then the delivery");
    assert_eq!(client.events().len(), 1);
    assert_eq!(client.events()[0].event_kind, "reconcile");

    match resolver.get("alpha") {
        Some(ActiveTarget::Snapshot { version, .. }) => {
            assert_eq!(version, "v000001.snapshot")
        }
        other => panic!("skill must become visible after the retried reconcile, got {other:?}"),
    }
    ctrl.shutdown();
}

/// A skill that already has valid activation keeps serving that activation
/// for the whole time its reconcile sits pending. Retry must not make the
/// existing trusted view worse than the pre-fix behaviour, where a dropped
/// reconcile simply left the stale-but-valid mapping in place.
#[test]
fn an_activated_skill_stays_readable_while_its_reconcile_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    seed_skill(dir.path(), "alpha");
    write_activation_fresh(dir.path(), "alpha");

    let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
    let reload = Arc::new(ActivationReloadController::new(
        dir.path(),
        resolver.clone(),
        Duration::from_millis(20),
        Duration::from_millis(200),
    ));
    // Prime the resolver from the on-disk activation, as mount bootstrap
    // does.
    reload.reload_skill_once("alpha");
    let primed = resolver.get("alpha");
    assert!(
        matches!(primed, Some(ActiveTarget::Snapshot { .. })),
        "precondition: skill must start visible, got {primed:?}"
    );

    let writer = Arc::new(InMemoryProtocolEventWriter::new());
    // Never reachable: the reconcile stays pending for the whole test.
    let client = Arc::new(LateDaemonNotifyClient::new(dir.path(), u32::MAX));
    let ctrl = NotifyController::new_with_reload(
        client.clone(),
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Duration::from_millis(50),
        500,
        writer.clone(),
        reload,
    );

    ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while client.attempts() < 4 && Instant::now() < deadline {
        assert_eq!(
            resolver.get("alpha").map(|t| t.as_label()),
            primed.as_ref().map(|t| t.as_label()),
            "a pending reconcile must not disturb the existing trusted view"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(client.attempts() >= 4);
    assert_eq!(ctrl.metrics().succeeded, 0);
    ctrl.shutdown();
}

// ---------------------------------------------------------------------------
// Production dispatch path (real background worker)
// ---------------------------------------------------------------------------

/// The same regression, driven entirely by the production worker with no
/// test-side flushing: SkillFS queues the reconcile against a socket that
/// does not exist, the daemon starts later, and the worker's own retry loop
/// delivers it. This is the test that would have failed before the fix.
#[test]
fn the_live_worker_retries_until_a_late_daemon_answers() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");

    let ctrl = reconcile_controller(&sock_path, root.path());
    ctrl.enqueue_startup_reconcile(&["alpha".to_string(), "category/beta".to_string()]);

    // Let the worker fail against the absent socket a few times.
    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().failed >= 2),
        "worker must retry while the socket is absent, metrics: {:?}",
        ctrl.metrics()
    );
    assert_eq!(
        ctrl.metrics().succeeded,
        0,
        "nothing can be delivered before the daemon exists"
    );

    // Daemon starts.
    let listener = bind(&sock_path);
    let server = std::thread::spawn(move || serve_acks(&listener, 2));

    let mut seen = server.join().expect("daemon must receive both reconciles");
    seen.sort();
    assert_eq!(seen, vec!["alpha", "category/beta"]);

    // Wait on `succeeded`, not `pending_len`: an in-flight entry is already
    // out of the queue, so pending can read 0 before the ACK is recorded.
    assert!(
        wait_for(Duration::from_secs(5), || ctrl.metrics().succeeded == 2),
        "both reconciles must be recorded as delivered, metrics: {:?}",
        ctrl.metrics()
    );
    assert_eq!(ctrl.pending_len(), 0, "nothing left to retry");
    ctrl.shutdown();
}

/// A refused authentication handshake must enter the shared endpoint gate,
/// rather than multiplying immediate attempts by the number of Skills.
///
/// This drives the real authenticated client. A daemon that closes during
/// the handshake and sec-core refusing a bad client proof both reach the
/// same EOF/InvalidFrame classification. Deterministic unit tests exercise
/// full budget exhaustion and generation reopening without sleeping through
/// the production backoff window; this integration test pins the real wire
/// classification and verifies that its first retry is endpoint-gated.
#[test]
fn refused_authentication_is_retried_through_the_endpoint_gate() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    // The authenticated client requires an owner-only parent and socket.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");
    let key_path = dir.path().join("notify.key");
    std::fs::write(&key_path, [7_u8; 32]).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let listener = bind(&sock_path);
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    // Read the client hello, then close before sending a challenge. A wrong
    // key closes later in the handshake, but both wire paths surface as the
    // same EOF/InvalidFrame classification to the client.
    let accepted = Arc::new(AtomicU64::new(0));
    let accepted_for_server = accepted.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_server = stop.clone();
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while !stop_for_server.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let mut hello = String::new();
                    let _ = BufReader::new(&stream).read_line(&mut hello);
                    accepted_for_server.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept authenticated notify client: {error}"),
            }
        }
    });

    let client = Arc::new(
        UnixSocketNotifyClient::new_authenticated(
            &sock_path,
            Duration::from_millis(300),
            &key_path,
        )
        .expect("build authenticated notify client"),
    );
    let ctrl = NotifyController::new(
        client,
        root.path().to_path_buf(),
        Duration::from_millis(20),
        300,
    );
    let skills: Vec<String> = (0..12).map(|index| format!("skill-{index}")).collect();
    ctrl.enqueue_startup_reconcile(&skills);

    // Watch `attempted`, not `pending_len`: the worker drains an entry out
    // of the queue before dispatching it, so pending reads 0 during every
    // send window and would signal "finished" after the first attempt.
    assert!(
        wait_for(Duration::from_secs(2), || ctrl.metrics().attempted >= 1),
        "the real handshake EOF must produce one delivery attempt; metrics: {:?}",
        ctrl.metrics()
    );

    // The first retry cannot be multiplied by the other eleven due Skills.
    // Even the minimum jittered first backoff is 188 ms.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        ctrl.metrics().attempted,
        1,
        "all Skills must share the first endpoint backoff"
    );

    // EOF must still be retried rather than classified as permanent.
    assert!(
        wait_for(Duration::from_secs(2), || ctrl.metrics().attempted >= 2),
        "the endpoint gate must schedule a later probe; metrics: {:?}",
        ctrl.metrics()
    );
    ctrl.shutdown();
    assert_eq!(ctrl.metrics().attempted, 2);
    assert_eq!(ctrl.metrics().succeeded, 0);
    assert!(
        wait_for(Duration::from_secs(2), || accepted.load(Ordering::Relaxed)
            == 2),
        "the server must observe exactly the real send attempts"
    );
    stop.store(true, Ordering::Release);
    server.join().unwrap();
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

/// Shutdown must abandon the remainder of an already-drained batch, not
/// walk it entry by entry paying a socket timeout each time.
///
/// The daemon here accepts connections and never answers, so every send
/// blocks for the full client timeout. With a batch of many skills, a
/// shutdown arriving during the first send must not cost one timeout per
/// remaining skill.
#[test]
fn shutdown_abandons_the_rest_of_a_drained_batch() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");
    let listener = bind(&sock_path);

    // Hold every accepted connection open without replying, so each send
    // burns its full read timeout.
    let held = Arc::new(Mutex::new(Vec::new()));
    let held_for_server = held.clone();
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            held_for_server.lock().push(stream);
        }
    });

    const SEND_TIMEOUT: Duration = Duration::from_millis(400);
    const SKILLS: usize = 12;

    let ctrl = NotifyController::new(
        Arc::new(UnixSocketNotifyClient::new(&sock_path, SEND_TIMEOUT)),
        root.path().to_path_buf(),
        Duration::from_millis(5),
        SEND_TIMEOUT.as_millis() as u64,
    );
    let names: Vec<String> = (0..SKILLS).map(|i| format!("skill-{i}")).collect();
    ctrl.enqueue_startup_reconcile(&names);

    // Wait until the worker is inside the first blocking send.
    assert!(
        wait_for(Duration::from_secs(3), || ctrl.metrics().attempted >= 1),
        "worker must have started a send"
    );

    let start = Instant::now();
    ctrl.shutdown();
    // Give the worker room to finish the in-flight send and notice the flag.
    std::thread::sleep(SEND_TIMEOUT * 3);
    let elapsed = start.elapsed();
    let attempted_after_shutdown = ctrl.metrics().attempted;

    // Walking the whole batch would cost SKILLS * SEND_TIMEOUT.
    assert!(
        attempted_after_shutdown < SKILLS as u64,
        "shutdown must abandon the batch, but {attempted_after_shutdown} of \
         {SKILLS} skills were attempted"
    );
    assert!(
        elapsed < SEND_TIMEOUT * (SKILLS as u32),
        "teardown took {elapsed:?}, close to a full serial batch"
    );

    // And no further attempts happen at all after shutdown settles.
    let settled = ctrl.metrics().attempted;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        ctrl.metrics().attempted,
        settled,
        "no new attempts may start after shutdown"
    );
    drop(ctrl);
}

/// Shutdown must both wake the worker and forbid requeueing, so an
/// unreachable daemon cannot keep the retry loop — and the controller's
/// private runtime thread — alive past teardown.
#[test]
fn shutdown_ends_the_retry_loop_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    // Never created, so every attempt fails transiently.
    let sock_path = dir.path().join("never-bound.sock");

    let start = Instant::now();
    for _ in 0..4 {
        let ctrl = NotifyController::new(
            Arc::new(UnixSocketNotifyClient::new(
                &sock_path,
                Duration::from_millis(50),
            )),
            root.path().to_path_buf(),
            // Short debounce so the live worker actually runs the retries.
            Duration::from_millis(5),
            50,
        );
        ctrl.enqueue_startup_reconcile(&["alpha".to_string(), "beta".to_string()]);
        std::thread::sleep(Duration::from_millis(60));
        assert!(ctrl.metrics().attempted > 0, "worker must have tried");
        // Dropping the last Arc triggers shutdown via Drop.
        drop(ctrl);
    }
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "an unbounded retry loop would have blocked teardown; took {:?}",
        start.elapsed()
    );
}

/// Sanity check that the ACK helper and the wire shape agree, so a failing
/// retry test above is a retry bug and not a protocol mismatch.
#[test]
fn ack_helper_matches_the_notify_v2_wire_contract() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");
    let listener = bind(&sock_path);
    let server = std::thread::spawn(move || serve_acks(&listener, 1));

    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
    let event = NotifyChangeEvent::new(
        "/srv/skills/alpha",
        "alpha",
        NotifyEventKind::Reconcile,
        Vec::new(),
        5000,
    );
    let result = client.send(&event);
    assert_eq!(server.join().unwrap(), vec!["alpha"]);
    assert!(result.is_ok(), "reconcile ACK must be accepted: {result:?}");
}

/// Connecting to a path that was never created reports a transient error,
/// not a permanent one. This is the classification the retry loop depends
/// on, verified against the real client rather than a stub.
#[test]
fn real_client_reports_a_missing_socket_as_transient() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("absent.sock");
    let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_millis(100));
    let event = NotifyChangeEvent::new(
        "/srv/skills/alpha",
        "alpha",
        NotifyEventKind::Reconcile,
        Vec::new(),
        100,
    );

    let error = client.send(&event).expect_err("no socket at that path");
    assert!(
        error.is_transient(),
        "a missing socket must be retryable, got {error}"
    );

    // And a socket file with no listener behaves the same way.
    drop(bind(&sock_path));
    let error = UnixStream::connect(&sock_path).expect_err("nothing is listening");
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
    let error = client.send(&event).expect_err("nothing is listening");
    assert!(
        error.is_transient(),
        "connection refused must be retryable, got {error}"
    );
}
