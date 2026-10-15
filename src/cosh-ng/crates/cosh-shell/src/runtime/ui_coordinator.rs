//! Host-side lifecycle and frame coordination for the isolated UI worker.

use std::time::Instant;

use crate::ui::worker_protocol::{
    decode_worker_message, encode_host_message, validate_frame_echo, validate_header_identity,
    validate_prepared_frame, FrameGeometry, FrameKey, Hello, HostMessage, PreparedFrame,
    ProtocolError, PublishedAck, RenderRequest, SessionNonce, Shutdown, WorkerGeneration,
    WorkerMessage, SCHEMA_VERSION,
};

use process::{ProcessError, WorkerProcess};
use state::{deadline, render_revision, ActiveRender, OutboundKind, PendingWrite};

#[path = "ui_coordinator/outbound.rs"]
mod outbound;
#[path = "ui_coordinator/process.rs"]
mod process;
#[path = "ui_coordinator/state.rs"]
mod state;

pub(crate) use state::{
    CandidateIdentity, CoordinatorPolicy, InflightFrame, PublishedFrame, RecoveryMode,
    WorkerLifecycle,
};

const TYPED_DRAW_OPS_CAPABILITY: &str = "typed-draw-ops-v1";

#[derive(Debug)]
pub(crate) enum CoordinatorError {
    CandidatePathNotAbsolute,
    InvalidPolicy,
    InvalidState,
    CounterExhausted,
    DeadlineOverflow,
    Process(ProcessError),
    Protocol(ProtocolError),
}

impl std::fmt::Display for CoordinatorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CandidatePathNotAbsolute => {
                write!(formatter, "UI worker candidate path must be absolute")
            }
            Self::InvalidPolicy => write!(formatter, "invalid injected UI coordinator policy"),
            Self::InvalidState => write!(formatter, "invalid UI coordinator state transition"),
            Self::CounterExhausted => write!(formatter, "UI coordinator counter exhausted"),
            Self::DeadlineOverflow => write!(formatter, "UI coordinator deadline overflowed"),
            Self::Process(error) => write!(formatter, "{error}"),
            Self::Protocol(error) => write!(formatter, "UI worker protocol failed: {error}"),
        }
    }
}

impl std::error::Error for CoordinatorError {}

impl From<ProcessError> for CoordinatorError {
    fn from(value: ProcessError) -> Self {
        Self::Process(value)
    }
}

impl From<ProtocolError> for CoordinatorError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

pub(crate) struct UiWorkerCoordinator {
    candidate: CandidateIdentity,
    session_nonce: SessionNonce,
    policy: CoordinatorPolicy,
    lifecycle: WorkerLifecycle,
    recovery_mode: RecoveryMode,
    generation: WorkerGeneration,
    failure_count: u64,
    process: Option<WorkerProcess>,
    outbound: Option<PendingWrite>,
    handshake_deadline: Option<Instant>,
    prepare_deadline: Option<Instant>,
    heartbeat_due: Option<(FrameKey, Instant)>,
    prior_heartbeat_key: Option<FrameKey>,
    last_heartbeat_sequence: Option<u64>,
    inflight: Option<InflightFrame>,
    prepared_delivered: bool,
    active_render: Option<ActiveRender>,
    published: Option<PublishedFrame>,
    pending_latest_request: Option<RenderRequest>,
    next_frame_id: u64,
    shutdown_reason: Option<String>,
    shutdown_deadline: Option<Instant>,
    shutdown_sent: bool,
    closed: bool,
}

impl UiWorkerCoordinator {
    #[cfg(test)]
    pub(crate) fn prepare_for_surface_publish_test(&mut self, key: FrameKey) {
        self.inflight = Some(InflightFrame::Preparing(key));
        self.prepared_delivered = true;
    }

