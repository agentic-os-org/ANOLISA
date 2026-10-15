//! Orthogonal Host-owned UI worker lifecycle and frame state.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::ui::worker_protocol::{
    FrameGeometry, FrameKey, ProtocolLimits, RenderRequest, RenderView, WorkerGeneration,
};

use super::CoordinatorError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CandidateIdentity {
    pub(crate) binary_path: PathBuf,
    pub(crate) candidate_version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CoordinatorPolicy {
    pub(crate) protocol_limits: ProtocolLimits,
    pub(crate) handshake_timeout: Duration,
    pub(crate) heartbeat_interval: Duration,
    pub(crate) heartbeat_timeout: Duration,
    pub(crate) backpressure_timeout: Duration,
    pub(crate) shutdown_timeout: Duration,
    pub(crate) failure_limit: u64,
    pub(crate) pending_revision_capacity: usize,
}

/// Process lifecycle, kept independent from frame and recovery state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerLifecycle {
    /// A generation was spawned and has not completed its Ready handshake.
    Starting(WorkerGeneration),
    /// A generation completed its Ready handshake.
    Ready(WorkerGeneration),
    /// The prior child was reaped and the named generation may now start.
    Rebuilding(WorkerGeneration),
    /// No worker generation is available.
    Unavailable,
}

/// Last frame whose terminal publication completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedFrame {
    /// Host-assigned identity echoed by the worker.
    pub key: FrameKey,
    /// Input sequence sampled after the terminal flush.
    pub publish_seq: u64,
    /// Host-validated geometry committed for the frame.
    pub geometry: FrameGeometry,
}

/// Sole frame currently being prepared or published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InflightFrame {
    /// The Host is awaiting or validating worker output.
    Preparing(FrameKey),
    /// Terminal publication began after the recorded input barrier.
    Publishing {
        /// Host-assigned frame identity.
        key: FrameKey,
        /// Input sequence sampled before terminal output begins.
        input_barrier: u64,
    },
}

/// Recovery policy state, independent from process lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryMode {
    /// Automatic rebuilding remains allowed.
    Normal,
    /// The injected failure count ceiling was reached.
    InteractionEnding,
    /// Safe automatic input return cannot be proven.
    ManualReconnect,
}

#[derive(Clone, Debug)]
pub(super) struct ActiveRender {
    pub(super) key: FrameKey,
    pub(super) request: RenderRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutboundKind {
    Hello,
    Render,
    PublishedAck(FrameKey),
    Shutdown,
}

#[derive(Debug)]
pub(super) struct PendingWrite {
    pub(super) bytes: Vec<u8>,
    pub(super) offset: usize,
    pub(super) deadline: Instant,
    pub(super) kind: OutboundKind,
}

pub(super) fn render_revision(request: &RenderRequest) -> u64 {
    match &request.render_view {
        RenderView::Empty(view) => view.revision,
        RenderView::ApprovalCard(view) => view.revision,
    }
}

pub(super) fn deadline(now: Instant, duration: Duration) -> Result<Instant, CoordinatorError> {
    now.checked_add(duration)
        .ok_or(CoordinatorError::DeadlineOverflow)
}
