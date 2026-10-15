//! Component tests for UI worker coordination.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::raw_input::InputRouter;
use crate::runtime::terminal_publisher::TerminalPublisher;
use crate::ui::worker_protocol::{
    DrawOp, EmptyRenderView, ProtocolLimits, RenderRequest, RenderView, Viewport,
};

use super::{
    CandidateIdentity, CoordinatorError, CoordinatorPolicy, InflightFrame, RecoveryMode,
    UiWorkerCoordinator, WorkerLifecycle,
};

const TEST_VERSION: &str = "task-6b-test-candidate";
const TEST_NONCE: [u8; 16] = [0x6b; 16];

fn policy(failure_limit: u64) -> CoordinatorPolicy {
    CoordinatorPolicy {
        protocol_limits: ProtocolLimits {
            max_payload_bytes: 65_536,
            max_draw_ops: 128,
            max_text_bytes: 4_096,
        },
        handshake_timeout: Duration::from_millis(500),
        heartbeat_interval: Duration::from_millis(50),
        heartbeat_timeout: Duration::from_millis(500),
        backpressure_timeout: Duration::from_millis(500),
        shutdown_timeout: Duration::from_millis(500),
        failure_limit,
        pending_revision_capacity: 1,
    }
}

fn render(revision: u64, columns: u16, rows: u16) -> RenderRequest {
    RenderRequest {
        capture_id: Some(revision + 1_000),
        viewport: Viewport { columns, rows },
        render_view: RenderView::Empty(EmptyRenderView { revision }),
    }
}

struct FixtureBinary {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl FixtureBinary {
    fn new(behavior: &str) -> Self {
        let directory = tempfile::tempdir().expect("create coordinator fixture directory");
        let path = directory.path().join("ui-worker-fixture.py");
        let source = format!(
            r#"#!/usr/bin/python3
import json
import os
import select
import socket
import struct
import time

BEHAVIOR = {behavior:?}
# Wire schema this fixture speaks. The Host rejects a worker on an older
# schema at Hello/Ready, so a v1 fixture would prove nothing about v2.
SCHEMA = 1 if BEHAVIOR == 'schema-v1' else 2
args = dict(zip(iter(__import__('sys').argv[1::2]), iter(__import__('sys').argv[2::2])))
fd = int(args['--ipc-fd'])
heartbeat_interval = int(args.get('--heartbeat-interval-nanos', '50000000')) / 1_000_000_000
sock = socket.socket(fileno=fd)

def read_exact(size):
    data = b''
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            return None
        data += chunk
    return data

def receive():
    prefix = read_exact(4)
    if prefix is None:
        return None
    size = struct.unpack('<I', prefix)[0]
    frame = read_exact(size)
    if frame is None:
        return None
    message_type = frame[6]
    nonce = frame[11:27]
    generation = struct.unpack('<Q', frame[27:35])[0]
    frame_id = struct.unpack('<Q', frame[35:43])[0]
    payload = json.loads(frame[43:].decode())
    return message_type, nonce, generation, frame_id, payload

def send(message_type, nonce, generation, frame_id, payload):
    body = json.dumps(payload, separators=(',', ':')).encode()
    header = b'CUI1' + struct.pack('<HBI', SCHEMA, message_type, len(body))
    header += nonce + struct.pack('<QQ', generation, frame_id)
    sock.sendall(struct.pack('<I', len(header) + len(body)) + header + body)

if BEHAVIOR == 'exit-immediately':
    raise SystemExit(31)
if BEHAVIOR == 'never-read':
    time.sleep(60)
    raise SystemExit(32)
if BEHAVIOR == 'require-heartbeat-interval' and '--heartbeat-interval-nanos' not in args:
    raise SystemExit(34)
if BEHAVIOR == 'count-shutdown':
    time.sleep(0.05)
hello = receive()
if hello is None:
    raise SystemExit(33)
_, nonce, generation, frame_id, payload = hello
if BEHAVIOR != 'count-shutdown':
    ready_version = 'wrong-candidate' if BEHAVIOR == 'wrong-version' else payload['candidate_version']
    send(4, nonce, generation, frame_id, {{
        'protocol_version': SCHEMA,
        'candidate_version': ready_version,
        'capabilities': ['typed-draw-ops-v1'],
    }})
if BEHAVIOR == 'abort-after-ready':
    time.sleep(0.05)
    os.abort()
heartbeat = 0
latest_ack_frame_id = 0
while True:
    if BEHAVIOR == 'periodic-heartbeat':
        readable, _, _ = select.select([sock], [], [], heartbeat_interval)
        if not readable:
            heartbeat += 1
            send(6, nonce, generation, latest_ack_frame_id, {{'monotonic_seq': heartbeat}})
            continue
    message = receive()
    if message is None:
        break
    message_type, nonce, generation, frame_id, payload = message
    if message_type == 1:
        revision = payload['render_view']['Empty']['revision']
        viewport = payload['viewport']
        response_generation = generation - 1 if BEHAVIOR == 'old-generation' else generation
        full_repaint = revision != 77
        prepared = {{
            'worker_generation': response_generation,
            'frame_id': frame_id,
            'capture_id': payload['capture_id'],
            'draw_ops': [
                {{'MoveTo': {{'x': 1, 'y': 1}}}},
                {{'Text': 'revision:' + str(revision)}},
            ],
            'geometry': {{
                'columns': viewport['columns'],
                'rows': viewport['rows'],
                'cursor_x': 1 + len('revision:' + str(revision)),
                'cursor_y': 1,
            }},
            'full_repaint': full_repaint,
        }}
        send(5, nonce, response_generation, frame_id, prepared)
        if BEHAVIOR == 'duplicate-prepared':
            send(5, nonce, response_generation, frame_id, prepared)
        if BEHAVIOR == 'queued-idle-heartbeats-before-ack':
            heartbeat += 1
            send(6, nonce, generation, latest_ack_frame_id, {{'monotonic_seq': heartbeat}})
            heartbeat += 1
            send(6, nonce, generation, latest_ack_frame_id, {{'monotonic_seq': heartbeat}})
    elif message_type == 2:
        latest_ack_frame_id = frame_id
        heartbeat += 1
        send(6, nonce, generation, latest_ack_frame_id, {{'monotonic_seq': heartbeat}})
    elif message_type == 3:
        if BEHAVIOR == 'count-shutdown':
            with open(os.path.join(os.path.dirname(__file__), 'shutdown-count'), 'a') as count:
                count.write('1\n')
            continue
        break
"#
        );
        fs::write(&path, source).expect("write coordinator fixture executable");
        let mut permissions = fs::metadata(&path)
            .expect("stat coordinator fixture")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("make coordinator fixture executable");
        Self {
            _directory: directory,
            path,
        }
    }

