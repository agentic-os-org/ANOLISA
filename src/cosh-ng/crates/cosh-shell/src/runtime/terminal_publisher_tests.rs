use std::io::{self, Write};
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;

use nix::libc;

use crate::raw_input::{CaptureAuthority, InputRouter, InputSequence};
use crate::ui::surface_frame::{SurfaceFrameError, SurfaceRect};
use crate::ui::worker_protocol::{
    DrawOp, DrawOpError, FrameGeometry, PreparedFrame, ProtocolLimits, RenderColor, RenderStyle,
    Viewport,
};

use super::terminal_publisher::{
    LegacyOwner, PublishError, TerminalPublisher, TerminalState, TrustedHostFrame,
};

const VIEWPORT: Viewport = Viewport {
    columns: 20,
    rows: 10,
};
const LIMITS: ProtocolLimits = ProtocolLimits {
    max_payload_bytes: 4096,
    max_draw_ops: 16,
    max_text_bytes: 8,
};
const ENCODED_TEXT_FRAME: &[u8] = b"\x1b[2;3Hhello";

struct FailAfterNWriter {
    bytes: Vec<u8>,
    remaining: usize,
    fail_flush: bool,
    flushes: usize,
}

impl FailAfterNWriter {
    fn new(remaining: usize, fail_flush: bool) -> Self {
        Self {
            bytes: Vec::new(),
            remaining,
            fail_flush,
            flushes: 0,
        }
    }
}

impl Write for FailAfterNWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::other("injected short write"));
        }
        let written = self.remaining.min(buf.len());
        self.bytes.extend_from_slice(&buf[..written]);
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.fail_flush {
            return Err(io::Error::other("injected flush failure"));
        }
        Ok(())
    }
}

struct ClosedFdWriter {
    fd: i32,
}

impl ClosedFdWriter {
    fn new() -> Self {
        let (stream, peer) = UnixStream::pair().expect("create test socketpair");
        drop(peer);
        let fd = stream.into_raw_fd();
        // Keep the numeric descriptor solely to prove the publisher handles a real EBADF.
        assert_eq!(unsafe { libc::close(fd) }, 0);
        Self { fd }
    }
}

impl Write for ClosedFdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = unsafe { libc::write(self.fd, buf.as_ptr().cast(), buf.len()) };
        if written < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(written as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingWriter {
    bytes: Vec<u8>,
    writes: usize,
    flushes: usize,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

struct SequencedWriter {
    bytes: Vec<u8>,
    flushes: usize,
    sequence: InputSequence,
}

impl SequencedWriter {
    fn new(sequence: InputSequence) -> Self {
        Self {
            bytes: Vec::new(),
            flushes: 0,
            sequence,
        }
    }
}

impl Write for SequencedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        self.sequence.next();
        Ok(())
    }
}

fn router_and_sequence() -> (InputRouter, InputSequence) {
    let router = InputRouter::new([7; 16], 3, 5, 8);
    let sequence = router.input_sequence();
    (router, sequence)
}

fn text_frame() -> PreparedFrame {
    PreparedFrame {
        worker_generation: 3,
        frame_id: 11,
        capture_id: Some(17),
        draw_ops: vec![
            DrawOp::MoveTo { x: 2, y: 1 },
            DrawOp::Text("hello".to_owned()),
        ],
        geometry: FrameGeometry {
            columns: VIEWPORT.columns,
            rows: VIEWPORT.rows,
            cursor_x: 7,
            cursor_y: 1,
        },
        full_repaint: false,
    }
}

fn frame_with(draw_ops: Vec<DrawOp>, geometry: FrameGeometry) -> PreparedFrame {
    PreparedFrame {
        worker_generation: 3,
        frame_id: 12,
        capture_id: Some(18),
        draw_ops,
        geometry,
        full_repaint: true,
    }
}

