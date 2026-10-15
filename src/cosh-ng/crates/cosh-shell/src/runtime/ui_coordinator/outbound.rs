//! Single-slot outbound IPC write queue for one worker generation.
//!
//! The coordinator never buffers more than one Host message at a time: a second
//! request while a write is in flight is a state error, because the protocol has
//! exactly one outstanding frame per generation. Splitting the queue out keeps
//! the backpressure deadline and the post-completion deadline arming next to
//! each other, away from the lifecycle state machine.

use std::time::Instant;

use crate::ui::worker_protocol::{encode_host_message, HostMessage};

use super::process::ProcessError;
use super::state::{deadline, OutboundKind, PendingWrite};
use super::{CoordinatorError, UiWorkerCoordinator};

impl UiWorkerCoordinator {
    pub(super) fn queue_message(
        &mut self,
        frame_id: u64,
        message: &HostMessage,
        kind: OutboundKind,
        now: Instant,
    ) -> Result<(), CoordinatorError> {
        if self.outbound.is_some() || self.process.is_none() {
            return Err(CoordinatorError::InvalidState);
        }
        let bytes = encode_host_message(
            self.session_nonce,
            self.generation,
            frame_id,
            message,
            &self.policy.protocol_limits,
        )?;
        self.outbound = Some(PendingWrite {
            bytes,
            offset: 0,
            deadline: deadline(now, self.policy.backpressure_timeout)?,
            kind,
        });
        Ok(())
    }

    pub(super) fn flush_outbound(&mut self, now: Instant) -> Result<(), CoordinatorError> {
        let Some(mut pending) = self.outbound.take() else {
            return Ok(());
        };
        let complete = self
            .process
            .as_mut()
            .ok_or(CoordinatorError::InvalidState)?
            .write(&pending.bytes, &mut pending.offset)?;
        if complete {
            self.outbound_completed(pending.kind, now)?;
        } else if now >= pending.deadline {
            return Err(CoordinatorError::Process(ProcessError::Io(
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "worker IPC backpressure timeout",
                ),
            )));
        } else {
            self.outbound = Some(pending);
        }
        Ok(())
    }

    fn outbound_completed(
        &mut self,
        kind: OutboundKind,
        now: Instant,
    ) -> Result<(), CoordinatorError> {
        match kind {
            OutboundKind::Hello => {
                self.handshake_deadline = Some(deadline(now, self.policy.handshake_timeout)?);
            }
            OutboundKind::Render => {
                self.prepare_deadline = Some(deadline(now, self.policy.heartbeat_timeout)?);
            }
            OutboundKind::PublishedAck(key) => {
                self.prior_heartbeat_key = self
                    .heartbeat_due
                    .map(|(previous, _)| previous)
                    .filter(|previous| *previous != key);
                self.heartbeat_due = Some((key, deadline(now, self.policy.heartbeat_timeout)?));
                if let Some(request) = self.pending_latest_request.take() {
                    self.dispatch_render(request, now)?;
                }
            }
            OutboundKind::Shutdown => self.shutdown_sent = true,
        }
        Ok(())
    }
}
