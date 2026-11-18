//! One user turn on a live ACP connection.
//!
//! The connection itself is owned by the adapter; this module only drives the
//! handshake, the prompt and the terminal fact, and reports whether the
//! connection may serve another turn.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use agent_client_protocol::v1::schema::{ContentBlock, StopReason, TextContent};

use crate::adapter::AdapterError;
use crate::types::AgentEvent;

use super::codec::{AcpClientCodec, Inbound, Phase};
use super::host_shell::{AcpHostExecuteRequest, AcpHostRefusal};
use super::permission::{permission_outcome, AcpHostAuthority, AcpPermissionPolicy};
use super::transport::{AgentTransport, LineEvent};
use super::{host_shell, projection, turn, CANCEL_GRACE, POLL_SLICE};

/// A40 bounded exact-duplicate suppression for tool_call_update frames.
///
/// Stores whole-frame fingerprints only within frozen count and byte bounds.
/// A frame already stored is always suppressed; beyond the bounds new frames
/// pass through without being stored, so residency cannot grow with the turn's
/// total output. This is deliberately not a general dedup framework.
pub(super) struct ToolUpdateDedup {
    seen: HashSet<String>,
    bytes: usize,
}

const MAX_TOOL_UPDATE_DEDUP_ENTRIES: usize = 4096;
const MAX_TOOL_UPDATE_DEDUP_BYTES: usize = 1024 * 1024;

impl ToolUpdateDedup {
    pub(super) fn new() -> Self {
        Self {
            seen: HashSet::new(),
            bytes: 0,
        }
    }

    /// Returns true when the frame should be projected. An exact duplicate of a
    /// stored frame returns false; a new frame is stored only within bounds.
    fn admit(&mut self, fingerprint: String) -> bool {
        if self.seen.contains(&fingerprint) {
            return false;
        }
        if self.seen.len() < MAX_TOOL_UPDATE_DEDUP_ENTRIES
            && self.bytes + fingerprint.len() <= MAX_TOOL_UPDATE_DEDUP_BYTES
        {
            self.bytes += fingerprint.len();
            self.seen.insert(fingerprint);
        }
        true
    }
}

/// One user turn on a live ACP connection.
pub(super) struct AcpSession<'a> {
    pub(super) transport: &'a mut AgentTransport,
    pub(super) codec: &'a mut AcpClientCodec,
    pub(super) owned: &'a mut HashSet<String>,
    pub(super) run_id: String,
    pub(super) idle_timeout: Duration,
    pub(super) policy: &'a AcpPermissionPolicy,
    pub(super) host_authority: Option<&'a AcpHostAuthority>,
    pub(super) cancelled: &'a AtomicBool,
    pub(super) committed_id: &'a Mutex<Option<String>>,
    pub(super) stop_reason: Option<StopReason>,
    pub(super) failed: bool,
    /// A40: exact-duplicate tool_call_update frames within this turn are
    /// suppressed so a misbehaving peer cannot render a second completion.
    /// Legitimate status progressions carry different fingerprints and pass.
    pub(super) tool_update_dedup: ToolUpdateDedup,
}

