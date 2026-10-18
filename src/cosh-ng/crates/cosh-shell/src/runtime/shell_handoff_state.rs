use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::runtime::prelude::ShellHandoffRequest;

#[derive(Debug, Default)]
pub(crate) struct ShellHandoffState {
    approved: VecDeque<PendingApprovedShellHandoff>,
    pending: VecDeque<PendingApprovedShellHandoff>,
}

impl ShellHandoffState {
    pub(crate) fn enqueue_approved_request(&mut self, request: ShellHandoffRequest) {
        self.approved.push_back(PendingApprovedShellHandoff {
            request,
            emitted_at: None,
            emitted_at_event_index: None,
            timeout_interrupt_sent: false,
        });
    }

    /// Emits one approved request only when no earlier request is still pending.
    ///
    /// The marker transport has one request sidecar and one parser claim slot,
    /// so overlapping emissions would replace the first request's identity.
    /// `event_index` is the shell-event count observed when the handoff is
    /// written to the PTY; the untracked-closure fallback only considers
    /// `ShellReady` events strictly after this index.
    pub(crate) fn emit_next_approved(&mut self, event_index: usize) -> Option<ShellHandoffRequest> {
        if !self.pending.is_empty() {
            return None;
        }
        let mut handoff = self.approved.pop_front()?;
        handoff.emitted_at = Some(Instant::now());
        handoff.emitted_at_event_index = Some(event_index);
        handoff.timeout_interrupt_sent = false;
        let request = handoff.request.clone();
        self.pending.push_back(handoff);
        Some(request)
    }

    pub(crate) fn pending_front(&self) -> Option<&PendingApprovedShellHandoff> {
        self.pending.front()
    }

    pub(crate) fn pop_pending(&mut self) -> Option<PendingApprovedShellHandoff> {
        self.pending.pop_front()
    }

    pub(crate) fn has_active_handoff(&self) -> bool {
        !self.approved.is_empty() || !self.pending.is_empty()
    }

    /// Whether `run_id` still owns an approved-but-unfinished handoff.
    ///
    /// Callers that decide whether a specific run's output can already reflect
    /// its command result must use this rather than [`Self::has_active_handoff`]:
    /// a handoff belonging to some other run says nothing about this one.
    pub(crate) fn has_active_handoff_for_run(&self, run_id: &str) -> bool {
        self.approved
            .iter()
            .chain(self.pending.iter())
            .any(|handoff| handoff.request.run_id == run_id)
    }

    pub(crate) fn mark_timeout_interrupt_if_elapsed(&mut self, timeout: Duration) -> bool {
        let Some(handoff) = self.pending.front_mut() else {
            return false;
        };
        let Some(emitted_at) = handoff.emitted_at else {
            return false;
        };
        if handoff.timeout_interrupt_sent || emitted_at.elapsed() < timeout {
            return false;
        }

        handoff.timeout_interrupt_sent = true;
        true
    }

    /// #2161 input-wait timeout: marks the front emitted handoff as
    /// interrupted once. Unlike [`Self::mark_timeout_interrupt_if_elapsed`]
    /// the clock lives outside (the sentinel's input-wait episode), so this
    /// only guards "emitted + not already interrupted".
    pub(crate) fn mark_input_wait_interrupt(&mut self) -> bool {
        let Some(handoff) = self.pending.front_mut() else {
            return false;
        };
        if handoff.emitted_at.is_none() || handoff.timeout_interrupt_sent {
            return false;
        }
        handoff.timeout_interrupt_sent = true;
        true
    }

    #[cfg(test)]
    pub(crate) fn approved_is_empty(&self) -> bool {
        self.approved.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn backdate_pending_emit_for_test(&mut self, age: Duration) {
        if let Some(handoff) = self.pending.front_mut() {
            handoff.emitted_at = Some(Instant::now() - age);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingApprovedShellHandoff {
    request: ShellHandoffRequest,
    emitted_at: Option<Instant>,
    emitted_at_event_index: Option<usize>,
    timeout_interrupt_sent: bool,
}

impl PendingApprovedShellHandoff {
    pub(crate) fn request(&self) -> &ShellHandoffRequest {
        &self.request
    }

    pub(crate) fn emitted_at_event_index(&self) -> Option<usize> {
        self.emitted_at_event_index
    }

    pub(crate) fn timeout_interrupt_sent(&self) -> bool {
        self.timeout_interrupt_sent
    }
}
