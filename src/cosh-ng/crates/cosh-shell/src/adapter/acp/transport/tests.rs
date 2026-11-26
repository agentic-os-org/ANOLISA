//! Process-level tests for the inbound transport queue.
//!
//! The peer is a scripted fixture, not a real Agent. These tests bound the
//! transport queue only; they say nothing about the UI queue or the Core SDK.

use std::fs;
use std::os::fd::RawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::super::host_runtime;
use super::super::host_shell::AcpHostExecuteRequest;
use super::{
    expose_received, queue_line, receive_line, set_nonblocking, write_cancellable, AgentTransport,
    Backlog, LineEvent,
};

const MAX_LINE: usize = 1024 * 1024;
const STDERR_TAIL: usize = 8192;

/// Frozen capacity oracle, independent of the production constants.
const ORACLE_MAX_FRAMES: usize = 4096;
const ORACLE_MAX_BYTES: usize = 8 * 1024 * 1024;

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos()
}

/// One legal bounded `session/update` notification per line.
const UPDATE_FRAME: &str = concat!(
    r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"host-session","#,
    r#""update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x"}}}}"#,
);

/// A peer that floods `frames` legal single-frame notifications, records whether
/// it finished, and then closes stdout so the reader reaches a definite end.
fn flooding_peer(frames: usize) -> (PathBuf, PathBuf) {
    flooding_peer_with_frame(frames, UPDATE_FRAME)
}

fn flooding_peer_with_frame(frames: usize, frame: &str) -> (PathBuf, PathBuf) {
    let stem = format!("cosh-acp-flood-{}-{}", std::process::id(), nonce());
    let script = std::env::temp_dir().join(format!("{stem}.sh"));
    let marker = std::env::temp_dir().join(format!("{stem}.done"));
    let body = format!(
        "for _ in $(seq 1 {frames}); do\n  printf '%s\\n' '{frame}' || exit 1\ndone\nprintf 'flooded\\n' > '{marker}'\nexit 0\n",
        frames = frames,
        frame = frame,
        marker = marker.display(),
    );
    fs::write(&script, format!("#!/bin/bash\n{body}\n")).expect("write flood fixture");
    let mut permissions = fs::metadata(&script)
        .expect("fixture metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod flood fixture");
    (script, marker)
}

fn execute_request() -> AcpHostExecuteRequest {
    AcpHostExecuteRequest {
        session_id: "host-session".to_string(),
        tool_call_id: "tool-flood".to_string(),
        command: "echo hi".to_string(),
        cwd: "/tmp".to_string(),
        expected_cwd: None,
        pty_instance: "pty-1".to_string(),
        hook_requires_approval: false,
        approval_id: None,
    }
}

/// Waits for the producer to reach an observable end without consuming anything:
/// the reader thread returned, either at the overload or at the peer's EOF.
/// Draining earlier would simulate a resumed consumer and hide the backlog.
fn wait_for_reader_end(transport: &AgentTransport) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if transport
            .reader
            .as_ref()
            .is_some_and(|reader| reader.is_finished())
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the reader reached no observable end within 60s");
}

/// One settled inspection of the queue, taken only after the producer ended.
fn inspect(lines: &mpsc::Receiver<LineEvent>) -> Vec<LineEvent> {
    let mut queued = Vec::new();
    while let Ok(event) = lines.try_recv() {
        queued.push(event);
    }
    queued
}

fn queued_bytes(queued: &[LineEvent]) -> usize {
    queued
        .iter()
        .map(|event| match event {
            LineEvent::Line(line) => line.len(),
            _ => 0,
        })
        .sum()
}

fn queued_lines(queued: &[LineEvent]) -> usize {
    queued
        .iter()
        .filter(|event| matches!(event, LineEvent::Line(_)))
        .count()
}