impl AcpSession<'_> {
    pub(super) fn run(
        &mut self,
        cwd: &Path,
        prompt: &str,
        resume: Option<&str>,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        if self.codec.phase() == Phase::Created {
            self.handshake(cwd, resume, sink)?;
        }
        self.prompt_turn(prompt, sink)
    }

    /// Whether this connection may serve another user turn.
    ///
    /// Only a turn that reached a normal stop reason on a live codec leaves the
    /// stream drained. A cancelled or failed turn may still owe frames, so the
    /// next request pays a cold start instead of reading them as its own.
    pub(super) fn reusable(&self) -> bool {
        !self.failed
            && !self.cancelled.load(Ordering::SeqCst)
            && self.codec.phase() == Phase::Ready
            && matches!(
                self.stop_reason,
                Some(StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests)
            )
    }

    /// A restricted continuation may only run on a peer that promised the
    /// restriction. The Shell's approval policy cannot reach execution inside
    /// the peer's own process, so the prompt must never be written.
    fn ensure_read_only_peer(&self) -> Result<(), AdapterError> {
        if self.codec.read_only_turn() && !self.codec.agent_supports_read_only_turn() {
            return Err(AdapterError {
                message: format!(
                    "acp peer did not advertise {} v{}; the analysis-only continuation was refused before any prompt",
                    turn::CAPABILITY_KEY,
                    turn::VERSION
                ),
            });
        }
        Ok(())
    }

    /// Brings a fresh connection to one ready session.
    fn handshake(
        &mut self,
        cwd: &Path,
        resume: Option<&str>,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        let frame = self.codec.initialize()?;
        self.transport.write_frame(&frame)?;
        self.await_fact(sink, |inbound| matches!(inbound, Inbound::Initialized))?;
        self.ensure_read_only_peer()?;
        let frame = match resume {
            Some(id) => {
                let supports_load = self
                    .codec
                    .agent_capabilities()
                    .map(|capabilities| capabilities.load_session)
                    .unwrap_or(false);
                if !supports_load {
                    return Err(AdapterError {
                        message: format!(
                            "acp peer does not advertise session/load; resume of {id} was refused before any prompt"
                        ),
                    });
                }
                self.codec.load_session(cwd, id)?
            }
            None => self.codec.new_session(cwd)?,
        };
        self.transport.write_frame(&frame)?;
        self.await_fact(sink, |inbound| {
            matches!(inbound, Inbound::SessionCreated { .. })
        })?;
        // Publish the identity behind its own short lock as soon as the session
        // exists, so a concurrent read-only query does not wait on the peer lock
        // held for the whole turn.
        *self
            .committed_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            self.codec.session_id().map(str::to_string);
        Ok(())
    }

    /// Sends one user prompt and drives it to a terminal fact.
    fn prompt_turn(
        &mut self,
        prompt: &str,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        self.ensure_read_only_peer()?;
        // A cancel that landed before the prompt must not start a user task on
        // the peer just to obtain a cancellation confirmation.
        if self.cancelled.load(Ordering::SeqCst) {
            return self.drive(sink, false);
        }
        let blocks = vec![ContentBlock::Text(TextContent::new(prompt.to_string()))];
        let frame = self.codec.prompt(blocks)?;
        if let Err(failure) = self.transport.write_frame(&frame) {
            // An aborted write delivered no complete frame, so the peer never
            // started a turn: the cancellation is the terminal fact, not an
            // error. A write that failed for any other reason still fails.
            if self.cancelled.load(Ordering::SeqCst) {
                sink(AgentEvent::AgentCancelled {
                    run_id: self.run_id.clone(),
                    reason: "session/prompt write cancelled before the frame was delivered"
                        .to_string(),
                })?;
                return Ok(());
            }
            return Err(failure);
        }
        self.drive(sink, true)
    }

    /// Waits for one matching fact, forwarding every other fact to the sink.
    fn await_fact(
        &mut self,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
        matches: impl Fn(&Inbound) -> bool,
    ) -> Result<(), AdapterError> {
        let deadline = Instant::now() + self.idle_timeout;
        let mut cancel_deadline: Option<Instant> = None;
        loop {
            // A pending cancel must still reach session/cancel, so the
            // handshake keeps going; a peer that stays silent gets the same
            // grace as a cancelled turn instead of the full idle deadline.
            if self.cancelled.load(Ordering::SeqCst) && cancel_deadline.is_none() {
                cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
            }
            if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(AdapterError {
                    message: format!(
                        "acp handshake did not complete within {}s of a requested cancel; stderr: {}",
                        CANCEL_GRACE.as_secs(),
                        self.transport.stderr_tail()
                    ),
                });
            }
            let Some(event) = self.transport.next_line(POLL_SLICE)? else {
                if Instant::now() >= deadline {
                    return Err(self.timeout("handshake"));
                }
                continue;
            };
            for inbound in self.decode(event)? {
                if matches(&inbound) {
                    return Ok(());
                }
                // A closed stdout or an error reply is terminal for the
                // handshake: forwarding it would leave the loop spinning on a
                // disconnected channel or waiting out the idle deadline.
                match inbound {
                    Inbound::TransportClosed => {
                        return Err(AdapterError {
                            message: format!(
                                "acp agent closed stdout during the handshake; stderr: {}",
                                self.transport.stderr_tail()
                            ),
                        });
                    }
                    Inbound::Failed {
                        kind,
                        code,
                        message,
                    } => {
                        return Err(AdapterError {
                            message: format!(
                                "acp {kind:?} failed during the handshake ({code}): {message}"
                            ),
                        });
                    }
                    other => self.forward(sink, &other)?,
                }
            }
        }
    }

    fn drive(
        &mut self,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
        prompt_sent: bool,
    ) -> Result<(), AdapterError> {
        let cancelled = self.cancelled;
        // A cancelled run still owes the peer session/cancel and a bounded wait
        // for its confirmation. A sink that stops accepting events must not cut
        // that notification short, so its error is held back while cancelled.
        let mut guarded = |event: AgentEvent| match sink(event) {
            Err(_) if cancelled.load(Ordering::SeqCst) => Ok(()),
            result => result,
        };
        let mut idle_deadline = Instant::now() + self.idle_timeout;
        let mut cancel_deadline: Option<Instant> = None;
        loop {
            if self.cancelled.load(Ordering::SeqCst) && cancel_deadline.is_none() {
                let frame = self.codec.cancel()?;
                self.transport.write_frame(&frame)?;
                cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
            }
            if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                // Without a prompt there is no turn for the peer to confirm,
                // so the grace ending is the cancelled terminal fact itself.
                if !prompt_sent {
                    guarded(AgentEvent::AgentCancelled {
                        run_id: self.run_id.clone(),
                        reason: "session/cancel sent before any prompt".to_string(),
                    })?;
                    return Ok(());
                }
                return Err(AdapterError {
                    message: format!(
                        "acp agent did not confirm cancellation within {}s; stderr: {}",
                        CANCEL_GRACE.as_secs(),
                        self.transport.stderr_tail()
                    ),
                });
            }
            let Some(event) = self.transport.next_line(POLL_SLICE)? else {
                if Instant::now() >= idle_deadline {
                    return Err(self.timeout("prompt"));
                }
                continue;
            };
            idle_deadline = Instant::now() + self.idle_timeout;
            for inbound in self.decode(event)? {
                match inbound {
                    Inbound::Stopped { reason } => {
                        self.stop_reason = Some(reason);
                        guarded(projection::stop_event(&self.run_id, reason))?;
                        return Ok(());
                    }
                    Inbound::Failed {
                        kind,
                        code,
                        message,
                    } => {
                        self.failed = true;
                        guarded(AgentEvent::AgentFailed {
                            run_id: self.run_id.clone(),
                            error: format!("acp {kind:?} failed ({code}): {message}"),
                            error_code: Some(format!("acp_{code}")),
                            max_turns: None,
                        })?;
                        return Ok(());
                    }
                    Inbound::TransportClosed => {
                        self.failed = true;
                        // No prompt was sent, so a peer that exits after the
                        // cancel ended a cancelled run, not a failed turn.
                        if !prompt_sent {
                            guarded(AgentEvent::AgentCancelled {
                                run_id: self.run_id.clone(),
                                reason: "session/cancel sent before any prompt".to_string(),
                            })?;
                            return Ok(());
                        }
                        guarded(AgentEvent::AgentFailed {
                            run_id: self.run_id.clone(),
                            error: format!(
                                "acp agent closed stdout before a stop reason; stderr: {}",
                                self.transport.stderr_tail()
                            ),
                            error_code: Some("acp_transport_closed".to_string()),
                            max_turns: None,
                        })?;
                        return Ok(());
                    }
                    other => self.forward(&mut guarded, &other)?,
                }
            }
        }
    }

    fn decode(&mut self, event: LineEvent) -> Result<Vec<Inbound>, AdapterError> {
        match event {
            LineEvent::Line(line) => {
                let decoded = self.codec.decode_frame(&line)?;
                for reply in decoded.replies {
                    self.transport.write_frame(&reply)?;
                }
                Ok(decoded.events)
            }
            LineEvent::Closed => Ok(self.codec.finish_stdout().into_iter().collect()),
            LineEvent::Failed(message) => Err(AdapterError { message }),
        }
    }

    fn forward(
        &mut self,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
        inbound: &Inbound,
    ) -> Result<(), AdapterError> {
        match inbound {
            Inbound::Update(update) => {
                if let agent_client_protocol::v1::schema::SessionUpdate::ToolCallUpdate(ref u) =
                    update
                {
                    if let Ok(fingerprint) = serde_json::to_string(u) {
                        if !self.tool_update_dedup.admit(fingerprint) {
                            return Ok(());
                        }
                    }
                }
                for event in projection::project_update(&self.run_id, update.clone()) {
                    sink(event)?;
                }
                Ok(())
            }
            Inbound::Permission { id, request } => {
                let decision = (self.policy)(request);
                let outcome = permission_outcome(request, decision);
                let frame = self.codec.permission_reply(id.clone(), outcome)?;
                self.transport.write_frame(&frame)?;
                sink(AgentEvent::StatusChanged {
                    run_id: self.run_id.clone(),
                    phase: "permission".to_string(),
                    message: format!(
                        "answered session/request_permission for {}",
                        request.tool_call.tool_call_id.0
                    ),
                })
            }
            Inbound::UnsupportedRequest { id, method } => {
                let frame = self.codec.unsupported_reply(id.clone(), method)?;
                self.transport.write_frame(&frame)?;
                sink(AgentEvent::StatusChanged {
                    run_id: self.run_id.clone(),
                    phase: "unsupported_method".to_string(),
                    message: format!("rejected unsupported acp method {method}"),
                })
            }
            Inbound::Initialized | Inbound::SessionCreated { .. } => Ok(()),
            Inbound::Stopped { .. } | Inbound::Failed { .. } | Inbound::TransportClosed => Ok(()),
            Inbound::HostExecute { id, request } => self.run_host_execute(id, request, sink),
            Inbound::HostExecuteRejected { code, .. } => sink(AgentEvent::StatusChanged {
                run_id: self.run_id.clone(),
                phase: "host_execute_rejected".to_string(),
                message: format!("refused host-shell execute with code {code}"),
            }),
        }
    }

    /// Registers responsibility, reviews and dispatches one host-shell action
    /// exactly once.
    fn run_host_execute(
        &mut self,
        id: &agent_client_protocol::v1::schema::RequestId,
        request: &AcpHostExecuteRequest,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        if let Err(refusal) = host_shell::owned_admission(self.owned, &request.tool_call_id) {
            return self.refuse_host_execute(id, refusal, sink);
        }
        let Some(authority) = self.host_authority else {
            return self.refuse_host_execute(
                id,
                AcpHostRefusal::new(
                    host_shell::CODE_METHOD_NOT_FOUND,
                    "no foreground authority is wired on this connection",
                ),
                sink,
            );
        };
        // Responsibility transfers before the local decision, so a long human
        // review cannot trip the peer's undelivered-request timeout.
        let owned = self.codec.host_owned_frame(request)?;
        self.transport.write_frame(&owned)?;
        self.owned.insert(request.tool_call_id.clone());
        let outcome = match authority(request) {
            Ok(outcome) => outcome,
            Err(refusal) => return self.refuse_host_execute(id, refusal, sink),
        };
        let outcome = host_shell::bound_content(outcome);
        let frame = self.codec.host_execute_reply(id.clone(), &outcome)?;
        self.transport.write_frame(&frame)?;
        sink(AgentEvent::StatusChanged {
            run_id: self.run_id.clone(),
            phase: "host_execute".to_string(),
            message: format!(
                "owned and ran {} with status {}",
                request.tool_call_id, outcome.status
            ),
        })
    }

    /// Answers a refused action without dispatching it.
    fn refuse_host_execute(
        &mut self,
        id: &agent_client_protocol::v1::schema::RequestId,
        refusal: AcpHostRefusal,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        let message = refusal.message.clone();
        let frame = self.codec.host_execute_rejection(id.clone(), refusal)?;
        self.transport.write_frame(&frame)?;
        sink(AgentEvent::StatusChanged {
            run_id: self.run_id.clone(),
            phase: "host_execute_refused".to_string(),
            message,
        })
    }

    fn timeout(&self, stage: &str) -> AdapterError {
        AdapterError {
            message: format!(
                "acp {stage} timed out; stderr: {}",
                self.transport.stderr_tail()
            ),
        }
    }
}