    pub(crate) fn new(
        candidate: CandidateIdentity,
        session_nonce: SessionNonce,
        initial_generation: WorkerGeneration,
        policy: CoordinatorPolicy,
    ) -> Result<Self, CoordinatorError> {
        if !candidate.binary_path.is_absolute()
            || policy.failure_limit == 0
            || policy.pending_revision_capacity != 1
            || policy.heartbeat_interval.is_zero()
            || policy.heartbeat_interval >= policy.heartbeat_timeout
            || u64::try_from(policy.heartbeat_interval.as_nanos()).is_err()
        {
            return Err(if !candidate.binary_path.is_absolute() {
                CoordinatorError::CandidatePathNotAbsolute
            } else {
                CoordinatorError::InvalidPolicy
            });
        }
        Ok(Self {
            candidate,
            session_nonce,
            policy,
            lifecycle: WorkerLifecycle::Starting(initial_generation),
            recovery_mode: RecoveryMode::Normal,
            generation: initial_generation,
            failure_count: 0,
            process: None,
            outbound: None,
            handshake_deadline: None,
            prepare_deadline: None,
            heartbeat_due: None,
            prior_heartbeat_key: None,
            last_heartbeat_sequence: None,
            inflight: None,
            prepared_delivered: false,
            active_render: None,
            published: None,
            pending_latest_request: None,
            next_frame_id: 1,
            shutdown_reason: None,
            shutdown_deadline: None,
            shutdown_sent: false,
            closed: false,
        })
    }

