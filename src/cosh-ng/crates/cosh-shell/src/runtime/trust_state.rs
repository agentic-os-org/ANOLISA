//! Approval trust state: session-scope trusted commands plus the run-scope
//! batch consent (issue #1773). Extracted from `ControlState` following the
//! per-domain split plan recorded for `runtime/state.rs` in the large-file
//! inventory.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub(crate) struct ApprovalTrustState {
    session_trusted_commands: HashSet<String>,
    run_batch_consent: Option<String>,
    /// Commands whose last attempt in their originating run left the effect state uncertain
    /// (failed / interrupted / timed out / untracked). Turn-scope batch
    /// consent must not sweep a retry of these: the user consented to the
    /// turn, not to blindly replaying a command nobody can account for.
    turn_consent_blocked_commands: HashMap<String, HashSet<String>>,
}

impl ApprovalTrustState {
    pub(crate) fn trust_session_command(&mut self, key: String) {
        self.session_trusted_commands.insert(key);
    }
    pub(crate) fn session_trusted_commands(&self) -> &HashSet<String> {
        &self.session_trusted_commands
    }
    /// Drops a single session trust key. Used when a command that may already
    /// have taken effect ends failed or unknown: an earlier AlwaysTrust must
    /// not cover a retry of it, so the next attempt has to ask again.
    pub(crate) fn revoke_session_command(&mut self, key: &str) -> bool {
        self.session_trusted_commands.remove(key)
    }
    /// Grants turn-scope batch consent for `run_id`. In-memory only; every
    /// run exit path clears it so consent never outlives its turn.
    pub(crate) fn grant_run_batch_consent(&mut self, run_id: String) {
        self.run_batch_consent = Some(run_id);
    }
    pub(crate) fn clear_run_batch_consent(&mut self) {
        self.run_batch_consent = None;
        // Captured sweeps retain their own run-bound snapshot across owner
        // teardown; only the live blocks expire here.
        self.turn_consent_blocked_commands.clear();
    }
    pub(crate) fn run_batch_consent(&self) -> Option<&str> {
        self.run_batch_consent.as_deref()
    }
    /// Excludes one command from turn-scope batch consent. Used when an
    /// attempt of it left the effect state uncertain: the user consented to
    /// the turn, not to replaying a command nobody can account for.
    pub(crate) fn block_command_for_turn_consent(&mut self, run_id: &str, key: &str) {
        self.turn_consent_blocked_commands
            .entry(run_id.to_string())
            .or_default()
            .insert(key.to_string());
    }
    pub(crate) fn is_blocked_for_turn_consent(&self, run_id: &str, key: &str) -> bool {
        self.turn_consent_blocked_commands
            .get(run_id)
            .is_some_and(|commands| commands.contains(key))
    }
    pub(crate) fn blocked_commands_for_run(&self, run_id: &str) -> HashSet<String> {
        self.turn_consent_blocked_commands
            .get(run_id)
            .cloned()
            .unwrap_or_default()
    }
}