/// With the real foreground authority waiting on a human decision, the client
/// loop is not draining, so the inbound queue must still stay inside a fixed
/// capacity and byte bound and report the overload instead of growing.
///
/// Two different flood sizes are measured so the observation is a capacity
/// statement rather than one lucky number.
#[test]
fn a_pending_authority_keeps_the_inbound_queue_within_the_frozen_bound() {
    for frames in [20_000usize, 60_000usize] {
        let (script, marker) = flooding_peer(frames);
        let mut transport = AgentTransport::launch(
            &script.to_string_lossy(),
            &[],
            MAX_LINE,
            STDERR_TAIL,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("launch flooding peer");

        let (event_tx, event_rx) = mpsc::channel();
        let (_approval_tx, approval_rx) = mpsc::channel();
        let authority = host_runtime::authority(
            "run-flood".to_string(),
            event_tx,
            Arc::new(Mutex::new(approval_rx)),
            Arc::new(AtomicBool::new(false)),
        );
        let request = execute_request();
        let pending = std::thread::spawn(move || authority(&request));
        // The ask left, so the authority is now waiting for a decision that
        // never arrives: exactly the pending human review the loop blocks in.
        let ask = event_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the ask must reach the runtime");
        assert!(
            matches!(
                ask,
                Ok(crate::types::AgentEvent::ToolPermissionRequest { .. })
            ),
            "expected a permission ask, got {ask:?}"
        );

        wait_for_reader_end(&transport);
        // The first fact the resumed consumer sees must be the overload, not
        // one of the frames still queued in front of it.
        let first = transport
            .next_line(Duration::from_secs(1))
            .expect("inbound fact");
        let overload = match first {
            Some(LineEvent::Failed(message)) => message,
            other => panic!("the overload must outrank the queue, got {other:?}"),
        };
        assert!(
            overload.contains("backlog"),
            "the failure must name the inbound backlog, got {overload}"
        );
        let queued = inspect(&transport.lines);
        let bytes = queued_bytes(&queued);
        let lines = queued_lines(&queued);
        eprintln!(
            "FLOOD-BOUND frames={frames} queued={lines} bytes={bytes} per_frame={} events={} peer_done={}",
            UPDATE_FRAME.len(),
            queued.len(),
            marker.exists()
        );
        assert!(
            lines <= ORACLE_MAX_FRAMES,
            "the inbound queue must stay within {ORACLE_MAX_FRAMES} frames, got {lines}"
        );
        assert!(
            bytes <= ORACLE_MAX_BYTES,
            "the inbound queue must stay within {ORACLE_MAX_BYTES} bytes, got {bytes}"
        );
        assert!(
            queued
                .iter()
                .all(|event| matches!(event, LineEvent::Line(_))),
            "the overload must not be left behind the queued frames"
        );
        assert!(
            !marker.exists(),
            "a bounded reader must stop the peer from completing the flood"
        );

        // The authority is still pending: nothing timed the human review out.
        assert!(
            !pending.is_finished(),
            "the human review must not be timed out"
        );
        drop(pending);
        let started = Instant::now();
        transport.shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "shutdown must reclaim the reader within a bound, took {:?}",
            started.elapsed()
        );
        let _ = fs::remove_file(&script);
        let _ = fs::remove_file(&marker);
    }
}

/// The bound is a live queue capacity, not a cumulative total: a consumer that
/// keeps reading through `next_line` must pass a stream far larger than it.
#[test]
fn a_draining_consumer_passes_a_stream_larger_than_the_queue_bound() {
    let frames = 20_000usize;
    let (script, marker) = flooding_peer(frames);
    let mut transport = AgentTransport::launch(
        &script.to_string_lossy(),
        &[],
        MAX_LINE,
        STDERR_TAIL,
        Arc::new(AtomicBool::new(false)),
    )
    .expect("launch flooding peer");

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut lines = 0usize;
    let mut closed = false;
    while Instant::now() < deadline {
        match transport
            .next_line(Duration::from_millis(200))
            .expect("inbound fact")
        {
            Some(LineEvent::Line(_)) => lines += 1,
            Some(LineEvent::Closed) => {
                closed = true;
                break;
            }
            Some(LineEvent::Failed(message)) => {
                panic!("a keeping-up consumer must not be failed: {message}")
            }
            None => continue,
        }
    }
    eprintln!("FLOOD-DRAIN frames={frames} delivered={lines} closed={closed}");
    assert!(closed, "the peer must reach EOF within the bound");
    assert_eq!(
        lines, frames,
        "every frame must reach a keeping-up consumer"
    );
    assert!(marker.exists(), "the peer must finish the flood");
    transport.shutdown();
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&marker);
}