    pub(crate) fn start(&mut self, now: Instant) -> Result<(), CoordinatorError> {
        if self.process.is_some()
            || self.closed
            || !matches!(self.lifecycle, WorkerLifecycle::Starting(_))
        {
            return Err(CoordinatorError::InvalidState);
        }
        self.spawn_generation(now)
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Result<Option<PreparedFrame>, CoordinatorError> {
        if self.closed {
            return Ok(None);
        }
        if self.shutdown_reason.is_some() {
            self.tick_shutdown(now)?;
            return Ok(None);
        }
        if matches!(self.lifecycle, WorkerLifecycle::Rebuilding(_)) {
            self.spawn_generation(now)?;
        }

        let exited = match self.process.as_mut() {
            Some(process) => process.try_status()?,
            None => None,
        };
        if exited.is_some() {
            self.record_failure()?;
            return Ok(None);
        }

        if let Err(error) = self.flush_outbound(now) {
            self.record_failure()?;
            return match error {
                CoordinatorError::Process(_) | CoordinatorError::Protocol(_) => Ok(None),
                other => Err(other),
            };
        }

        let received = match self.process.as_mut() {
            Some(process) => process.read_frame(self.policy.protocol_limits),
            None => return Ok(None),
        };
        let frame = match received {
            Ok(frame) => frame,
            Err(_) => {
                self.record_failure()?;
                return Ok(None);
            }
        };
        if let Some(frame) = frame {
            return self.handle_worker_frame(&frame, now);
        }

        if self.deadline_expired(now) {
            self.record_failure()?;
        }
        Ok(None)
    }

    pub(crate) fn request_render(
        &mut self,
        request: RenderRequest,
        now: Instant,
    ) -> Result<(), CoordinatorError> {
        if self.closed
            || self.shutdown_reason.is_some()
            || self.recovery_mode != RecoveryMode::Normal
            || matches!(self.lifecycle, WorkerLifecycle::Unavailable)
        {
            return Err(CoordinatorError::InvalidState);
        }
        let busy = !matches!(self.lifecycle, WorkerLifecycle::Ready(g) if g == self.generation)
            || self.inflight.is_some()
            || self.outbound.is_some();
        if busy {
            self.queue_latest(request);
            return Ok(());
        }
        self.dispatch_render(request, now)
    }

    pub(crate) fn begin_publishing(
        &mut self,
        key: FrameKey,
        input_barrier: u64,
    ) -> Result<(), CoordinatorError> {
        if self.inflight != Some(InflightFrame::Preparing(key)) || !self.prepared_delivered {
            return Err(CoordinatorError::InvalidState);
        }
        self.published = None;
        self.prepared_delivered = false;
        self.inflight = Some(InflightFrame::Publishing { key, input_barrier });
        Ok(())
    }

    pub(crate) fn publish_succeeded(
        &mut self,
        key: FrameKey,
        publish_seq: u64,
        geometry: FrameGeometry,
        now: Instant,
    ) -> Result<(), CoordinatorError> {
        let input_barrier = match self.inflight {
            Some(InflightFrame::Publishing {
                key: current,
                input_barrier,
            }) if current == key => input_barrier,
            _ => return Err(CoordinatorError::InvalidState),
        };
        if self.outbound.is_some() || publish_seq < input_barrier {
            return Err(CoordinatorError::InvalidState);
        }
        let message = HostMessage::PublishedAck(PublishedAck {
            frame_id: key.frame_id,
        });
        self.queue_message(key.frame_id, &message, OutboundKind::PublishedAck(key), now)?;
        self.published = Some(PublishedFrame {
            key,
            publish_seq,
            geometry,
        });
        self.inflight = None;
        self.active_render = None;
        Ok(())
    }

    pub(crate) fn publish_failed(&mut self, key: FrameKey) -> Result<(), CoordinatorError> {
        if !matches!(self.inflight, Some(InflightFrame::Publishing { key: current, .. }) if current == key)
        {
            return Err(CoordinatorError::InvalidState);
        }
        self.record_failure()
    }

    pub(crate) fn begin_shutdown(
        &mut self,
        reason: String,
        now: Instant,
    ) -> Result<(), CoordinatorError> {
        if self.closed || self.shutdown_reason.is_some() {
            return Err(CoordinatorError::InvalidState);
        }
        self.pending_latest_request = None;
        self.active_render = None;
        self.inflight = None;
        self.prepared_delivered = false;
        self.published = None;
        self.handshake_deadline = None;
        self.prepare_deadline = None;
        self.heartbeat_due = None;
        self.prior_heartbeat_key = None;
        self.shutdown_sent = false;
        self.shutdown_reason = Some(reason);
        self.shutdown_deadline = Some(deadline(now, self.policy.shutdown_timeout)?);
        self.ensure_shutdown_queued(now)
    }

    pub(crate) const fn lifecycle(&self) -> WorkerLifecycle {
        self.lifecycle
    }

    pub(crate) const fn worker_generation(&self) -> WorkerGeneration {
        self.generation
    }

    pub(crate) const fn failure_count(&self) -> u64 {
        self.failure_count
    }

    /// Whether the shutdown transaction finished and the child was reaped.
    pub(crate) const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether a child process is currently held.
    pub(crate) const fn has_worker(&self) -> bool {
        self.process.is_some()
    }

    pub(crate) const fn recovery_mode(&self) -> RecoveryMode {
        self.recovery_mode
    }

    pub(crate) const fn inflight(&self) -> Option<InflightFrame> {
        self.inflight
    }

    pub(crate) const fn published(&self) -> Option<PublishedFrame> {
        self.published
    }

    pub(crate) fn pending_latest_revision(&self) -> Option<u64> {
        self.pending_latest_request.as_ref().map(render_revision)
    }

    pub(crate) const fn last_heartbeat_sequence(&self) -> Option<u64> {
        self.last_heartbeat_sequence
    }

    fn spawn_generation(&mut self, now: Instant) -> Result<(), CoordinatorError> {
        let hello = HostMessage::Hello(Hello {
            protocol_version: SCHEMA_VERSION,
            candidate_version: self.candidate.candidate_version.clone(),
            capabilities: vec![TYPED_DRAW_OPS_CAPABILITY.to_owned()],
        });
        let bytes = encode_host_message(
            self.session_nonce,
            self.generation,
            0,
            &hello,
            &self.policy.protocol_limits,
        )?;
        let pending = PendingWrite {
            bytes,
            offset: 0,
            deadline: deadline(now, self.policy.backpressure_timeout)?,
            kind: OutboundKind::Hello,
        };
        let process = match WorkerProcess::spawn(
            &self.candidate.binary_path,
            self.policy.protocol_limits,
            self.policy.heartbeat_interval,
        ) {
            Ok(process) => process,
            Err(_) => {
                self.record_failure()?;
                return Ok(());
            }
        };
        self.process = Some(process);
        self.lifecycle = WorkerLifecycle::Starting(self.generation);
        self.last_heartbeat_sequence = None;
        self.outbound = Some(pending);
        Ok(())
    }

    fn dispatch_render(
        &mut self,
        request: RenderRequest,
        now: Instant,
    ) -> Result<(), CoordinatorError> {
        let frame_id = self.next_frame_id;
        self.next_frame_id = self
            .next_frame_id
            .checked_add(1)
            .ok_or(CoordinatorError::CounterExhausted)?;
        let key = FrameKey {
            worker_generation: self.generation,
            frame_id,
            capture_id: request.capture_id,
        };
        let message = HostMessage::Render(request.clone());
        self.queue_message(frame_id, &message, OutboundKind::Render, now)?;
        self.active_render = Some(ActiveRender { key, request });
        self.inflight = Some(InflightFrame::Preparing(key));
        self.prepared_delivered = false;
        Ok(())
    }

    fn handle_worker_frame(
        &mut self,
        frame: &[u8],
        now: Instant,
    ) -> Result<Option<PreparedFrame>, CoordinatorError> {
        let decoded = decode_worker_message(frame, &self.policy.protocol_limits);
        let (header, message) = match decoded {
            Ok(decoded) => decoded,
            Err(_) => {
                self.record_failure()?;
                return Ok(None);
            }
        };
        match message {
            WorkerMessage::Ready(ready) => {
                let expected = FrameKey {
                    worker_generation: self.generation,
                    frame_id: 0,
                    capture_id: None,
                };
                let valid = matches!(self.lifecycle, WorkerLifecycle::Starting(g) if g == self.generation)
                    && validate_header_identity(&header, self.session_nonce, expected).is_ok()
                    && ready.protocol_version == SCHEMA_VERSION
                    && ready.candidate_version == self.candidate.candidate_version
                    && ready
                        .capabilities
                        .iter()
                        .any(|capability| capability == TYPED_DRAW_OPS_CAPABILITY);
                if !valid {
                    self.record_failure()?;
                    return Ok(None);
                }
                self.handshake_deadline = None;
                self.lifecycle = WorkerLifecycle::Ready(self.generation);
                self.prior_heartbeat_key = None;
                self.heartbeat_due =
                    Some((expected, deadline(now, self.policy.heartbeat_timeout)?));
                if let Some(request) = self.pending_latest_request.take() {
                    self.dispatch_render(request, now)?;
                }
                Ok(None)
            }
            WorkerMessage::PreparedFrame(prepared) => {
                let Some(active) = self.active_render.as_ref() else {
                    self.record_failure()?;
                    return Ok(None);
                };
                let valid = !self.prepared_delivered
                    && self.inflight == Some(InflightFrame::Preparing(active.key))
                    && validate_header_identity(&header, self.session_nonce, active.key).is_ok()
                    && validate_frame_echo(&header, &prepared, active.key).is_ok()
                    && validate_prepared_frame(
                        &prepared,
                        active.request.viewport,
                        &self.policy.protocol_limits,
                    )
                    .is_ok();
                if !valid {
                    self.record_failure()?;
                    return Ok(None);
                }
                self.prepare_deadline = None;
                self.prepared_delivered = true;
                Ok(Some(prepared))
            }
            WorkerMessage::Heartbeat(heartbeat) => {
                let Some((key, heartbeat_deadline)) = self.heartbeat_due else {
                    self.record_failure()?;
                    return Ok(None);
                };
                let monotonic = self
                    .last_heartbeat_sequence
                    .is_none_or(|previous| heartbeat.monotonic_seq > previous);
                let current_identity =
                    validate_header_identity(&header, self.session_nonce, key).is_ok();
                let prior_identity = self.prior_heartbeat_key.is_some_and(|previous| {
                    validate_header_identity(&header, self.session_nonce, previous).is_ok()
                });
                if now >= heartbeat_deadline || (!current_identity && !prior_identity) || !monotonic
                {
                    self.record_failure()?;
                    return Ok(None);
                }
                if current_identity {
                    self.prior_heartbeat_key = None;
                }
                self.last_heartbeat_sequence = Some(heartbeat.monotonic_seq);
                self.heartbeat_due = Some((key, deadline(now, self.policy.heartbeat_timeout)?));
                Ok(None)
            }
            WorkerMessage::WorkerError(_) => {
                self.record_failure()?;
                Ok(None)
            }
        }
    }

    fn queue_latest(&mut self, request: RenderRequest) {
        let latest = self
            .pending_latest_request
            .as_ref()
            .or_else(|| self.active_render.as_ref().map(|active| &active.request));
        if latest.is_some_and(|latest| {
            render_revision(latest) > render_revision(&request) || latest == &request
        }) {
            return;
        }
        self.pending_latest_request = Some(request);
    }

    fn restore_active_after_failure(&mut self, request: RenderRequest) {
        let restore = self
            .pending_latest_request
            .as_ref()
            .is_none_or(|pending| render_revision(&request) > render_revision(pending));
        if restore {
            self.pending_latest_request = Some(request);
        }
    }

    fn deadline_expired(&self, now: Instant) -> bool {
        self.handshake_deadline
            .is_some_and(|deadline| now >= deadline)
            || self
                .prepare_deadline
                .is_some_and(|deadline| now >= deadline)
            || self
                .heartbeat_due
                .is_some_and(|(_, deadline)| now >= deadline)
    }

    fn record_failure(&mut self) -> Result<(), CoordinatorError> {
        let active_request = self.active_render.take().map(|active| active.request);
        self.inflight = None;
        self.prepared_delivered = false;
        self.published = None;
        self.outbound = None;
        self.handshake_deadline = None;
        self.prepare_deadline = None;
        self.heartbeat_due = None;
        self.prior_heartbeat_key = None;
        if let Some(mut process) = self.process.take() {
            if let Err(error) = process.terminate_and_reap() {
                self.pending_latest_request = None;
                self.lifecycle = WorkerLifecycle::Unavailable;
                self.recovery_mode = RecoveryMode::InteractionEnding;
                return Err(CoordinatorError::Process(error));
            }
        }
        self.failure_count = self
            .failure_count
            .checked_add(1)
            .ok_or(CoordinatorError::CounterExhausted)?;
        if self.failure_count >= self.policy.failure_limit {
            self.pending_latest_request = None;
            self.lifecycle = WorkerLifecycle::Unavailable;
            self.recovery_mode = RecoveryMode::InteractionEnding;
            return Ok(());
        }
        if let Some(request) = active_request {
            self.restore_active_after_failure(request);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(CoordinatorError::CounterExhausted)?;
        self.lifecycle = WorkerLifecycle::Rebuilding(self.generation);
        Ok(())
    }

    fn tick_shutdown(&mut self, now: Instant) -> Result<(), CoordinatorError> {
        let exited = match self.process.as_mut() {
            Some(process) => process.try_status()?,
            None => None,
        };
        if exited.is_some() {
            self.finish_shutdown();
            return Ok(());
        }
        if self.flush_outbound(now).is_err() {
            self.force_shutdown()?;
            return Ok(());
        }
        self.ensure_shutdown_queued(now)?;
        if self
            .shutdown_deadline
            .is_some_and(|shutdown_deadline| now >= shutdown_deadline)
        {
            self.force_shutdown()?;
        }
        Ok(())
    }

    fn ensure_shutdown_queued(&mut self, now: Instant) -> Result<(), CoordinatorError> {
        if self.process.is_none() || self.outbound.is_some() || self.shutdown_sent {
            return Ok(());
        }
        let reason = self
            .shutdown_reason
            .as_ref()
            .ok_or(CoordinatorError::InvalidState)?
            .clone();
        self.queue_message(
            self.next_frame_id.saturating_sub(1),
            &HostMessage::Shutdown(Shutdown { reason }),
            OutboundKind::Shutdown,
            now,
        )
    }

    fn force_shutdown(&mut self) -> Result<(), CoordinatorError> {
        if let Some(mut process) = self.process.take() {
            process.terminate_and_reap()?;
        }
        self.finish_shutdown();
        Ok(())
    }

    fn finish_shutdown(&mut self) {
        self.process = None;
        self.outbound = None;
        self.shutdown_reason = None;
        self.shutdown_deadline = None;
        self.shutdown_sent = false;
        self.lifecycle = WorkerLifecycle::Unavailable;
        self.closed = true;
    }
}

#[cfg(test)]
mod tests;