fn recovery_frame() -> PreparedFrame {
    PreparedFrame {
        worker_generation: 3,
        frame_id: 13,
        capture_id: Some(19),
        draw_ops: vec![
            DrawOp::SetStyle(RenderStyle::Reset),
            DrawOp::ClearRect {
                x: 0,
                y: 0,
                width: VIEWPORT.columns,
                height: VIEWPORT.rows,
            },
            DrawOp::MoveTo { x: 0, y: 0 },
            DrawOp::Text("ready".to_owned()),
        ],
        geometry: FrameGeometry {
            columns: VIEWPORT.columns,
            rows: VIEWPORT.rows,
            cursor_x: 5,
            cursor_y: 0,
        },
        full_repaint: true,
    }
}

fn make_publisher_dirty(
    publisher: &mut TerminalPublisher,
    router: &mut InputRouter,
    sequence: &InputSequence,
) {
    let mut output = FailAfterNWriter::new(3, false);
    let result = publisher.publish(&mut output, text_frame(), router, sequence);

    assert!(matches!(result, Err(PublishError::Output(_))));
    assert_eq!(output.bytes, b"\x1b[2");
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

fn assert_failed_publish_is_aborted(fail_after: usize, fail_flush: bool, expected_bytes: &[u8]) {
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = FailAfterNWriter::new(fail_after, fail_flush);

    let result = publisher.publish(&mut output, text_frame(), &mut router, &sequence);

    assert!(matches!(result, Err(PublishError::Output(_))));
    assert_eq!(output.bytes, expected_bytes);
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

fn assert_worker_frame_rejected(frame: PreparedFrame, expected: DrawOpError) {
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = RecordingWriter::default();

    let result = publisher.publish(&mut output, frame, &mut router, &sequence);

    assert!(matches!(
        result,
        Err(PublishError::InvalidDrawOps(actual)) if actual == expected
    ));
    assert_eq!(output.writes, 0);
    assert_eq!(output.flushes, 0);
    assert!(output.bytes.is_empty());
    assert_eq!(publisher.state(), TerminalState::Clean);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

#[test]
fn terminal_publisher_aborts_when_write_fails_inside_ansi_control() {
    // RED mutation: omit abort_publish and the Dirty transition on write_all error.
    assert_failed_publish_is_aborted(3, false, b"\x1b[2");
}

#[test]
fn terminal_publisher_aborts_when_write_fails_inside_text() {
    // RED mutation: treat a short text write as a committed frame.
    assert_failed_publish_is_aborted(8, false, b"\x1b[2;3Hhe");
}

#[test]
fn terminal_publisher_aborts_when_flush_fails() {
    // RED mutation: commit before flush or omit abort_publish on flush error.
    assert_failed_publish_is_aborted(usize::MAX, true, ENCODED_TEXT_FRAME);
}

#[test]
fn terminal_publisher_aborts_when_preencoded_output_is_closed() {
    // RED mutation: leak the publish guard when the first post-encode write fails.
    assert_failed_publish_is_aborted(0, false, b"");
}

#[test]
fn terminal_publisher_aborts_when_real_output_fd_is_closed() {
    // RED mutation: treat EBADF after pre-encoding as a publishable frame.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = ClosedFdWriter::new();

    let result = publisher.publish(&mut output, text_frame(), &mut router, &sequence);

    assert!(matches!(
        result,
        Err(PublishError::Output(ref error)) if error.raw_os_error() == Some(libc::EBADF)
    ));
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

#[test]
fn terminal_publisher_dirty_state_rejects_incremental_frame_without_io() {
    // RED mutation: allow begin_publish while DirtyNeedsFullRepaint for any valid frame.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    make_publisher_dirty(&mut publisher, &mut router, &sequence);
    let mut output = RecordingWriter::default();

    let result = publisher.publish(&mut output, text_frame(), &mut router, &sequence);

    assert!(matches!(result, Err(PublishError::RecoveryRepaintRequired)));
    assert_eq!(output.writes, 0);
    assert_eq!(output.flushes, 0);
    assert!(output.bytes.is_empty());
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

#[test]
fn terminal_publisher_dirty_state_rejects_forged_full_repaint_flag() {
    // RED mutation: trust PreparedFrame.full_repaint without a reset and full-viewport clear.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    make_publisher_dirty(&mut publisher, &mut router, &sequence);
    let mut output = RecordingWriter::default();
    let mut forged = text_frame();
    forged.full_repaint = true;

    let result = publisher.publish(&mut output, forged, &mut router, &sequence);

    assert!(matches!(result, Err(PublishError::RecoveryRepaintRequired)));
    assert_eq!(output.writes, 0);
    assert_eq!(output.flushes, 0);
    assert!(output.bytes.is_empty());
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

#[test]
fn terminal_publisher_verified_recovery_repaint_restores_clean_capture() {
    // RED mutation: reject every dirty-state frame, including a Host-verified recovery repaint.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    make_publisher_dirty(&mut publisher, &mut router, &sequence);
    let mut output = SequencedWriter::new(sequence.clone());

    let result = publisher.publish(&mut output, recovery_frame(), &mut router, &sequence);

    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => panic!("recovery repaint failed: {error}"),
    };
    assert!(output.bytes.starts_with(b"\x1b[0m\x1b[s\x1b[1;1H"));
    assert_eq!(output.flushes, 1);
    assert_eq!(receipt.frame_id, 13);
    assert_eq!(receipt.publish_seq, 1);
    assert_eq!(receipt.token.map(|token| token.capture_id), Some(19));
    assert_eq!(receipt.token.map(|token| token.publish_seq), Some(1));
    assert_eq!(router.published_geometry(), Some(receipt.geometry));
    assert_eq!(publisher.state(), TerminalState::Clean);
}

#[test]
fn terminal_publisher_failed_recovery_repaint_stays_dirty_without_capture() {
    // RED mutation: clear Dirty before recovery write, flush, and commit all succeed.
    for (fail_after, fail_flush) in [(4, false), (usize::MAX, true)] {
        let (mut router, sequence) = router_and_sequence();
        let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
        make_publisher_dirty(&mut publisher, &mut router, &sequence);
        let mut output = FailAfterNWriter::new(fail_after, fail_flush);

        let result = publisher.publish(&mut output, recovery_frame(), &mut router, &sequence);

        assert!(matches!(result, Err(PublishError::Output(_))));
        assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
        assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
        assert_eq!(router.published_geometry(), None);
    }
}

#[test]
fn terminal_publisher_rejects_esc_text_before_terminal_write() {
    // RED mutation: remove the control-character validation gate.
    let text = "\x1b[31mred".to_owned();
    assert_worker_frame_rejected(
        frame_with(
            vec![DrawOp::Text(text)],
            FrameGeometry {
                columns: VIEWPORT.columns,
                rows: VIEWPORT.rows,
                cursor_x: 0,
                cursor_y: 0,
            },
        ),
        DrawOpError::ControlCharacter,
    );
}

#[test]
fn terminal_publisher_rejects_osc_text_before_terminal_write() {
    // RED mutation: pass OSC-bearing Text directly to the terminal encoder.
    let text = "\x1b]8;;https://example.invalid\x07link".to_owned();
    assert_worker_frame_rejected(
        frame_with(
            vec![DrawOp::Text(text)],
            FrameGeometry {
                columns: VIEWPORT.columns,
                rows: VIEWPORT.rows,
                cursor_x: 0,
                cursor_y: 0,
            },
        ),
        DrawOpError::ControlCharacter,
    );
}

#[test]
fn terminal_publisher_rejects_dsr_text_before_terminal_write() {
    // RED mutation: pass DSR-bearing Text directly to the terminal encoder.
    let text = "\x1b[6n".to_owned();
    assert_worker_frame_rejected(
        frame_with(
            vec![DrawOp::Text(text)],
            FrameGeometry {
                columns: VIEWPORT.columns,
                rows: VIEWPORT.rows,
                cursor_x: 0,
                cursor_y: 0,
            },
        ),
        DrawOpError::ControlCharacter,
    );
}

#[test]
fn terminal_publisher_rejects_out_of_bounds_move_before_terminal_write() {
    // RED mutation: remove viewport checking for MoveTo.
    assert_worker_frame_rejected(
        frame_with(
            vec![DrawOp::MoveTo {
                x: VIEWPORT.columns,
                y: 0,
            }],
            FrameGeometry {
                columns: VIEWPORT.columns,
                rows: VIEWPORT.rows,
                cursor_x: 0,
                cursor_y: 0,
            },
        ),
        DrawOpError::OutOfBounds {
            x: VIEWPORT.columns,
            y: 0,
            columns: VIEWPORT.columns,
            rows: VIEWPORT.rows,
        },
    );
}

#[test]
fn terminal_publisher_rejects_oversized_text_before_terminal_write() {
    // RED mutation: stop threading ProtocolLimits into Text validation.
    assert_worker_frame_rejected(
        frame_with(
            vec![DrawOp::Text("123456789".to_owned())],
            FrameGeometry {
                columns: VIEWPORT.columns,
                rows: VIEWPORT.rows,
                cursor_x: 9,
                cursor_y: 0,
            },
        ),
        DrawOpError::TextTooLong { len: 9, max: 8 },
    );
}

#[test]
fn terminal_publisher_rejects_forged_geometry_before_terminal_write() {
    // RED mutation: trust PreparedFrame.geometry instead of recomputing it.
    let forged = FrameGeometry {
        columns: VIEWPORT.columns + 1,
        rows: VIEWPORT.rows,
        cursor_x: 4,
        cursor_y: 1,
    };
    assert_worker_frame_rejected(
        frame_with(
            vec![DrawOp::MoveTo { x: 2, y: 1 }, DrawOp::Text("ok".to_owned())],
            forged,
        ),
        DrawOpError::GeometryMismatch {
            expected: VIEWPORT,
            actual: forged,
        },
    );
}

#[test]
fn terminal_publisher_encodes_then_commits_worker_frame_after_flush() {
    // RED mutations: emit worker text verbatim, sample input before flush, or skip commit.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = SequencedWriter::new(sequence.clone());
    let frame = PreparedFrame {
        worker_generation: 3,
        frame_id: 21,
        capture_id: Some(34),
        draw_ops: vec![
            DrawOp::MoveTo { x: 2, y: 1 },
            DrawOp::SetStyle(RenderStyle::Bold),
            DrawOp::SetStyle(RenderStyle::Foreground(RenderColor::Green)),
            DrawOp::Text("ok".to_owned()),
            DrawOp::SetStyle(RenderStyle::Reset),
            DrawOp::CursorVisible(false),
        ],
        geometry: FrameGeometry {
            columns: VIEWPORT.columns,
            rows: VIEWPORT.rows,
            cursor_x: 4,
            cursor_y: 1,
        },
        full_repaint: true,
    };

    let result = publisher.publish(&mut output, frame, &mut router, &sequence);

    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => panic!("worker publish failed: {error}"),
    };
    assert_eq!(output.bytes, b"\x1b[2;3H\x1b[1m\x1b[32mok\x1b[0m\x1b[?25l");
    assert_eq!(output.flushes, 1);
    assert_eq!(receipt.frame_id, 21);
    assert_eq!(receipt.publish_seq, 1);
    assert_eq!(receipt.geometry.cursor_x, 4);
    assert_eq!(receipt.token.map(|token| token.capture_id), Some(34));
    assert_eq!(receipt.token.map(|token| token.publish_seq), Some(1));
    assert_eq!(router.published_geometry(), Some(receipt.geometry));
    assert_eq!(publisher.state(), TerminalState::Clean);
}

#[test]
fn terminal_publisher_trusted_host_frame_uses_same_publish_barrier() {
    // RED mutations: bypass begin/commit or sample input before the trusted bytes flush.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = SequencedWriter::new(sequence.clone());
    let geometry = FrameGeometry {
        columns: VIEWPORT.columns,
        rows: VIEWPORT.rows,
        cursor_x: 6,
        cursor_y: 2,
    };
    let frame = TrustedHostFrame::new_for_test(
        LegacyOwner::Approval,
        3,
        55,
        Some(89),
        b"\x1b[3;1Hallow?".to_vec(),
        geometry,
    );

    let result = publisher.publish_trusted_host_frame(&mut output, frame, &mut router, &sequence);

    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => panic!("trusted Host publish failed: {error}"),
    };
    assert_eq!(output.bytes, b"\x1b[3;1Hallow?");
    assert_eq!(output.flushes, 1);
    assert_eq!(receipt.frame_id, 55);
    assert_eq!(receipt.publish_seq, 1);
    assert_eq!(receipt.geometry, geometry);
    assert_eq!(receipt.token.map(|token| token.capture_id), Some(89));
    assert_eq!(receipt.token.map(|token| token.publish_seq), Some(1));
    assert_eq!(router.published_geometry(), Some(geometry));
}

#[test]
fn terminal_publisher_trusted_host_write_failure_aborts_guard() {
    // RED mutation: give the migration adapter a direct-write bypass around the transaction.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = FailAfterNWriter::new(4, false);
    let frame = TrustedHostFrame::new_for_test(
        LegacyOwner::Question,
        3,
        56,
        Some(90),
        b"trusted-host-frame".to_vec(),
        FrameGeometry {
            columns: VIEWPORT.columns,
            rows: VIEWPORT.rows,
            cursor_x: 0,
            cursor_y: 0,
        },
    );

    let result = publisher.publish_trusted_host_frame(&mut output, frame, &mut router, &sequence);

    assert!(matches!(result, Err(PublishError::Output(_))));
    assert_eq!(output.bytes, b"trus");
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

#[test]
fn terminal_publisher_dirty_state_rejects_unverifiable_trusted_bytes() {
    // RED mutation: let opaque trusted bytes recover dirty state without typed repaint proof.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    make_publisher_dirty(&mut publisher, &mut router, &sequence);
    let mut output = RecordingWriter::default();
    let frame = TrustedHostFrame::new_for_test(
        LegacyOwner::Auth,
        3,
        57,
        Some(91),
        b"opaque-host-frame".to_vec(),
        FrameGeometry {
            columns: VIEWPORT.columns,
            rows: VIEWPORT.rows,
            cursor_x: 0,
            cursor_y: 0,
        },
    );

    let result = publisher.publish_trusted_host_frame(&mut output, frame, &mut router, &sequence);

    assert!(matches!(result, Err(PublishError::RecoveryRepaintRequired)));
    assert_eq!(output.writes, 0);
    assert_eq!(output.flushes, 0);
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}

#[test]
fn terminal_publisher_trusted_host_frame_has_no_public_production_construction() {
    // RED mutation: expose any TrustedHostFrame field or production impl to arbitrary modules.
    let source = include_str!("terminal_publisher.rs");
    let after_declaration = match source.split_once("pub struct TrustedHostFrame {") {
        Some((_, after)) => after,
        None => panic!("TrustedHostFrame declaration missing"),
    };
    let fields = match after_declaration.split_once("}\n\n") {
        Some((fields, _)) => fields,
        None => panic!("TrustedHostFrame declaration is not closed"),
    };
    let production_source = source
        .split_once("#[cfg(test)]\nimpl TrustedHostFrame")
        .map_or(source, |(production, _)| production);

    assert!(!fields.lines().any(|line| {
        let field = line.trim_start();
        field.starts_with("pub ") || field.starts_with("pub(")
    }));
    assert!(!production_source.contains("impl TrustedHostFrame"));
    assert_eq!(source.matches("impl TrustedHostFrame").count(), 1);
}

/// A 20x4 surface anchored on 0-based viewport row 6, i.e. the bottom four rows
/// of a 10-row terminal. Its sentinel is the surface's last row.
const SURFACE_RECT: SurfaceRect = SurfaceRect {
    anchor_row: 6,
    columns: 20,
    height: 4,
};

const SURFACE_VIEWPORT: Viewport = Viewport {
    columns: SURFACE_RECT.columns,
    rows: SURFACE_RECT.height,
};

fn surface_frame(
    draw_ops: Vec<DrawOp>,
    cursor: (u16, u16),
    capture_id: Option<u64>,
) -> PreparedFrame {
    PreparedFrame {
        worker_generation: 3,
        frame_id: 31,
        capture_id,
        draw_ops,
        geometry: FrameGeometry {
            columns: SURFACE_VIEWPORT.columns,
            rows: SURFACE_VIEWPORT.rows,
            cursor_x: cursor.0,
            cursor_y: cursor.1,
        },
        full_repaint: true,
    }
}

#[test]
fn publish_surface_anchors_the_worker_frame_in_the_host_rectangle() {
    // RED mutations: treat surface coordinates as viewport-absolute (drops
    // anchor_row), skip the per-row CR, or omit the tail reset/cursor/sentinel.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = SequencedWriter::new(sequence.clone());
    let frame = surface_frame(
        vec![
            DrawOp::MoveTo { x: 2, y: 1 },
            DrawOp::SetStyle(RenderStyle::Bold),
            DrawOp::Text("ok".to_owned()),
            DrawOp::SetStyle(RenderStyle::Reset),
        ],
        (4, 1),
        Some(34),
    );

    let receipt = publisher
        .publish_surface(&mut output, frame, SURFACE_RECT, &mut router, &sequence)
        .expect("surface publish");

    assert_eq!(
        output.bytes,
        b"\x1b[8;3H\x1b[1mok\x1b[0m\r\x1b[0m\x1b[?25l\x1b[10;1H"
    );
    assert_eq!(output.flushes, 1);
    assert_eq!(receipt.frame_id, 31);
    assert_eq!(receipt.token.map(|token| token.capture_id), Some(34));
    assert_eq!(receipt.geometry.columns, SURFACE_VIEWPORT.columns);
    assert_eq!(receipt.geometry.rows, SURFACE_VIEWPORT.rows);
    assert_eq!(router.published_geometry(), Some(receipt.geometry));
    assert_eq!(
        router.authority(),
        Some(CaptureAuthority::Published(
            receipt.token.expect("capture token")
        ))
    );
    assert_eq!(publisher.state(), TerminalState::Clean);
}

/// The rectangle, not the publisher's viewport, decides what may be written: a
/// frame that validated against the full terminal must not be published into a
/// smaller surface just because the publisher was constructed with it.
#[test]
fn publish_surface_rejects_a_frame_built_for_a_wider_viewport() {
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = RecordingWriter::default();
    let frame = PreparedFrame {
        geometry: FrameGeometry {
            columns: VIEWPORT.columns,
            rows: VIEWPORT.rows,
            cursor_x: 4,
            cursor_y: 1,
        },
        ..surface_frame(
            vec![DrawOp::MoveTo { x: 2, y: 1 }, DrawOp::Text("ok".to_owned())],
            (4, 1),
            Some(34),
        )
    };

    let result =
        publisher.publish_surface(&mut output, frame, SURFACE_RECT, &mut router, &sequence);

    assert!(
        matches!(
            result,
            Err(PublishError::InvalidDrawOps(
                DrawOpError::GeometryMismatch { .. }
            ))
        ),
        "{result:?}"
    );
    assert!(output.bytes.is_empty(), "no byte may be written");
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
}

/// Cursor policy belongs to the Host. A worker that smuggles a cursor op is
/// refused as a whole frame, and the refusal must happen before the publish
/// guard opens so the router is never left without an input owner.
#[test]
fn publish_surface_rejects_worker_cursor_control_without_opening_a_guard() {
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = RecordingWriter::default();
    let frame = surface_frame(
        vec![
            DrawOp::MoveTo { x: 2, y: 1 },
            DrawOp::Text("ok".to_owned()),
            DrawOp::CursorVisible(true),
        ],
        (4, 1),
        Some(34),
    );

    let result =
        publisher.publish_surface(&mut output, frame, SURFACE_RECT, &mut router, &sequence);

    assert!(
        matches!(
            result,
            Err(PublishError::OutsideSurface(
                SurfaceFrameError::WorkerControlledCursor
            ))
        ),
        "a worker cursor op must be refused as a whole frame"
    );
    assert!(output.bytes.is_empty(), "{:?}", output.bytes);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(publisher.state(), TerminalState::Clean);
}

#[test]
fn publish_surface_recovers_a_dirty_surface_with_a_verified_rectangle_repaint() {
    // RED mutation: accept any full_repaint flag, or clear the terminal instead
    // of the rectangle.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    make_publisher_dirty(&mut publisher, &mut router, &sequence);
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);

    let mut output = SequencedWriter::new(sequence.clone());
    let frame = surface_frame(
        vec![
            DrawOp::SetStyle(RenderStyle::Reset),
            DrawOp::ClearRect {
                x: 0,
                y: 0,
                width: SURFACE_RECT.columns,
                height: SURFACE_RECT.height,
            },
            DrawOp::MoveTo { x: 0, y: 0 },
            DrawOp::Text("ready".to_owned()),
        ],
        (5, 0),
        Some(35),
    );

    let receipt = publisher
        .publish_surface(&mut output, frame, SURFACE_RECT, &mut router, &sequence)
        .expect("verified rectangle repaint must recover the surface");

    assert_eq!(publisher.state(), TerminalState::Clean);
    assert!(receipt.token.is_some());
    let text = String::from_utf8_lossy(&output.bytes);
    assert!(
        text.contains("\x1b[20X"),
        "one row erase per surface row: {text:?}"
    );
    assert!(
        text.contains("\x1b[7;1H"),
        "clear starts at the anchor: {text:?}"
    );
}

/// A frame claiming `full_repaint` while clearing the terminal viewport rather
/// than the surface proves nothing about the rectangle, so it must not recover.
#[test]
fn publish_surface_rejects_a_repaint_that_clears_outside_the_rectangle() {
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    make_publisher_dirty(&mut publisher, &mut router, &sequence);

    let mut output = RecordingWriter::default();
    let frame = surface_frame(
        vec![
            DrawOp::SetStyle(RenderStyle::Reset),
            DrawOp::ClearRect {
                x: 0,
                y: 0,
                width: VIEWPORT.columns,
                height: VIEWPORT.rows,
            },
        ],
        (0, 0),
        Some(36),
    );

    let result =
        publisher.publish_surface(&mut output, frame, SURFACE_RECT, &mut router, &sequence);

    assert!(
        matches!(result, Err(PublishError::InvalidDrawOps(_))),
        "an out-of-rectangle clear must be refused by validation: {result:?}"
    );
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
}

#[test]
fn publish_surface_write_failure_aborts_the_guard_and_marks_the_surface_dirty() {
    // RED mutation: keep the capture token after a partial write.
    let (mut router, sequence) = router_and_sequence();
    let mut publisher = TerminalPublisher::new(VIEWPORT, LIMITS);
    let mut output = FailAfterNWriter::new(3, false);
    let frame = surface_frame(
        vec![DrawOp::MoveTo { x: 2, y: 1 }, DrawOp::Text("ok".to_owned())],
        (4, 1),
        Some(34),
    );

    let result =
        publisher.publish_surface(&mut output, frame, SURFACE_RECT, &mut router, &sequence);

    assert!(matches!(result, Err(PublishError::Output(_))), "{result:?}");
    assert_eq!(output.bytes, b"\x1b[8");
    assert_eq!(publisher.state(), TerminalState::DirtyNeedsFullRepaint);
    assert_eq!(router.authority(), Some(CaptureAuthority::Legacy));
    assert_eq!(router.published_geometry(), None);
}