    fn candidate(&self) -> CandidateIdentity {
        CandidateIdentity {
            binary_path: self.path.clone(),
            candidate_version: TEST_VERSION.to_owned(),
        }
    }

    fn shutdown_count(&self) -> usize {
        let path = self
            .path
            .parent()
            .expect("fixture path has a parent")
            .join("shutdown-count");
        fs::read_to_string(path)
            .map(|contents| contents.lines().count())
            .unwrap_or(0)
    }
}

fn start_ready(fixture: &FixtureBinary, failure_limit: u64) -> (UiWorkerCoordinator, Instant) {
    let now = Instant::now();
    let mut coordinator =
        UiWorkerCoordinator::new(fixture.candidate(), TEST_NONCE, 1, policy(failure_limit))
            .expect("construct coordinator");
    coordinator.start(now).expect("start fixture worker");
    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(1))
    });
    (coordinator, now)
}

fn drive_until(
    coordinator: &mut UiWorkerCoordinator,
    predicate: impl Fn(&UiWorkerCoordinator) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        coordinator
            .tick(Instant::now())
            .expect("drive coordinator state machine");
        if predicate(coordinator) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!(
        "coordinator did not reach expected state: lifecycle={:?} failures={}",
        coordinator.lifecycle(),
        coordinator.failure_count()
    );
}

fn take_prepared(
    coordinator: &mut UiWorkerCoordinator,
) -> crate::ui::worker_protocol::PreparedFrame {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(frame) = coordinator
            .tick(Instant::now())
            .expect("drive coordinator until frame")
        {
            return frame;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("coordinator did not return a prepared frame");
}

fn publish_frame(
    coordinator: &mut UiWorkerCoordinator,
    frame: &crate::ui::worker_protocol::PreparedFrame,
    input_barrier: u64,
) {
    coordinator
        .begin_publishing(frame.frame_key(), input_barrier)
        .expect("begin frame publication");
    coordinator
        .publish_succeeded(
            frame.frame_key(),
            input_barrier + 1,
            frame.geometry,
            Instant::now(),
        )
        .expect("finish frame publication");
}

fn process_is_absent(pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}")).exists()
}

fn worker_pid(coordinator: &UiWorkerCoordinator) -> Option<u32> {
    coordinator
        .process
        .as_ref()
        .and_then(|process| process.pid())
}

#[test]
fn coordinator_publisher_and_router_compose_actual_runtime_modules() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    let request = render(21, 80, 24);
    coordinator
        .request_render(request, now)
        .expect("request frame");
    let frame = take_prepared(&mut coordinator);
    let key = frame.frame_key();
    let input_router = Arc::new(Mutex::new(InputRouter::new(TEST_NONCE, 1, 0, 0)));
    let input_sequence = input_router.lock().expect("input router").input_sequence();
    let mut publisher = TerminalPublisher::new(
        Viewport {
            columns: 80,
            rows: 24,
        },
        policy(3).protocol_limits,
    );
    let mut terminal = Vec::new();

    coordinator
        .begin_publishing(key, input_sequence.current())
        .expect("begin publishing");
    let receipt = publisher
        .publish(
            &mut terminal,
            frame,
            &mut input_router.lock().expect("input router"),
            &input_sequence,
        )
        .expect("publish through actual terminal publisher");
    coordinator
        .publish_succeeded(key, receipt.publish_seq, receipt.geometry, Instant::now())
        .expect("commit coordinator publication");

    assert_eq!(terminal, b"\x1b[2;2Hrevision:21");
    assert_eq!(receipt.frame_id, key.frame_id);
    assert!(receipt.token.is_some());
}