#[cfg(test)]
mod tool_update_dedup_tests {
    use super::{ToolUpdateDedup, MAX_TOOL_UPDATE_DEDUP_BYTES, MAX_TOOL_UPDATE_DEDUP_ENTRIES};

    #[test]
    fn an_exact_duplicate_is_suppressed_and_distinct_frames_pass() {
        let mut d = ToolUpdateDedup::new();
        assert!(d.admit("a".to_string()));
        assert!(!d.admit("a".to_string()));
        assert!(d.admit("b".to_string()));
        assert!(!d.admit("b".to_string()));
    }

    #[test]
    fn residency_stays_bounded_under_distinct_flood_and_stored_duplicates_still_suppressed() {
        let mut d = ToolUpdateDedup::new();
        let first = "frame-0".to_string();
        assert!(d.admit(first.clone()));
        for i in 1..(MAX_TOOL_UPDATE_DEDUP_ENTRIES * 2) {
            assert!(d.admit(format!("frame-{i}")));
        }
        assert!(d.seen.len() <= MAX_TOOL_UPDATE_DEDUP_ENTRIES);
        assert!(d.bytes <= MAX_TOOL_UPDATE_DEDUP_BYTES);
        // The early stored frame's exact duplicate is still suppressed.
        assert!(!d.admit(first));
        // A late frame beyond the bound was not stored, so it passes again.
        let late = format!("frame-{}", MAX_TOOL_UPDATE_DEDUP_ENTRIES * 2 - 1);
        assert!(d.admit(late));
    }

    #[test]
    fn byte_bound_caps_residency_for_large_frames() {
        let mut d = ToolUpdateDedup::new();
        let big = "x".repeat(MAX_TOOL_UPDATE_DEDUP_BYTES / 4);
        for i in 0..16 {
            d.admit(format!("{big}-{i}"));
        }
        assert!(d.bytes <= MAX_TOOL_UPDATE_DEDUP_BYTES);
        assert!(d.seen.len() <= MAX_TOOL_UPDATE_DEDUP_ENTRIES);
    }
}