/// The reader can latch the overload while a receive is already waiting. The
/// selection applied to an already-received fact is exercised directly, so the
/// post-receive branch is covered without depending on thread scheduling.
#[test]
fn an_overload_latched_during_a_receive_outranks_the_received_frame() {
    let (sender, lines) = mpsc::channel();
    let backlog = Backlog {
        frames: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
        overloaded: AtomicBool::new(false),
    };
    let stop = r#"{"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn"}}"#.to_string();
    // Reserve occupancy through the production enqueue path, then take the fact
    // out of the channel exactly as a receive would.
    assert!(queue_line(&backlog, &sender, stop));
    assert_eq!(backlog.frames.load(Ordering::Relaxed), 1);
    let received = lines.try_recv().expect("the queued fact");
    // The reader latches the overload at this point, after the pre-receive check
    // has already passed and before the fact is exposed.
    backlog.overloaded.store(true, Ordering::Relaxed);
    let exposed = expose_received(&backlog, received);
    match exposed {
        LineEvent::Failed(message) => assert!(
            message.contains("backlog"),
            "the failure must name the inbound backlog, got {message}"
        ),
        other => panic!("the latched overload must outrank the received frame, got {other:?}"),
    }
    assert_eq!(backlog.frames.load(Ordering::Relaxed), 0);
    assert_eq!(backlog.bytes.load(Ordering::Relaxed), 0);
}

/// Without a latched overload the same selection must expose the frame itself,
/// so the priority rule cannot degenerate into failing every received fact.
#[test]
fn a_received_frame_without_an_overload_is_exposed_unchanged() {
    let (sender, lines) = mpsc::channel();
    let backlog = Backlog {
        frames: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
        overloaded: AtomicBool::new(false),
    };
    let stop = r#"{"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn"}}"#.to_string();
    assert!(queue_line(&backlog, &sender, stop.clone()));
    let received = lines.try_recv().expect("the queued fact");
    let exposed = expose_received(&backlog, received);
    match exposed {
        LineEvent::Line(line) => assert_eq!(line, stop),
        other => panic!("a healthy stream must expose the frame, got {other:?}"),
    }
    assert_eq!(backlog.frames.load(Ordering::Relaxed), 0);
    assert_eq!(backlog.bytes.load(Ordering::Relaxed), 0);
}

/// The pre-receive interleaving, forced by a gate instead of a sleep: the
/// overload is latched and the stop enqueued before the receiver exists, so the
/// stop is provably still queued when the overload is reported.
#[test]
fn an_overload_latched_before_the_receive_leaves_the_frame_queued() {
    let (sender, lines) = mpsc::channel();
    let lines = Arc::new(Mutex::new(lines));
    let backlog = Arc::new(Backlog {
        frames: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
        overloaded: AtomicBool::new(false),
    });
    backlog.overloaded.store(true, Ordering::Relaxed);
    assert!(queue_line(
        &backlog,
        &sender,
        r#"{"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn"}}"#.to_string(),
    ));
    assert_eq!(backlog.frames.load(Ordering::Relaxed), 1);
    let receiving = Arc::clone(&backlog);
    let receiving_lines = Arc::clone(&lines);
    let pending = std::thread::spawn(move || {
        let guard = receiving_lines.lock().expect("lines lock");
        receive_line(&guard, &receiving, Duration::from_secs(10))
    });
    let event = pending.join().expect("the receive must return");
    match event {
        Some(LineEvent::Failed(message)) => assert!(
            message.contains("backlog"),
            "the failure must name the inbound backlog, got {message}"
        ),
        other => panic!("the latched overload must outrank the queued frame, got {other:?}"),
    }
    assert_eq!(
        backlog.frames.load(Ordering::Relaxed),
        1,
        "the pre-receive check must not consume the queued frame"
    );
    let remaining = {
        let guard = lines.lock().expect("lines lock");
        receive_line(&guard, &backlog, Duration::from_millis(200))
    };
    assert!(
        matches!(remaining, Some(LineEvent::Line(_))),
        "the stop must still be queued, got {remaining:?}"
    );
    assert_eq!(backlog.frames.load(Ordering::Relaxed), 0);
    assert_eq!(backlog.bytes.load(Ordering::Relaxed), 0);
}

