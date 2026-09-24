//! Persisted projection gaps left by shrunken shell event snapshots.

use super::ShellAuditRecorder;
use crate::types::audit::{
    AuditEventOutcome, AuditEventV1, AuditIdentity, AuditOutcomeStatus, AuditRedaction,
    AuditSubject,
};

impl ShellAuditRecorder {
    /// Persists the projection gap left by a shrunken event snapshot.
    ///
    /// Returns whether the marker's primary write is durable so the caller
    /// marks the episode as reported only then and otherwise retries on the
    /// episode's next observation. Like the recovery markers it writes
    /// directly, keeping the result free of recovery-injection failures;
    /// only counters are recorded.
    pub(super) fn record_truncation(&mut self, snapshot_len: usize) -> bool {
        let event = AuditEventV1::shell(
            "audit.truncated",
            AuditIdentity {
                shell_session_id: Some(self.shell_session_id.clone()),
                ..AuditIdentity::default()
            },
            AuditEventOutcome {
                status: AuditOutcomeStatus::Failed,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "audit".to_string(),
                name: None,
            },
            &serde_json::json!({
                "operation": "event_snapshot_shrunk",
                "seen": self.seen_events,
                "len": snapshot_len,
            }),
            AuditRedaction::clean(),
        );
        match event {
            Ok(mut event) => {
                self.ensure_writer();
                let result = self
                    .writer
                    .as_mut()
                    .ok_or_else(|| "audit writer is unavailable".to_string())
                    .and_then(|writer| writer.append(&mut event, true));
                match result {
                    Ok(()) => true,
                    Err(error) => {
                        self.mark_degraded(&error);
                        false
                    }
                }
            }
            Err(error) => {
                self.mark_degraded(&error);
                false
            }
        }
    }
}