#[test]
fn coordinator_requires_nonzero_heartbeat_interval_below_timeout() {
    let fixture = FixtureBinary::new("healthy");
    let mut zero_interval = policy(3);
    zero_interval.heartbeat_interval = Duration::ZERO;
    assert!(matches!(
        UiWorkerCoordinator::new(fixture.candidate(), TEST_NONCE, 1, zero_interval),
        Err(CoordinatorError::InvalidPolicy)
    ));

    let mut equal_interval = policy(3);
    equal_interval.heartbeat_interval = equal_interval.heartbeat_timeout;
    assert!(matches!(
        UiWorkerCoordinator::new(fixture.candidate(), TEST_NONCE, 1, equal_interval),
        Err(CoordinatorError::InvalidPolicy)
    ));
}

#[test]
fn coordinator_launch_contract_injects_heartbeat_interval() {
    let fixture = FixtureBinary::new("require-heartbeat-interval");
    let launch_policy = policy(3);
    drop(
        super::process::WorkerProcess::spawn(
            &fixture.path,
            launch_policy.protocol_limits,
            launch_policy.heartbeat_interval,
        )
        .expect("isolated coordinator fixture must launch before handshake"),
    );
    let (coordinator, _) = start_ready(&fixture, 3);

    assert!(matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(1)));
    assert_eq!(coordinator.failure_count(), 0);
}

#[test]
fn coordinator_sigstop_idle_after_ready_times_out_kills_waits_and_rebuilds() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, _) = start_ready(&fixture, 3);
    let old_pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(old_pid as i32, nix::libc::SIGSTOP);
    }

    let now = Instant::now();
    coordinator
        .tick(now + policy(3).heartbeat_timeout + Duration::from_millis(1))
        .expect("detect idle heartbeat timeout");

    assert!(matches!(
        coordinator.lifecycle(),
        WorkerLifecycle::Rebuilding(2)
    ));
    assert!(process_is_absent(old_pid), "idle worker must be waited");
    assert_eq!(coordinator.failure_count(), 1);
}

#[test]
fn coordinator_sigstop_after_one_heartbeat_times_out_kills_waits_and_rebuilds() {
    let fixture = FixtureBinary::new("periodic-heartbeat");
    let (mut coordinator, _) = start_ready(&fixture, 3);
    drive_until(&mut coordinator, |coordinator| {
        coordinator.last_heartbeat_sequence().is_some()
    });
    let observed_sequence = coordinator
        .last_heartbeat_sequence()
        .expect("worker sent its first heartbeat");
    let old_pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(old_pid as i32, nix::libc::SIGSTOP);
    }

    let now = Instant::now();
    coordinator
        .tick(now + policy(3).heartbeat_timeout + Duration::from_millis(1))
        .expect("detect recurring heartbeat timeout");

    assert!(matches!(
        coordinator.lifecycle(),
        WorkerLifecycle::Rebuilding(2)
    ));
    assert!(process_is_absent(old_pid), "stopped worker must be waited");
    assert_eq!(coordinator.failure_count(), 1);
    assert_eq!(observed_sequence, 1);
}

#[test]
fn coordinator_worker_panic_rebuilds_without_resetting_failure_count() {
    let fixture = FixtureBinary::new("abort-after-ready");
    let (mut coordinator, _) = start_ready(&fixture, 3);

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });

    assert_eq!(coordinator.failure_count(), 1);
    assert_eq!(coordinator.worker_generation(), 2);
}

#[test]
fn coordinator_sigkill_reaps_old_pid_before_generation_advances() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, _) = start_ready(&fixture, 3);
    let old_pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(old_pid as i32, nix::libc::SIGKILL);
    }

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });

    assert!(
        process_is_absent(old_pid),
        "old pid must be waited before rebuild"
    );
    assert_eq!(coordinator.worker_generation(), 2);
}