/// One legal `session/update` whose whole line is exactly `total_len` bytes, so
/// the byte bound can be exercised with frames still under the line limit.
fn large_update_frame(total_len: usize) -> String {
    let prefix = concat!(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"host-session","#,
        r#""update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""#,
    );
    let suffix = r#""}}}}"#;
    assert!(total_len > prefix.len() + suffix.len());
    format!(
        "{prefix}{}{suffix}",
        "x".repeat(total_len - prefix.len() - suffix.len())
    )
}

/// Small frames never reach the byte bound, so the byte branch needs its own
/// evidence: a few large legal frames must trip it while the frame count is
/// still far below the capacity bound.
#[test]
fn a_large_frame_flood_trips_the_byte_bound_before_the_frame_bound() {
    let frame_len = 512 * 1024;
    let frame = large_update_frame(frame_len);
    assert_eq!(frame.len(), frame_len);
    assert!(
        frame_len < MAX_LINE,
        "each frame must stay under the line limit"
    );
    let (script, marker) = flooding_peer_with_frame(40, &frame);
    let mut transport = AgentTransport::launch(
        &script.to_string_lossy(),
        &[],
        MAX_LINE,
        STDERR_TAIL,
        Arc::new(AtomicBool::new(false)),
    )
    .expect("launch large-frame peer");

    let (event_tx, event_rx) = mpsc::channel();
    let (_approval_tx, approval_rx) = mpsc::channel();
    let authority = host_runtime::authority(
        "run-large".to_string(),
        event_tx,
        Arc::new(Mutex::new(approval_rx)),
        Arc::new(AtomicBool::new(false)),
    );
    let request = execute_request();
    let pending = std::thread::spawn(move || authority(&request));
    let ask = event_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the ask must reach the runtime");
    assert!(matches!(
        ask,
        Ok(crate::types::AgentEvent::ToolPermissionRequest { .. })
    ));

    wait_for_reader_end(&transport);
    let first = transport
        .next_line(Duration::from_secs(1))
        .expect("inbound fact");
    let overload = match first {
        Some(LineEvent::Failed(message)) => message,
        other => panic!("the byte overload must outrank the queue, got {other:?}"),
    };
    assert!(overload.contains("backlog"), "got {overload}");
    let queued = inspect(&transport.lines);
    let bytes = queued_bytes(&queued);
    let lines = queued_lines(&queued);
    eprintln!(
        "FLOOD-BYTES per_frame={frame_len} queued={lines} bytes={bytes} peer_done={}",
        marker.exists()
    );
    assert!(
        lines < ORACLE_MAX_FRAMES,
        "the byte branch must trip first, got {lines} frames"
    );
    assert!(
        bytes <= ORACLE_MAX_BYTES,
        "the queued payload must stay within {ORACLE_MAX_BYTES} bytes, got {bytes}"
    );
    assert!(
        bytes + frame_len > ORACLE_MAX_BYTES,
        "the bound must be tight: {bytes} + {frame_len} must exceed {ORACLE_MAX_BYTES}"
    );
    assert!(!marker.exists(), "the bounded reader must stop the peer");
    assert!(
        !pending.is_finished(),
        "the human review must not be timed out"
    );
    drop(pending);
    let started = Instant::now();
    transport.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "shutdown must reclaim the reader within a bound, took {:?}",
        started.elapsed()
    );
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&marker);
}

