//! Host-owned terminal publish transaction for worker and trusted migration frames.
//!
//! Worker output reaches the terminal only after every typed draw operation and
//! its geometry have been validated and pre-encoded. Once publishing begins, an
//! output or flush failure always aborts the input guard and marks the terminal
//! dirty; only a completed flush may mint a capture token.

use std::io::{self, Write};

use crate::raw_input::{InputRouter, InputSequence, PublishedCaptureToken};
use crate::ui::surface_frame::{encode_surface_frame, SurfaceFrameError, SurfaceRect};
use crate::ui::worker_protocol::{
    recompute_geometry, validate_prepared_frame, DrawOp, DrawOpError, FrameGeometry, FrameKey,
    PreparedFrame, ProtocolLimits, RenderColor, RenderStyle, Viewport, WorkerGeneration,
};

/// Whether the Host can safely continue incremental terminal rendering.
///
/// Owned here rather than in `runtime::terminal` so the publish transaction and
/// its recovery state stay in one place; the legacy fault-prompt primitive is
/// replaced by the typed recovery path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalState {
    /// The last publish completed without an output error.
    Clean,
    /// Output may have stopped mid-frame and requires a complete repaint.
    DirtyNeedsFullRepaint,
}

/// Compile-time whitelist of Host render owners allowed to use the temporary
/// trusted-frame migration adapter. Worker and IPC types cannot name an owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyOwner {
    /// Tool approval cards.
    Approval,
    /// Interactive questions.
    Question,
    /// Authentication prompts.
    Auth,
    /// Task forms and snapshots.
    Task,
    /// Session panels.
    Session,
    /// Settings and mode panels.
    Settings,
    /// Hook actions and consultations.
    Hook,
    /// Evidence panels.
    Evidence,
    /// Agent streaming output.
    AgentStream,
}

/// Host-rendered bytes accepted only by the temporary legacy migration path.
///
/// The closed [`LegacyOwner`] marker prevents a worker protocol message from
/// being passed directly to this API. The adapter still uses the same publish
/// and input-sequence transaction as a typed worker frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedHostFrame {
    owner: LegacyOwner,
    worker_generation: WorkerGeneration,
    frame_id: u64,
    capture_id: Option<u64>,
    bytes: Vec<u8>,
    geometry: FrameGeometry,
}

#[cfg(test)]
impl TrustedHostFrame {
    pub(super) fn new_for_test(
        owner: LegacyOwner,
        worker_generation: WorkerGeneration,
        frame_id: u64,
        capture_id: Option<u64>,
        bytes: Vec<u8>,
        geometry: FrameGeometry,
    ) -> Self {
        Self {
            owner,
            worker_generation,
            frame_id,
            capture_id,
            bytes,
            geometry,
        }
    }
}

/// Evidence that a frame was flushed and its capture transaction resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishReceipt {
    /// Capability minted for a captured frame, or `None` without a capture id.
    pub token: Option<PublishedCaptureToken>,
    /// Published frame id.
    pub frame_id: u64,
    /// Input sequence sampled only after the terminal flush completed.
    pub publish_seq: u64,
    /// Host-validated or Host-produced geometry committed to the input router.
    pub geometry: FrameGeometry,
}

/// Failures that prevent a frame from being committed as published.
#[derive(Debug)]
pub enum PublishError {
    /// A worker draw operation or worker-reported geometry failed validation.
    InvalidDrawOps(DrawOpError),
    /// A validated frame still could not be placed inside the Host rectangle.
    OutsideSurface(SurfaceFrameError),
    /// A dirty terminal received a frame without Host-verifiable recovery ops.
    RecoveryRepaintRequired,
    /// Writing or flushing the terminal failed after the publish guard began.
    Output(io::Error),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDrawOps(error) => write!(f, "invalid draw operations: {error}"),
            Self::OutsideSurface(error) => write!(f, "frame does not fit the surface: {error}"),
            Self::RecoveryRepaintRequired => {
                write!(f, "terminal requires a verified full-viewport repaint")
            }
            Self::Output(error) => write!(f, "terminal output failed: {error}"),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidDrawOps(error) => Some(error),
            Self::OutsideSurface(error) => Some(error),
            Self::RecoveryRepaintRequired => None,
            Self::Output(error) => Some(error),
        }
    }
}

/// Host terminal writer that validates, flushes, and commits each frame as one
/// input-capture transaction.
pub struct TerminalPublisher {
    viewport: Viewport,
    limits: ProtocolLimits,
    state: TerminalState,
}

struct EncodedFrame<'a> {
    key: FrameKey,
    geometry: FrameGeometry,
    bytes: &'a [u8],
}

impl TerminalPublisher {
    /// Creates a clean publisher with Host-injected viewport and protocol limits.
    #[must_use]
    pub const fn new(viewport: Viewport, limits: ProtocolLimits) -> Self {
        Self {
            viewport,
            limits,
            state: TerminalState::Clean,
        }
    }

