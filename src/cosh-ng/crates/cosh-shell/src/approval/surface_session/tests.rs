use std::io::{self, Write};

use nix::pty::Winsize;

use super::*;
use crate::ui::worker_protocol::{ApprovalActionView, ProtocolLimits, UiLanguage};

struct ControlledTerminal {
    size: Winsize,
    unreadable: bool,
    bytes: Vec<u8>,
    accepted: usize,
    remaining: Option<usize>,
}

impl Write for ControlledTerminal {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self
            .remaining
            .map_or(bytes.len(), |remaining| remaining.min(bytes.len()));
        if count == 0 && !bytes.is_empty() {
            return Err(io::Error::other("controlled partial write failed"));
        }
        self.bytes.extend_from_slice(&bytes[..count]);
        self.accepted += count;
        if let Some(remaining) = &mut self.remaining {
            *remaining -= count;
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SurfaceTerminal for ControlledTerminal {
    fn winsize(&mut self) -> io::Result<Winsize> {
        if self.unreadable {
            Err(io::Error::other("viewport unavailable"))
        } else {
            Ok(self.size)
        }
    }

    fn accepted(&self) -> usize {
        self.accepted
    }

    fn reset_accepted(&mut self) {
        self.accepted = 0;
    }
}

fn session() -> ApprovalSurfaceSession<ControlledTerminal> {
    let activation = ApprovalSliceActivation {
        worker_path: "/nonexistent/cosh-ui-worker".into(),
        limits: ProtocolLimits {
            max_payload_bytes: 1_048_576,
            max_draw_ops: 4096,
            max_text_bytes: 16_384,
        },
        max_surface_rows: 23,
        columns: 40,
        handshake_timeout: Duration::from_millis(1),
        render_timeout: Duration::from_millis(1),
        heartbeat_interval: Duration::from_millis(1),
        heartbeat_timeout: Duration::from_millis(2),
        backpressure_timeout: Duration::from_millis(1),
        shutdown_timeout: Duration::from_millis(1),
        failure_limit: 1,
    };
    let nonce = [1; 16];
    let coordinator = UiWorkerCoordinator::new(
        CandidateIdentity {
            binary_path: activation.worker_path.clone(),
            candidate_version: env!("CARGO_PKG_VERSION").into(),
        },
        nonce,
        1,
        activation.coordinator_policy(),
    )
    .expect("valid coordinator policy");
    let publisher = TerminalPublisher::new(
        Viewport {
            columns: 40,
            rows: 23,
        },
        activation.limits,
    );
    let router = InputRouter::new(nonce, 1, 0, 0);
    let sequence = router.input_sequence();
    ApprovalSurfaceSession {
        activation,
        action_key: ApprovalActionKey {
            shell_session_id: "shell".into(),
            run_id: "run".into(),
            approval_id: "approval".into(),
            request_id: "request".into(),
            tool_use_id: "tool".into(),
        },
        coordinator,
        gate: ApprovalSurfaceGate::new(ControlledTerminal {
            size: Winsize {
                ws_row: 30,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
            unreadable: false,
            bytes: Vec::new(),
            accepted: 0,
            remaining: None,
        }),
        publisher,
        router,
        sequence,
        live: true,
    }
}

#[test]
fn repair_source_tracks_partial_acquisition_and_owned_unpublished_rectangles() {
    for accepted in [0, 2] {
        let mut session = session();
        session.live = false;
        session.gate.term_mut().remaining = Some(accepted);
        let error = session.present(card()).unwrap_err();
        assert!(
            matches!(error, SurfaceError::Gate(GateError::Write { accepted: count, .. }) if count == accepted)
        );
        assert!(session.gate.rect().is_none());
        assert_eq!(session.gate.term_mut().bytes.len(), accepted);
        assert!(!session.is_live());
        assert_eq!(
            session.region_needs_repair(Some(&error)),
            accepted > 0,
            "{error:?}"
        );
    }
    let mut session = session();
    session.live = false;
    assert!(!session.region_needs_repair(None));
    session.activation.max_surface_rows = 1;
    let error = session.present(card()).unwrap_err();
    assert!(matches!(error, SurfaceError::TooTall { .. }));
    assert!(session.gate.term_mut().bytes.is_empty());
    assert!(!session.region_needs_repair(Some(&error)));
    session.gate.acquire(12).unwrap();
    assert!(!session.is_live());
    assert!(session.region_needs_repair(None));
    session.gate.withdraw().unwrap();
    assert!(session.gate.term_mut().accepted() > 0);
    assert!(
        !session.region_needs_repair(None),
        "successful withdrawal bytes must not imply a dirty region"
    );
}

fn card() -> ApprovalCardView {
    ApprovalCardView {
        revision: 2,
        language: UiLanguage::En,
        display_id: "approval".into(),
        kind: "tool".into(),
        risk: "high".into(),
        reason: "write".into(),
        subject: "Bash".into(),
        preview: "printf effect".into(),
        label: "Bash".into(),
        actions: vec![
            ApprovalActionView::Approve,
            ApprovalActionView::Deny,
            ApprovalActionView::Details,
        ],
        selected: ApprovalActionView::Approve,
        expanded: false,
        irrecoverable: false,
    }
}

#[test]
fn changed_or_unreadable_geometry_never_withdraws_the_old_rectangle() {
    for unreadable in [false, true] {
        let mut session = session();
        let rows = surface_rows_for(&RenderView::ApprovalCard(card()), 40);
        let old = session.gate.acquire(rows).expect("initial rectangle");
        let terminal = session.gate.term_mut();
        terminal.bytes.clear();
        terminal.size.ws_col = 47;
        terminal.unreadable = unreadable;
        assert!(session.present(card()).is_err());
        assert!(
            session.gate.term_mut().bytes.is_empty(),
            "an untrusted rectangle must not emit CUP or erase bytes"
        );
        assert_eq!(session.gate.rect(), Some(old));
        session.gate.relinquish_without_erasing();
    }
}

#[test]
fn a_height_change_with_stable_geometry_can_reacquire() {
    let mut session = session();
    let rows = surface_rows_for(&RenderView::ApprovalCard(card()), 40);
    session
        .gate
        .acquire(rows + 1)
        .expect("initial taller rectangle");
    session.gate.term_mut().bytes.clear();
    assert!(session.present(card()).is_err(), "no worker was started");
    assert_eq!(
        session.gate.rect().expect("replacement rectangle").height,
        rows
    );
    assert!(!session.gate.term_mut().bytes.is_empty());
    session.gate.relinquish_without_erasing();
}

fn prepared_frame(rect: SurfaceRect) -> PreparedFrame {
    use crate::ui::worker_protocol::{DrawOp, FrameGeometry};
    PreparedFrame {
        worker_generation: 1,
        frame_id: 2,
        capture_id: Some(7),
        draw_ops: vec![
            DrawOp::MoveTo { x: 0, y: 0 },
            DrawOp::Text("fresh frame".into()),
        ],
        geometry: FrameGeometry {
            columns: rect.columns,
            rows: rect.height,
            cursor_x: 11,
            cursor_y: 0,
        },
        full_repaint: false,
    }
}

#[test]
fn a_prepared_frame_never_publishes_after_geometry_becomes_untrusted() {
    for unreadable in [false, true] {
        let mut session = session();
        session.live = false;
        let rect = session.gate.acquire(10).expect("initial rectangle");
        let frame = prepared_frame(rect);
        session
            .coordinator
            .prepare_for_surface_publish_test(frame.frame_key());
        let terminal = session.gate.term_mut();
        terminal.bytes.clear();
        terminal.size.ws_col = 47;
        terminal.unreadable = unreadable;
        assert!(session.commit(frame, rect).is_err());
        assert!(
            session.gate.term_mut().bytes.is_empty(),
            "prepared bytes must not reach stale coordinates"
        );
        assert_eq!(session.router.published_geometry(), None);
        assert!(!session.live);
        session.gate.relinquish_without_erasing();
    }
}

#[test]
fn a_prepared_frame_on_stable_geometry_reaches_the_terminal() {
    let mut session = session();
    session.live = false;
    let rect = session.gate.acquire(10).expect("initial rectangle");
    let frame = prepared_frame(rect);
    let geometry = frame.geometry;
    session
        .coordinator
        .prepare_for_surface_publish_test(frame.frame_key());
    session.gate.term_mut().bytes.clear();
    assert!(
        session.commit(frame, rect).is_err(),
        "ack requires a real worker IPC"
    );
    let bytes = &session.gate.term_mut().bytes;
    assert!(bytes
        .windows(b"fresh frame".len())
        .any(|bytes| bytes == b"fresh frame"));
    assert_eq!(session.router.published_geometry(), Some(geometry));
    assert!(!session.live, "the missing IPC ack prevents a live session");
    session.gate.relinquish_without_erasing();
}