/// A payload no default pipe capacity can hold, so a writer that is not drained
/// is provably still inside the write rather than merely slow.
const WRITE_PAYLOAD_BYTES: usize = 900_000;

/// A raw pipe with a non-blocking write end, matching what `launch` sets on the
/// Agent stdin. The read end stays open here, so a stalled write sees a full
/// pipe instead of a broken one.
fn owned_pipe() -> (RawFd, RawFd) {
    let mut descriptors = [0 as nix::libc::c_int; 2];
    assert_eq!(unsafe { nix::libc::pipe(descriptors.as_mut_ptr()) }, 0);
    set_nonblocking(descriptors[1]).expect("non-blocking write end");
    (descriptors[0], descriptors[1])
}

fn close_pipe(read_fd: RawFd, write_fd: RawFd) {
    unsafe {
        nix::libc::close(read_fd);
        nix::libc::close(write_fd);
    }
}

fn readable(read_fd: RawFd) -> bool {
    let mut descriptor = nix::libc::pollfd {
        fd: read_fd,
        events: nix::libc::POLLIN,
        revents: 0,
    };
    unsafe { nix::libc::poll(&mut descriptor, 1, 0) > 0 }
}

/// Reads exactly `wanted` bytes, blocking until they arrive.
fn read_exactly(read_fd: RawFd, wanted: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    while bytes.len() < wanted {
        let count = unsafe {
            nix::libc::read(
                read_fd,
                buffer.as_mut_ptr().cast(),
                buffer.len().min(wanted - bytes.len()),
            )
        };
        assert!(count > 0, "the write end must deliver the requested bytes");
        bytes.extend_from_slice(&buffer[..count as usize]);
    }
    bytes
}

/// Fills the pipe until the kernel refuses more, so a stalled write is a fact
/// rather than a timing assumption.
fn fill_pipe(write_fd: RawFd) -> usize {
    let filler = vec![b'f'; 8192];
    let mut filled = 0;
    loop {
        let count = unsafe { nix::libc::write(write_fd, filler.as_ptr().cast(), filler.len()) };
        if count < 0 {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock,
                "a full pipe must refuse more, not fail"
            );
            return filled;
        }
        filled += count as usize;
    }
}

/// A cancel must stop a write the pipe cannot accept, without appending a single
/// payload byte to what the peer already holds.
#[test]
fn a_cancel_stops_a_write_the_pipe_cannot_accept() {
    let (read_fd, write_fd) = owned_pipe();
    let filled = fill_pipe(write_fd);
    let cancelled = AtomicBool::new(true);
    let payload = vec![b'x'; WRITE_PAYLOAD_BYTES];
    let started = Instant::now();
    let outcome = write_cancellable(write_fd, &payload, &cancelled);
    let elapsed = started.elapsed();
    let drained = read_exactly(read_fd, filled);
    let more = readable(read_fd);
    close_pipe(read_fd, write_fd);
    eprintln!(
        "WRITE-CANCEL-FULL-PRESET elapsed_ms={} pipe_bytes={filled}",
        elapsed.as_millis()
    );
    let failure = outcome.expect_err("a write that cannot progress must end on cancellation");
    assert!(
        failure.message.contains("cancelled"),
        "the write must report cancellation, got {failure:?}"
    );
    assert_eq!(drained.len(), filled);
    assert!(
        drained.iter().all(|byte| *byte == b'f'),
        "no payload byte may be appended to a full pipe"
    );
    assert!(!more, "the cancelled write must not add bytes");
    assert!(
        elapsed < Duration::from_secs(1),
        "the cancel must be observed without waiting out the pipe, took {elapsed:?}"
    );
}

