//! Persisted projection gaps left by shrunken shell event snapshots.

use super::ShellAuditRecorder;
use crate::types::audit::{
    AuditEventOutcome, AuditEventV1, AuditIdentity, AuditOutcomeStatus, AuditRedaction,
    AuditSubject, AuditTruncationData,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TruncationGap {
    seen: usize,
    len: usize,
}

/// Delivery state of the current shrink episode's gap marker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum TruncationReport {
    /// No shrink episode is open.
    #[default]
    Idle,
    /// The marker has not entered the segment yet.
    Pending(TruncationGap),
    /// The marker entered the segment but its flush or sync failed.
    Unsynced(TruncationGap),
    /// The marker is durable; growth can re-arm the next episode.
    Reported(TruncationGap),
}

impl ShellAuditRecorder {
    /// Opens or retries one shrink episode without replacing its first gap.
    pub(super) fn report_truncation(&mut self, snapshot_len: usize) {
        if self.truncation == TruncationReport::Idle {
            self.truncation = TruncationReport::Pending(TruncationGap {
                seen: self.seen_events,
                len: snapshot_len,
            });
        }
        self.retry_truncation();
    }

    /// Retries only the undelivered step of an open shrink episode.
    pub(super) fn retry_truncation(&mut self) -> bool {
        self.truncation = match self.truncation {
            TruncationReport::Idle | TruncationReport::Reported(_) => self.truncation,
            TruncationReport::Pending(gap) => self.stage_truncation(gap),
            TruncationReport::Unsynced(gap) => self.commit_truncation(gap),
        };
        matches!(
            self.truncation,
            TruncationReport::Idle | TruncationReport::Reported(_)
        )
    }

    /// Re-arms only after both durable reporting and projection growth.
    pub(super) fn finish_truncation_after_projection(&mut self, snapshot_len: usize) {
        if matches!(
            self.truncation,
            TruncationReport::Reported(gap) if snapshot_len > gap.seen
        ) {
            self.truncation = TruncationReport::Idle;
        }
    }

    fn stage_truncation(&mut self, gap: TruncationGap) -> TruncationReport {
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
            &AuditTruncationData {
                operation: "event_snapshot_shrunk".to_string(),
                seen: gap.seen as u64,
                len: gap.len as u64,
            },
            AuditRedaction::clean(),
        );
        let mut event = match event {
            Ok(event) => event,
            Err(error) => {
                self.mark_degraded(&error);
                return TruncationReport::Pending(gap);
            }
        };
        self.ensure_writer();
        // This bounded marker is smaller than the segment buffer, so a stage
        // error means no marker bytes entered the segment.
        let staged = self
            .writer
            .as_mut()
            .ok_or_else(|| "audit writer is unavailable".to_string())
            .and_then(|writer| writer.stage(&mut event));
        match staged {
            Ok(()) => self.commit_truncation(gap),
            Err(error) => {
                self.mark_degraded(&error);
                TruncationReport::Pending(gap)
            }
        }
    }

    fn commit_truncation(&mut self, gap: TruncationGap) -> TruncationReport {
        let committed = self
            .writer
            .as_mut()
            .ok_or_else(|| "audit writer is unavailable".to_string())
            .and_then(|writer| writer.commit(true));
        match committed {
            Ok(()) => TruncationReport::Reported(gap),
            Err(error) => {
                self.mark_degraded(&error);
                TruncationReport::Unsynced(gap)
            }
        }
    }
}