    /// Returns whether the terminal is safe for incremental rendering.
    #[must_use]
    pub const fn state(&self) -> TerminalState {
        self.state
    }

    /// Validates and publishes one typed worker frame.
    ///
    /// Validation, geometry recomputation, recovery proof, and ANSI encoding all
    /// finish before input ownership changes or terminal I/O begins. After
    /// `begin_publish`, a write or flush error explicitly consumes the guard
    /// through [`InputRouter::abort_publish`].
    ///
    /// # Errors
    /// Returns [`PublishError::InvalidDrawOps`] without terminal I/O when the
    /// worker frame is unsafe, [`PublishError::RecoveryRepaintRequired`] when a
    /// dirty terminal cannot verify a full repaint, or [`PublishError::Output`]
    /// after aborting the publish guard when the terminal write or flush fails.
    pub fn publish<W: Write>(
        &mut self,
        output: &mut W,
        frame: PreparedFrame,
        input_router: &mut InputRouter,
        input_sequence: &InputSequence,
    ) -> Result<PublishReceipt, PublishError> {
        validate_prepared_frame(&frame, self.viewport, &self.limits)
            .map_err(PublishError::InvalidDrawOps)?;
        let geometry = recompute_geometry(&frame.draw_ops, self.viewport, &self.limits)
            .map_err(PublishError::InvalidDrawOps)?;
        let recovery_repaint = self.state == TerminalState::DirtyNeedsFullRepaint
            && is_verified_recovery_repaint(&frame, self.viewport);
        if self.state == TerminalState::DirtyNeedsFullRepaint && !recovery_repaint {
            return Err(PublishError::RecoveryRepaintRequired);
        }
        let encoded = encode_draw_ops(&frame.draw_ops);
        let receipt = self.publish_encoded(
            output,
            EncodedFrame {
                key: frame.frame_key(),
                geometry,
                bytes: &encoded,
            },
            input_router,
            input_sequence,
        )?;
        if recovery_repaint {
            self.state = TerminalState::Clean;
        }
        Ok(receipt)
    }

    /// Publishes one worker frame into a Host-owned rectangle of the viewport.
    ///
    /// Wire v2 frames carry surface-relative coordinates, so the frame is
    /// validated against the rectangle's own dimensions and then translated to
    /// absolute positions by [`encode_surface_frame`]. The rectangle is the
    /// authority on what may be written: a frame that validated against a wider
    /// viewport is still refused here rather than clipped silently.
    ///
    /// # Errors
    /// Returns [`PublishError::InvalidDrawOps`] when the frame is unsafe for the
    /// rectangle's viewport, [`PublishError::OutsideSurface`] when a validated
    /// frame still addresses a cell outside the rectangle or carries a
    /// worker-controlled cursor op, [`PublishError::RecoveryRepaintRequired`]
    /// when a dirty surface cannot verify a full repaint, or
    /// [`PublishError::Output`] after aborting the publish guard.
    pub fn publish_surface<W: Write>(
        &mut self,
        output: &mut W,
        frame: PreparedFrame,
        rect: SurfaceRect,
        input_router: &mut InputRouter,
        input_sequence: &InputSequence,
    ) -> Result<PublishReceipt, PublishError> {
        let surface_viewport = Viewport {
            columns: rect.columns,
            rows: rect.height,
        };
        validate_prepared_frame(&frame, surface_viewport, &self.limits)
            .map_err(PublishError::InvalidDrawOps)?;
        let geometry = recompute_geometry(&frame.draw_ops, surface_viewport, &self.limits)
            .map_err(PublishError::InvalidDrawOps)?;
        let recovery_repaint = self.state == TerminalState::DirtyNeedsFullRepaint
            && is_verified_recovery_repaint(&frame, surface_viewport);
        if self.state == TerminalState::DirtyNeedsFullRepaint && !recovery_repaint {
            return Err(PublishError::RecoveryRepaintRequired);
        }
        // Encoding happens before the publish guard opens, so a frame that
        // cannot be placed inside the rectangle never touches the terminal and
        // never leaves the input router mid-transaction.
        let encoded =
            encode_surface_frame(&frame.draw_ops, rect).map_err(PublishError::OutsideSurface)?;
        let receipt = self.publish_encoded(
            output,
            EncodedFrame {
                key: frame.frame_key(),
                geometry,
                bytes: &encoded,
            },
            input_router,
            input_sequence,
        )?;
        if recovery_repaint {
            self.state = TerminalState::Clean;
        }
        Ok(receipt)
    }