/// The cancel notification is written while the flag is already set. A pipe that
/// can still accept it must carry it, so cancellation never loses its protocol
/// frame just because the run is cancelled.
#[test]
fn a_cancelled_write_still_delivers_a_frame_the_pipe_can_accept() {
    let (read_fd, write_fd) = owned_pipe();
    let cancelled = AtomicBool::new(true);
    let payload = br#"{"jsonrpc":"2.0","method":"session/cancel"}"#.to_vec();
    let wanted = payload.len();
    let reader = std::thread::spawn(move || read_exactly(read_fd, wanted));
    let outcome = write_cancellable(write_fd, &payload, &cancelled);
    let received = reader.join().expect("cancel notification reader");
    close_pipe(read_fd, write_fd);
    outcome.expect("a writable pipe must still carry the cancel notification");
    assert_eq!(received, payload, "the cancel frame must arrive unchanged");
}

/// Without a cancel the chunked non-blocking write must still deliver a frame
/// larger than the pipe in full, so the bound never truncates a normal prompt.
#[test]
fn a_draining_reader_receives_a_frame_larger_than_the_pipe_in_full() {
    let (read_fd, write_fd) = owned_pipe();
    let cancelled = AtomicBool::new(false);
    let payload = vec![b'x'; WRITE_PAYLOAD_BYTES];
    let reader = std::thread::spawn(move || read_exactly(read_fd, WRITE_PAYLOAD_BYTES));
    let outcome = write_cancellable(write_fd, &payload, &cancelled);
    let received = reader.join().expect("draining reader");
    close_pipe(read_fd, write_fd);
    outcome.expect("a draining reader must accept the whole frame");
    assert_eq!(received.len(), payload.len());
    assert_eq!(received, payload, "every byte must arrive unchanged");
}

/// The reader consumes one bounded prefix and then stops, which proves the
/// write started and can no longer complete. The cancel must be observed at a
/// slice boundary rather than after the whole frame is delivered.
#[test]
fn a_cancel_latched_while_the_pipe_is_full_stops_the_write() {
    let (read_fd, write_fd) = owned_pipe();
    let cancelled = Arc::new(AtomicBool::new(false));
    let reader_cancelled = Arc::clone(&cancelled);
    let prefix = 4096;
    let reader = std::thread::spawn(move || {
        let bytes = read_exactly(read_fd, prefix);
        reader_cancelled.store(true, Ordering::SeqCst);
        bytes.len()
    });
    let payload = vec![b'x'; WRITE_PAYLOAD_BYTES];
    let started = Instant::now();
    let outcome = write_cancellable(write_fd, &payload, &cancelled);
    let elapsed = started.elapsed();
    let consumed = reader.join().expect("prefix reader");
    close_pipe(read_fd, write_fd);
    eprintln!(
        "WRITE-CANCEL-FULL elapsed_ms={} consumed={consumed} payload={}",
        elapsed.as_millis(),
        payload.len()
    );
    let failure = outcome.expect_err("the stalled write must end on cancellation");
    assert!(
        failure.message.contains("cancelled"),
        "the write must report cancellation, got {failure:?}"
    );
    assert_eq!(consumed, prefix, "only the drained prefix may be delivered");
    assert!(
        elapsed < Duration::from_secs(5),
        "the cancel must converge inside the grace, took {elapsed:?}"
    );
}

/// Without a cancel a slowly progressing write must still deliver every byte, so
/// the post-cancel budget never becomes a general write deadline.
#[test]
fn a_slowly_draining_reader_without_a_cancel_receives_every_byte() {
    let (read_fd, write_fd) = owned_pipe();
    let cancelled = AtomicBool::new(false);
    let payload = vec![b'x'; 128 * 1024];
    let wanted = payload.len();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        while bytes.len() < wanted {
            let step = read_exactly(read_fd, 512.min(wanted - bytes.len()));
            bytes.extend_from_slice(&step);
            std::thread::sleep(Duration::from_millis(5));
        }
        bytes
    });
    let outcome = write_cancellable(write_fd, &payload, &cancelled);
    let received = reader.join().expect("slow reader");
    close_pipe(read_fd, write_fd);
    outcome.expect("a slow reader without a cancel must not be cut off");
    assert_eq!(received.len(), payload.len());
    assert_eq!(
        received, payload,
        "a slow drain must not truncate the frame"
    );
}