#[test]
fn coordinator_sigstop_heartbeat_timeout_kills_and_reaps_child() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(1, 80, 24), now)
        .expect("request frame before heartbeat");
    let frame = take_prepared(&mut coordinator);
    coordinator
        .begin_publishing(frame.frame_key(), 10)
        .expect("begin publication");
    coordinator
        .publish_succeeded(frame.frame_key(), 11, frame.geometry, Instant::now())
        .expect("queue published acknowledgement");

    let old_pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(old_pid as i32, nix::libc::SIGSTOP);
    }
    let sent_at = Instant::now();
    coordinator
        .tick(sent_at)
        .expect("send acknowledgement to stopped worker");
    coordinator
        .tick(sent_at + policy(3).heartbeat_timeout + Duration::from_millis(1))
        .expect("detect heartbeat timeout");

    assert!(matches!(
        coordinator.lifecycle(),
        WorkerLifecycle::Rebuilding(2)
    ));
    assert!(
        process_is_absent(old_pid),
        "SIGSTOP child must be killed and waited"
    );
    assert_eq!(coordinator.failure_count(), 1);
}

#[test]
fn coordinator_rejects_late_frame_from_prior_generation() {
    let fixture = FixtureBinary::new("old-generation");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(4, 20, 5), now)
        .expect("request frame");

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });

    assert_eq!(coordinator.failure_count(), 1);
    assert!(coordinator.inflight().is_none());
}

#[test]
fn coordinator_ready_then_crash_still_consumes_failure_budget() {
    let fixture = FixtureBinary::new("abort-after-ready");
    let (mut coordinator, _) = start_ready(&fixture, 2);
    assert_eq!(
        coordinator.failure_count(),
        0,
        "Ready is not a budget reset"
    );

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });
    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(2))
    });
    assert_eq!(
        coordinator.failure_count(),
        1,
        "Ready must not reset failures"
    );
    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Unavailable)
    });

    assert_eq!(coordinator.failure_count(), 2);
    assert_eq!(coordinator.recovery_mode(), RecoveryMode::InteractionEnding);
}

#[test]
fn coordinator_pending_latest_revision_cannot_be_overwritten_by_older_frame() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(1, 20, 5), now)
        .expect("request first revision");
    coordinator
        .request_render(render(9, 30, 6), now)
        .expect("queue newest revision");
    coordinator
        .request_render(render(7, 25, 4), now)
        .expect("ignore older queued revision");
    assert_eq!(coordinator.pending_latest_revision(), Some(9));

    let first = take_prepared(&mut coordinator);
    coordinator
        .begin_publishing(first.frame_key(), 41)
        .expect("begin first publication");
    assert!(matches!(
        coordinator.inflight(),
        Some(InflightFrame::Publishing {
            input_barrier: 41,
            ..
        })
    ));
    coordinator
        .publish_succeeded(first.frame_key(), 42, first.geometry, Instant::now())
        .expect("finish first publication");

    let latest = take_prepared(&mut coordinator);
    assert_eq!(latest.geometry.columns, 30);
    assert_eq!(latest.geometry.rows, 6);
    assert!(latest
        .draw_ops
        .iter()
        .any(|op| matches!(op, DrawOp::Text(text) if text == "revision:9")));
    assert_eq!(coordinator.pending_latest_revision(), None);
}

#[test]
fn coordinator_equal_revision_changed_active_request_replaces_pending() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    let active = render(5, 20, 5);
    let mut changed = render(5, 31, 7);
    changed.capture_id = Some(9_005);

    coordinator
        .request_render(active, now)
        .expect("request active snapshot");
    coordinator
        .request_render(changed.clone(), now)
        .expect("queue changed equal-revision snapshot");
    assert_eq!(coordinator.pending_latest_request.as_ref(), Some(&changed));

    let first = take_prepared(&mut coordinator);
    publish_frame(&mut coordinator, &first, 10);
    let latest = take_prepared(&mut coordinator);

    assert_eq!(latest.capture_id, changed.capture_id);
    assert_eq!(latest.geometry.columns, changed.viewport.columns);
    assert_eq!(latest.geometry.rows, changed.viewport.rows);
}

#[test]
fn coordinator_equal_revision_changed_pending_request_replaces_pending() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    let active = render(8, 20, 5);
    let first_pending = render(8, 31, 7);
    let mut latest_pending = render(8, 42, 9);
    latest_pending.capture_id = Some(10_008);

    coordinator
        .request_render(active, now)
        .expect("request active snapshot");
    coordinator
        .request_render(first_pending, now)
        .expect("queue first equal-revision snapshot");
    coordinator
        .request_render(latest_pending.clone(), now)
        .expect("replace pending equal-revision snapshot");
    assert_eq!(
        coordinator.pending_latest_request.as_ref(),
        Some(&latest_pending)
    );

    let first = take_prepared(&mut coordinator);
    publish_frame(&mut coordinator, &first, 20);
    let latest = take_prepared(&mut coordinator);

    assert_eq!(latest.capture_id, latest_pending.capture_id);
    assert_eq!(latest.geometry.columns, latest_pending.viewport.columns);
    assert_eq!(latest.geometry.rows, latest_pending.viewport.rows);
}

