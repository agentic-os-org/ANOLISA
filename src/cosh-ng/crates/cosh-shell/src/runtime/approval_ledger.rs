//! #1940: approval lifecycle ledger.
//!
//! Accounting index for control-protocol approval requests: every
//! `ToolPermissionRequest` owned by the active run is registered here,
//! and every provider response is marked off. (A request under a
//! foreign run id is denied at the registration door in `agent/poll.rs`
//! and never enters the ledger — a sweep could never reach its owner.)
//! Request facts stay in
//! `state.approvals.requests`; the ledger only keeps lifecycle markers
//! keyed by the #1939 identity contract (`run_id` + `request_id`), so it
//! can never drift from the single source of truth. The batch drain
//! assertion and the run-terminal sweep read this ledger to guarantee
//! that a dropped request is denied instead of silently starving the
//! provider (issue #1920's failure class).
//!
//! Maintenance contract: **any new code path that sends a
//! `control_response` for an approval request must also call
//! [`ApprovalLifecycleLedger::mark_responded`]** (directly, or via the
//! accounting wrappers in `approval/runtime.rs`). An unmarked response
//! lets the batch drain / run-terminal sweep re-deny an already-answered
//! request. The existing exits — `respond_provider_approval_to_owner`,
//! the reentrant shell deny in `agent/poll.rs`, and successful
//! host-executed delivery in `runtime/evidence_delivery.rs` — are the
//! reference points and are pinned by tests.

use std::collections::HashMap;

#[derive(Debug, Default)]
pub(crate) struct ApprovalLifecycleLedger {
    entries: HashMap<(String, String), ApprovalLifecycleMarker>,
}

#[derive(Debug, Default)]
struct ApprovalLifecycleMarker {
    responded: bool,
}

impl ApprovalLifecycleLedger {
    /// Registers a control request on first sight. Idempotent: a replay
    /// of the same `request_id` merges into the existing entry and never
    /// resets a recorded response.
    pub(crate) fn register(&mut self, run_id: &str, request_id: &str) {
        self.entries
            .entry((run_id.to_string(), request_id.to_string()))
            .or_default();
    }

    /// Marks a provider response as sent. A no-op for unregistered ids so
    /// non-approval control responses (questions, evidence) pass through
    /// without creating phantom entries.
    pub(crate) fn mark_responded(&mut self, run_id: &str, request_id: &str) {
        if let Some(marker) = self
            .entries
            .get_mut(&(run_id.to_string(), request_id.to_string()))
        {
            marker.responded = true;
        }
    }

    /// Request ids of the given run that never received a response.
    pub(crate) fn unresponded_for_run(&self, run_id: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .entries
            .iter()
            .filter(|((run, _), marker)| run == run_id && !marker.responded)
            .map(|((_, request_id), _)| request_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Drops every entry of a finished run so the ledger cannot grow
    /// across turns.
    pub(crate) fn clear_run(&mut self, run_id: &str) {
        self.entries.retain(|(run, _), _| run != run_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_is_idempotent_and_keeps_response_marker() {
        let mut ledger = ApprovalLifecycleLedger::default();
        ledger.register("run-1", "ctrl-1");
        ledger.mark_responded("run-1", "ctrl-1");
        ledger.register("run-1", "ctrl-1");
        assert!(ledger.unresponded_for_run("run-1").is_empty());
    }

    #[test]
    fn mark_responded_ignores_unregistered_ids() {
        let mut ledger = ApprovalLifecycleLedger::default();
        ledger.mark_responded("run-1", "question-1");
        assert!(ledger.unresponded_for_run("run-1").is_empty());
        ledger.register("run-1", "ctrl-1");
        assert_eq!(ledger.unresponded_for_run("run-1"), vec!["ctrl-1"]);
    }

    #[test]
    fn clear_run_scopes_to_one_run() {
        let mut ledger = ApprovalLifecycleLedger::default();
        ledger.register("run-1", "ctrl-1");
        ledger.register("run-2", "ctrl-2");
        ledger.clear_run("run-1");
        assert!(ledger.unresponded_for_run("run-1").is_empty());
        assert_eq!(ledger.unresponded_for_run("run-2"), vec!["ctrl-2"]);
    }
}