    /// Publishes bytes produced by a whitelisted legacy Host renderer.
    ///
    /// This migration adapter does not accept worker or IPC frame types. Trusted
    /// bytes bypass DrawOp validation, but never bypass the publish guard, flush,
    /// capture, or input-sequence barrier.
    ///
    /// # Errors
    /// Returns [`PublishError::RecoveryRepaintRequired`] without terminal I/O
    /// while dirty because opaque bytes cannot prove a typed recovery repaint,
    /// or [`PublishError::Output`] after aborting the publish guard when the
    /// terminal write or flush fails.
    pub fn publish_trusted_host_frame<W: Write>(
        &mut self,
        output: &mut W,
        frame: TrustedHostFrame,
        input_router: &mut InputRouter,
        input_sequence: &InputSequence,
    ) -> Result<PublishReceipt, PublishError> {
        if self.state == TerminalState::DirtyNeedsFullRepaint {
            return Err(PublishError::RecoveryRepaintRequired);
        }
        let key = FrameKey {
            worker_generation: frame.worker_generation,
            frame_id: frame.frame_id,
            capture_id: frame.capture_id,
        };
        let _host_owner = frame.owner;
        self.publish_encoded(
            output,
            EncodedFrame {
                key,
                geometry: frame.geometry,
                bytes: &frame.bytes,
            },
            input_router,
            input_sequence,
        )
    }

    fn publish_encoded<W: Write>(
        &mut self,
        output: &mut W,
        frame: EncodedFrame<'_>,
        input_router: &mut InputRouter,
        input_sequence: &InputSequence,
    ) -> Result<PublishReceipt, PublishError> {
        let guard = input_router.begin_publish(frame.key, input_sequence.current());
        if let Err(error) = output.write_all(frame.bytes) {
            self.state = TerminalState::DirtyNeedsFullRepaint;
            input_router.abort_publish(guard);
            return Err(PublishError::Output(error));
        }
        if let Err(error) = output.flush() {
            self.state = TerminalState::DirtyNeedsFullRepaint;
            input_router.abort_publish(guard);
            return Err(PublishError::Output(error));
        }
        let publish_seq = input_sequence.current();
        let token = input_router.commit_publish(guard, publish_seq, frame.geometry);
        Ok(PublishReceipt {
            token,
            frame_id: frame.key.frame_id,
            publish_seq,
            geometry: frame.geometry,
        })
    }
}

fn is_verified_recovery_repaint(frame: &PreparedFrame, viewport: Viewport) -> bool {
    if !frame.full_repaint {
        return false;
    }
    matches!(
        frame.draw_ops.as_slice(),
        [
            DrawOp::SetStyle(RenderStyle::Reset),
            DrawOp::ClearRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            ..
        ] if *width == viewport.columns && *height == viewport.rows
    )
}

fn encode_draw_ops(draw_ops: &[DrawOp]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for op in draw_ops {
        match op {
            DrawOp::ClearRect {
                x,
                y,
                width,
                height,
            } => encode_clear_rect(&mut encoded, *x, *y, *width, *height),
            DrawOp::MoveTo { x, y } => encode_move_to(&mut encoded, *x, *y),
            DrawOp::SetStyle(style) => encode_style(&mut encoded, *style),
            DrawOp::Text(text) => encoded.extend_from_slice(text.as_bytes()),
            DrawOp::CursorVisible(true) => encoded.extend_from_slice(b"\x1b[?25h"),
            DrawOp::CursorVisible(false) => encoded.extend_from_slice(b"\x1b[?25l"),
        }
    }
    encoded
}

fn encode_clear_rect(encoded: &mut Vec<u8>, x: u16, y: u16, width: u16, height: u16) {
    encoded.extend_from_slice(b"\x1b[s");
    for row_offset in 0..height {
        encode_move_to(encoded, x, y.saturating_add(row_offset));
        encoded.extend_from_slice(format!("\x1b[{width}X").as_bytes());
    }
    encoded.extend_from_slice(b"\x1b[u");
}

fn encode_move_to(encoded: &mut Vec<u8>, x: u16, y: u16) {
    encoded.extend_from_slice(
        format!("\x1b[{};{}H", y.saturating_add(1), x.saturating_add(1)).as_bytes(),
    );
}

fn encode_style(encoded: &mut Vec<u8>, style: RenderStyle) {
    let code = match style {
        RenderStyle::Reset => 0,
        RenderStyle::Bold => 1,
        RenderStyle::Dim => 2,
        RenderStyle::Foreground(color) => foreground_code(color),
        RenderStyle::Background(color) => background_code(color),
    };
    encoded.extend_from_slice(format!("\x1b[{code}m").as_bytes());
}

const fn foreground_code(color: RenderColor) -> u8 {
    match color {
        RenderColor::Default => 39,
        RenderColor::Black => 30,
        RenderColor::Red => 31,
        RenderColor::Green => 32,
        RenderColor::Yellow => 33,
        RenderColor::Blue => 34,
        RenderColor::Magenta => 35,
        RenderColor::Cyan => 36,
        RenderColor::White => 37,
    }
}

const fn background_code(color: RenderColor) -> u8 {
    match color {
        RenderColor::Default => 49,
        RenderColor::Black => 40,
        RenderColor::Red => 41,
        RenderColor::Green => 42,
        RenderColor::Yellow => 43,
        RenderColor::Blue => 44,
        RenderColor::Magenta => 45,
        RenderColor::Cyan => 46,
        RenderColor::White => 47,
    }
}