#[test]
fn coordinator_recovery_preserves_later_equal_revision_request() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    let active = render(11, 20, 5);
    let mut pending = render(11, 47, 13);
    pending.capture_id = Some(20_011);

    coordinator
        .request_render(active, now)
        .expect("request active snapshot");
    coordinator
        .request_render(pending.clone(), now)
        .expect("queue later equal-revision snapshot");
    let old_pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(old_pid as i32, nix::libc::SIGKILL);
    }

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });
    assert_eq!(coordinator.pending_latest_request.as_ref(), Some(&pending));
    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(2))
    });
    let rebuilt = take_prepared(&mut coordinator);

    assert_eq!(rebuilt.worker_generation, 2);
    assert_eq!(rebuilt.capture_id, pending.capture_id);
    assert_eq!(rebuilt.geometry.columns, pending.viewport.columns);
    assert_eq!(rebuilt.geometry.rows, pending.viewport.rows);
    assert!(rebuilt
        .draw_ops
        .iter()
        .any(|op| matches!(op, DrawOp::Text(text) if text == "revision:11")));
}

#[test]
fn coordinator_exact_duplicate_active_request_is_dropped() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    let request = render(13, 20, 5);

    coordinator
        .request_render(request.clone(), now)
        .expect("request active snapshot");
    coordinator
        .request_render(request, now)
        .expect("drop exact duplicate snapshot");

    assert!(coordinator.pending_latest_request.is_none());
}

#[test]
fn coordinator_rejects_publish_sequence_below_recorded_input_barrier() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(3, 20, 5), now)
        .expect("request frame");
    let frame = take_prepared(&mut coordinator);
    coordinator
        .begin_publishing(frame.frame_key(), 50)
        .expect("begin publication");

    let result =
        coordinator.publish_succeeded(frame.frame_key(), 49, frame.geometry, Instant::now());

    assert!(matches!(result, Err(CoordinatorError::InvalidState)));
    assert!(coordinator.published().is_none());
    assert!(matches!(
        coordinator.inflight(),
        Some(InflightFrame::Publishing {
            input_barrier: 50,
            ..
        })
    ));
}

#[test]
fn coordinator_publish_ack_deadline_overflow_preserves_uncommitted_snapshot() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(6, 20, 5), now)
        .expect("request frame");
    let frame = take_prepared(&mut coordinator);
    coordinator
        .begin_publishing(frame.frame_key(), 60)
        .expect("begin publication");
    coordinator.policy.backpressure_timeout = Duration::MAX;

    let result =
        coordinator.publish_succeeded(frame.frame_key(), 61, frame.geometry, Instant::now());

    assert!(matches!(result, Err(CoordinatorError::DeadlineOverflow)));
    assert!(coordinator.published.is_none());
    assert!(coordinator.active_render.is_some());
    assert!(coordinator.outbound.is_none());
    assert!(matches!(
        coordinator.inflight,
        Some(InflightFrame::Publishing {
            input_barrier: 60,
            ..
        })
    ));
}

#[test]
fn coordinator_duplicate_prepared_frame_fails_generation_and_rebuilds() {
    let fixture = FixtureBinary::new("duplicate-prepared");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(12, 20, 5), now)
        .expect("request frame");
    let first = take_prepared(&mut coordinator);
    assert_eq!(first.frame_id, 1);

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });

    assert_eq!(coordinator.failure_count(), 1);
    assert!(coordinator.inflight().is_none());
}

#[test]
fn coordinator_requires_an_absolute_candidate_binary_path() {
    let result = UiWorkerCoordinator::new(
        CandidateIdentity {
            binary_path: PathBuf::from("target/debug/cosh-ui-worker"),
            candidate_version: TEST_VERSION.to_owned(),
        },
        TEST_NONCE,
        1,
        policy(3),
    );
    assert!(matches!(
        result,
        Err(CoordinatorError::CandidatePathNotAbsolute)
    ));
}

#[test]
fn coordinator_rejects_ready_from_a_different_candidate_version() {
    let fixture = FixtureBinary::new("wrong-version");
    let now = Instant::now();
    let mut coordinator = UiWorkerCoordinator::new(fixture.candidate(), TEST_NONCE, 1, policy(3))
        .expect("construct coordinator");
    coordinator.start(now).expect("start wrong-version fixture");

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });

    assert_eq!(coordinator.failure_count(), 1);
}

/// Wire v2 makes worker coordinates surface-relative, so a v1 worker's frame
/// would be painted at the wrong terminal rows. Rejection has to happen during
/// the handshake, before any frame is accepted, and it must consume failure
/// budget rather than being retried forever.
#[test]
fn coordinator_rejects_a_wire_v1_worker_during_handshake() {
    let fixture = FixtureBinary::new("schema-v1");
    let now = Instant::now();
    let mut coordinator = UiWorkerCoordinator::new(fixture.candidate(), TEST_NONCE, 1, policy(3))
        .expect("construct coordinator");
    coordinator.start(now).expect("start schema-v1 fixture");

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });

    assert!(
        !matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(_)),
        "a v1 worker must never reach Ready: {:?}",
        coordinator.lifecycle()
    );
    assert_eq!(coordinator.failure_count(), 1);
}

