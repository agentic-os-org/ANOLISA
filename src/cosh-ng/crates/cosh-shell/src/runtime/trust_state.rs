//! Approval trust state: session-scope trusted commands plus the run-scope
//! batch consent (issue #1773). Extracted from `ControlState` following the
//! per-domain split plan recorded for `runtime/state.rs` in the large-file
//! inventory.

use std::collections::HashSet;

#[derive(Debug, Default)]
pub(crate) struct ApprovalTrustState {
    session_trusted_commands: HashSet<String>,
    run_batch_consent: Option<String>,
}

impl ApprovalTrustState {
    pub(crate) fn trust_session_command(&mut self, key: String) {
        self.session_trusted_commands.insert(key);
    }
    pub(crate) fn session_trusted_commands(&self) -> &HashSet<String> {
        &self.session_trusted_commands
    }
    /// Grants turn-scope batch consent for `run_id`. In-memory only; every
    /// run exit path clears it so consent never outlives its turn.
    pub(crate) fn grant_run_batch_consent(&mut self, run_id: String) {
        self.run_batch_consent = Some(run_id);
    }
    pub(crate) fn clear_run_batch_consent(&mut self) {
        self.run_batch_consent = None;
    }
    pub(crate) fn run_batch_consent(&self) -> Option<&str> {
        self.run_batch_consent.as_deref()
    }
}