#[test]
fn coordinator_orderly_shutdown_waits_for_worker_exit() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, _) = start_ready(&fixture, 3);
    let pid = worker_pid(&coordinator).expect("active worker pid");
    coordinator
        .begin_shutdown("test complete".to_owned(), Instant::now())
        .expect("begin orderly shutdown");

    drive_until(&mut coordinator, |coordinator| coordinator.closed);

    assert!(process_is_absent(pid), "orderly shutdown must waitpid");
    assert_eq!(coordinator.failure_count(), 0);
    assert!(matches!(
        coordinator.lifecycle(),
        WorkerLifecycle::Unavailable
    ));
}

#[test]
fn coordinator_shutdown_timeout_kills_and_waits_for_stopped_worker() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, _) = start_ready(&fixture, 3);
    let pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(pid as i32, nix::libc::SIGSTOP);
    }
    let now = Instant::now();
    coordinator
        .begin_shutdown("stopped worker".to_owned(), now)
        .expect("begin shutdown");
    coordinator
        .tick(now + policy(3).shutdown_timeout + Duration::from_millis(1))
        .expect("force timed-out shutdown");

    assert!(coordinator.closed);
    assert!(
        process_is_absent(pid),
        "stopped worker must be killed and waited"
    );
    assert_eq!(coordinator.failure_count(), 0);
}

#[test]
fn coordinator_failure_limit_enters_interaction_ending() {
    let fixture = FixtureBinary::new("abort-after-ready");
    let (mut coordinator, _) = start_ready(&fixture, 1);

    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Unavailable)
    });

    assert_eq!(coordinator.failure_count(), 1);
    assert_eq!(coordinator.recovery_mode(), RecoveryMode::InteractionEnding);
}

#[test]
fn coordinator_failure_limit_clears_all_work_and_rejects_new_render() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 1);
    coordinator
        .request_render(render(1, 20, 5), now)
        .expect("request published frame");
    let published = take_prepared(&mut coordinator);
    publish_frame(&mut coordinator, &published, 10);
    drive_until(&mut coordinator, |coordinator| {
        coordinator.outbound.is_none()
    });

    coordinator
        .request_render(render(2, 21, 6), Instant::now())
        .expect("request active frame");
    coordinator
        .request_render(render(3, 22, 7), Instant::now())
        .expect("queue pending frame");
    assert!(coordinator.active_render.is_some());
    assert!(coordinator.pending_latest_request.is_some());
    assert!(coordinator.published.is_some());

    let pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(pid as i32, nix::libc::SIGKILL);
    }
    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Unavailable)
    });

    assert_eq!(coordinator.recovery_mode(), RecoveryMode::InteractionEnding);
    assert!(coordinator.active_render.is_none());
    assert!(coordinator.pending_latest_request.is_none());
    assert!(coordinator.outbound.is_none());
    assert!(coordinator.inflight.is_none());
    assert!(coordinator.published.is_none());
    assert!(matches!(
        coordinator.request_render(render(4, 23, 8), Instant::now()),
        Err(CoordinatorError::InvalidState)
    ));
}

#[test]
fn coordinator_initial_spawn_failure_allows_question_mark_retry() {
    let fixture = FixtureBinary::new("healthy");
    let mut permissions = fs::metadata(&fixture.path)
        .expect("stat fixture before removing execute permission")
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&fixture.path, permissions).expect("make initial candidate non-executable");
    let now = Instant::now();
    let mut coordinator = UiWorkerCoordinator::new(fixture.candidate(), TEST_NONCE, 1, policy(2))
        .expect("construct coordinator");

    let caller_result = (|| -> Result<(), CoordinatorError> {
        coordinator.start(now)?;
        assert_eq!(coordinator.failure_count(), 1);
        assert!(matches!(
            coordinator.lifecycle(),
            WorkerLifecycle::Rebuilding(2)
        ));
        let mut permissions = fs::metadata(&fixture.path)
            .expect("stat fixture before restoring execute permission")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&fixture.path, permissions)
            .expect("restore initial candidate execute permission");

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            coordinator.tick(Instant::now())?;
            if matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(2)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("caller did not reach the rebuilt generation");
    })();

    assert!(caller_result.is_ok(), "caller stopped at {caller_result:?}");
    assert_eq!(coordinator.failure_count(), 1);
    assert_eq!(coordinator.recovery_mode(), RecoveryMode::Normal);
}

#[test]
fn coordinator_missing_binary_spawn_failures_are_bounded_one_per_tick() {
    let directory = tempfile::tempdir().expect("create missing candidate directory");
    let candidate = CandidateIdentity {
        binary_path: directory.path().join("missing-ui-worker"),
        candidate_version: TEST_VERSION.to_owned(),
    };
    let now = Instant::now();
    let mut coordinator = UiWorkerCoordinator::new(candidate, TEST_NONCE, 1, policy(2))
        .expect("construct missing-binary coordinator");

    let caller_result = (|| -> Result<(), CoordinatorError> {
        coordinator.start(now)?;
        assert_eq!(coordinator.failure_count(), 1);
        assert!(matches!(
            coordinator.lifecycle(),
            WorkerLifecycle::Rebuilding(2)
        ));
        assert_eq!(coordinator.recovery_mode(), RecoveryMode::Normal);

        coordinator.tick(now)?;
        Ok(())
    })();

    assert!(caller_result.is_ok(), "caller stopped at {caller_result:?}");
    assert_eq!(coordinator.failure_count(), 2);
    assert!(matches!(
        coordinator.lifecycle(),
        WorkerLifecycle::Unavailable
    ));
    assert_eq!(coordinator.recovery_mode(), RecoveryMode::InteractionEnding);
}

#[test]
fn coordinator_rebuild_spawn_failure_allows_question_mark_retry() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, _) = start_ready(&fixture, 3);
    let mut permissions = fs::metadata(&fixture.path)
        .expect("stat fixture before removing execute permission")
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&fixture.path, permissions).expect("make rebuild candidate non-executable");
    let pid = worker_pid(&coordinator).expect("active worker pid");
    unsafe {
        nix::libc::kill(pid as i32, nix::libc::SIGKILL);
    }
    drive_until(&mut coordinator, |coordinator| {
        matches!(coordinator.lifecycle(), WorkerLifecycle::Rebuilding(2))
    });

    let caller_result = (|| -> Result<(), CoordinatorError> {
        coordinator.tick(Instant::now())?;
        assert_eq!(coordinator.failure_count(), 2);
        assert!(matches!(
            coordinator.lifecycle(),
            WorkerLifecycle::Rebuilding(3)
        ));
        let mut permissions = fs::metadata(&fixture.path)
            .expect("stat fixture before restoring execute permission")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&fixture.path, permissions)
            .expect("restore rebuild candidate execute permission");

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            coordinator.tick(Instant::now())?;
            if matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(3)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("caller did not reach the next rebuilt generation");
    })();

    assert!(caller_result.is_ok(), "caller stopped at {caller_result:?}");
    assert_eq!(coordinator.failure_count(), 2);
    assert_eq!(coordinator.recovery_mode(), RecoveryMode::Normal);
}

#[test]
fn coordinator_shutdown_finishes_partial_outbound_and_sends_shutdown_once() {
    let fixture = FixtureBinary::new("count-shutdown");
    let mut injected_policy = policy(3);
    injected_policy.protocol_limits.max_payload_bytes = 4 * 1024 * 1024;
    injected_policy.shutdown_timeout = Duration::from_secs(2);
    let candidate = CandidateIdentity {
        binary_path: fixture.path.clone(),
        candidate_version: "x".repeat(2 * 1024 * 1024),
    };
    let now = Instant::now();
    let mut coordinator = UiWorkerCoordinator::new(candidate, TEST_NONCE, 1, injected_policy)
        .expect("construct partial-outbound coordinator");
    coordinator.start(now).expect("spawn shutdown fixture");
    coordinator
        .begin_shutdown("partial outbound".to_owned(), now)
        .expect("begin shutdown while Hello is queued");
    coordinator.tick(now).expect("partially write queued Hello");
    let pending = coordinator
        .outbound
        .as_ref()
        .expect("pre-existing Hello remains queued");
    assert!(
        pending.offset > 0,
        "the pre-existing frame must make progress"
    );
    assert!(
        pending.offset < pending.bytes.len(),
        "the pre-existing frame must still be partial"
    );

    let observation_deadline = Instant::now() + Duration::from_secs(1);
    while fixture.shutdown_count() == 0 && Instant::now() < observation_deadline {
        coordinator
            .tick(Instant::now())
            .expect("finish outbound and send Shutdown");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(fixture.shutdown_count(), 1, "Shutdown must be delivered");

    for _ in 0..10 {
        coordinator
            .tick(Instant::now())
            .expect("wait after the sole Shutdown");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        fixture.shutdown_count(),
        1,
        "multiple shutdown ticks must not resend Shutdown"
    );

    coordinator
        .tick(now + injected_policy.shutdown_timeout + Duration::from_millis(1))
        .expect("force shutdown fixture after deadline");
    assert!(coordinator.closed);
}

#[test]
fn coordinator_backpressure_timeout_reaps_nonreading_worker() {
    let fixture = FixtureBinary::new("never-read");
    let mut injected_policy = policy(3);
    injected_policy.protocol_limits.max_payload_bytes = 4 * 1024 * 1024;
    injected_policy.backpressure_timeout = Duration::from_millis(10);
    let candidate = CandidateIdentity {
        binary_path: fixture.path.clone(),
        candidate_version: "x".repeat(2 * 1024 * 1024),
    };
    let now = Instant::now();
    let mut coordinator = UiWorkerCoordinator::new(candidate, TEST_NONCE, 1, injected_policy)
        .expect("construct backpressure coordinator");
    coordinator.start(now).expect("spawn nonreading worker");
    let pid = worker_pid(&coordinator).expect("nonreading worker pid");
    coordinator.tick(now).expect("fill worker socket");
    coordinator
        .tick(now + injected_policy.backpressure_timeout + Duration::from_millis(1))
        .expect("detect backpressure timeout");

    assert!(matches!(
        coordinator.lifecycle(),
        WorkerLifecycle::Rebuilding(2)
    ));
    assert_eq!(coordinator.failure_count(), 1);
    assert!(
        process_is_absent(pid),
        "backpressured worker must be reaped"
    );
}

#[test]
fn coordinator_delivers_resized_typed_partial_frame() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(77, 40, 12), now)
        .expect("request partial resize frame");

    let frame = take_prepared(&mut coordinator);

    assert_eq!(frame.geometry.columns, 40);
    assert_eq!(frame.geometry.rows, 12);
    assert!(
        !frame.full_repaint,
        "revision 77 fixture is a partial redraw"
    );
    assert_eq!(
        frame.draw_ops,
        vec![
            DrawOp::MoveTo { x: 1, y: 1 },
            DrawOp::Text("revision:77".to_owned()),
        ]
    );
}

#[test]
fn coordinator_accepts_multiple_pre_ack_heartbeats_until_current_key_arrives() {
    let fixture = FixtureBinary::new("queued-idle-heartbeats-before-ack");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(2, 20, 5), now)
        .expect("request heartbeat frame");
    let frame = take_prepared(&mut coordinator);
    publish_frame(&mut coordinator, &frame, 10);

    coordinator
        .tick(Instant::now())
        .expect("accept first queued pre-ack heartbeat");
    coordinator
        .tick(Instant::now())
        .expect("accept second queued pre-ack heartbeat");
    assert_eq!(coordinator.last_heartbeat_sequence(), Some(2));
    assert_eq!(coordinator.failure_count(), 0);
    assert!(matches!(coordinator.lifecycle(), WorkerLifecycle::Ready(1)));
    drive_until(&mut coordinator, |coordinator| {
        coordinator.last_heartbeat_sequence() == Some(3)
    });

    assert_eq!(coordinator.failure_count(), 0);
    assert!(coordinator.prior_heartbeat_key.is_none());
}

#[test]
fn coordinator_published_ack_observes_monotonic_heartbeat() {
    let fixture = FixtureBinary::new("healthy");
    let (mut coordinator, now) = start_ready(&fixture, 3);
    coordinator
        .request_render(render(2, 20, 5), now)
        .expect("request heartbeat frame");
    let frame = take_prepared(&mut coordinator);
    coordinator
        .begin_publishing(frame.frame_key(), 10)
        .expect("begin publication");
    coordinator
        .publish_succeeded(frame.frame_key(), 11, frame.geometry, Instant::now())
        .expect("ack published frame");

    drive_until(&mut coordinator, |coordinator| {
        coordinator.last_heartbeat_sequence() == Some(1)
    });

    assert_eq!(coordinator.failure_count(), 0);
    assert_eq!(
        coordinator.published(),
        Some(super::PublishedFrame {
            key: frame.frame_key(),
            publish_seq: 11,
            geometry: frame.geometry,
        })
    );
}

fn open_fd_count() -> usize {
    fs::read_dir("/proc/self/fd")
        .expect("read test process descriptors")
        .count()
}

#[test]
fn coordinator_five_hundred_rebuilds_leave_no_fd_or_zombie_growth() {
    let fixture = FixtureBinary::new("healthy");
    let baseline_fds = open_fd_count();
    let (mut coordinator, _) = start_ready(&fixture, 501);

    for failure in 1..=500u64 {
        let pid = worker_pid(&coordinator).expect("active worker pid");
        unsafe {
            nix::libc::kill(pid as i32, nix::libc::SIGKILL);
        }
        drive_until(&mut coordinator, |coordinator| {
            coordinator.failure_count() == failure
        });
        assert!(
            process_is_absent(pid),
            "generation {failure} left an unreaped pid"
        );
        assert!(matches!(
            coordinator.lifecycle(),
            WorkerLifecycle::Rebuilding(generation) if generation == failure + 1
        ));
        drive_until(&mut coordinator, |coordinator| {
            matches!(
                coordinator.lifecycle(),
                WorkerLifecycle::Ready(generation) if generation == failure + 1
            )
        });
    }

    coordinator
        .begin_shutdown("rebuild soak complete".to_owned(), Instant::now())
        .expect("close final generation");
    drive_until(&mut coordinator, |coordinator| coordinator.closed);
    drop(coordinator);
    let final_fds = open_fd_count();
    assert!(
        final_fds <= baseline_fds,
        "coordinator leaked descriptors: baseline={baseline_fds} final={final_fds}"
    );
}
