use std::collections::HashSet;

use crate::agent::run::AgentRunOrigin;
use crate::evidence::output_policy::{shell_evidence_view, EvidenceFacts, EvidenceView};
use crate::runtime::prelude::{CommandBlock, ShellHandoffRequest};

#[derive(Debug, Default)]
pub(crate) struct EvidenceState {
    shell_command_completed: Vec<RuntimeShellCommandCompleted>,
    continued_shell_handoff_approvals: HashSet<String>,
}

impl EvidenceState {
    pub(crate) fn record_shell_command_completed(
        &mut self,
        evidence: RuntimeShellCommandCompleted,
    ) {
        self.shell_command_completed.push(evidence);
    }

    /// Claims **at most one** pending recovery, oldest first.
    ///
    /// A claim is irreversible — the evidence moves to `RecoveryQueued` and its
    /// approval id enters the dedup set — so a caller may only claim what it can
    /// start immediately. Starting one continuation polls the provider, which can
    /// surface a compaction recommendation; every further internal run would then
    /// be dropped by the start gate while already counting as claimed, losing
    /// that recovery for good. One per idle boundary keeps claim and start
    /// atomic; remaining recoveries are picked up at the next boundary. Same
    /// contract as [`Self::claim_stalled_provider_shell_handoff_continuations`].
    pub(crate) fn claim_pending_shell_handoff_continuations(
        &mut self,
    ) -> Vec<RuntimeShellCommandCompleted> {
        let mut requests = Vec::new();
        for evidence in self.shell_command_completed.iter_mut() {
            if evidence.continuation_state != ShellEvidenceContinuationState::PendingRecovery {
                continue;
            }
            let Some(approval_id) = evidence.approval_id.as_ref() else {
                continue;
            };
            if !self
                .continued_shell_handoff_approvals
                .insert(approval_id.clone())
            {
                continue;
            }
            evidence.continuation_state = ShellEvidenceContinuationState::RecoveryQueued;
            requests.push(evidence.clone());
            break;
        }
        requests
    }

    pub(crate) fn claim_stalled_provider_shell_handoff_continuations(
        &mut self,
    ) -> Vec<RuntimeShellCommandCompleted> {
        let mut requests = Vec::new();
        for evidence in self.shell_command_completed.iter_mut() {
            if !matches!(
                evidence.continuation_state,
                ShellEvidenceContinuationState::DeliveredToProvider
                    | ShellEvidenceContinuationState::ProviderProgressObserved
            ) {
                continue;
            }
            let Some(approval_id) = evidence.approval_id.as_ref() else {
                continue;
            };
            if !self
                .continued_shell_handoff_approvals
                .insert(approval_id.clone())
            {
                continue;
            }
            evidence.recovery_reason = Some("evidence_idle_timeout");
            evidence.continuation_state = ShellEvidenceContinuationState::RecoveryQueued;
            requests.push(evidence.clone());
            break;
        }
        requests
    }

    pub(crate) fn mark_provider_progress_observed(&mut self, closed: bool) {
        for evidence in &mut self.shell_command_completed {
            match evidence.continuation_state {
                ShellEvidenceContinuationState::DeliveredToProvider
                | ShellEvidenceContinuationState::ProviderProgressObserved => {
                    evidence.continuation_state = if closed {
                        ShellEvidenceContinuationState::Closed
                    } else {
                        ShellEvidenceContinuationState::ProviderProgressObserved
                    };
                }
                ShellEvidenceContinuationState::PendingRecovery
                | ShellEvidenceContinuationState::RecoveryQueued
                | ShellEvidenceContinuationState::Closed => {}
            }
        }
    }

    pub(crate) fn mark_recovery_reason(&mut self, approval_id: &str, reason: &'static str) {
        if let Some(evidence) = self
            .shell_command_completed
            .iter_mut()
            .rev()
            .find(|evidence| {
                evidence.approval_id.as_deref() == Some(approval_id)
                    && evidence.continuation_state == ShellEvidenceContinuationState::RecoveryQueued
            })
        {
            evidence.recovery_reason = Some(reason);
        }
    }

    pub(crate) fn latest_recovery(&self) -> Option<&RuntimeShellCommandCompleted> {
        self.shell_command_completed
            .iter()
            .rev()
            .find(|evidence| evidence.recovery_reason.is_some())
    }

    pub(crate) fn latest_shell_command_completed(&self) -> Option<&RuntimeShellCommandCompleted> {
        self.shell_command_completed.last()
    }

    pub(crate) fn has_open_provider_shell_evidence(&self) -> bool {
        self.shell_command_completed.iter().any(|evidence| {
            matches!(
                evidence.continuation_state,
                ShellEvidenceContinuationState::DeliveredToProvider
                    | ShellEvidenceContinuationState::ProviderProgressObserved
            )
        })
    }

    pub(crate) fn provider_visible_view(evidence: &RuntimeShellCommandCompleted) -> EvidenceView {
        shell_evidence_view(EvidenceFacts {
            shell_session_id: &evidence.shell_session_id,
            command_id: &evidence.command_block_id,
            command: &evidence.command,
            cwd: &evidence.cwd,
            end_cwd: &evidence.end_cwd,
            status: evidence.status,
            exit_code: evidence.exit_code,
            duration_ms: evidence.duration_ms,
            output_ref: evidence.terminal_output_ref.as_deref(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShellEvidenceDelivery {
    pub(crate) delivered: bool,
    pub(crate) status: &'static str,
    pub(crate) recovery_reason: Option<&'static str>,
    pub(crate) provider_preview_complete: bool,
}

impl ShellEvidenceDelivery {
    pub(crate) fn not_attempted() -> Self {
        Self {
            delivered: false,
            status: "not_attempted",
            recovery_reason: None,
            provider_preview_complete: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellEvidenceContinuationState {
    PendingRecovery,
    DeliveredToProvider,
    ProviderProgressObserved,
    RecoveryQueued,
    Closed,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeShellCommandCompleted {
    pub(crate) approval_id: Option<String>,
    pub(crate) origin: AgentRunOrigin,
    pub(crate) provider_request_id: Option<String>,
    pub(crate) tool_use_id: Option<String>,
    pub(crate) shell_session_id: String,
    pub(crate) command_block_id: String,
    pub(crate) command: String,
    /// Provider-facing command text (#2142 D-6), already bounded. For a
    /// handoff the provider authored this is the request's original text so
    /// the model can correlate result with call even when `command` carries
    /// the marker's redaction placeholder; every other source keeps the
    /// redacted form. Durable surfaces must keep using `command`.
    pub(crate) provider_command: String,
    pub(crate) cwd: String,
    pub(crate) end_cwd: String,
    pub(crate) status: &'static str,
    pub(crate) exit_code: i32,
    pub(crate) duration_ms: u64,
    pub(crate) terminal_output_ref: Option<String>,
    pub(crate) redaction_status: &'static str,
    pub(crate) provider_result_delivered: bool,
    pub(crate) provider_result_delivery_status: &'static str,
    pub(crate) recovery_reason: Option<&'static str>,
    pub(crate) continuation_state: ShellEvidenceContinuationState,
}

impl RuntimeShellCommandCompleted {
    pub(crate) fn from_shell_handoff(
        handoff: &ShellHandoffRequest,
        block: &CommandBlock,
        status: &'static str,
        origin: AgentRunOrigin,
    ) -> Self {
        let delivery = ShellEvidenceDelivery::not_attempted();
        // D-6 (#2142): the provider authored an approved_provider_shell_tool
        // command, so its result echoes the request's original text
        // (truncate-only); other sources keep the redacted block text.
        let provider_command = if handoff.source == "approved_provider_shell_tool" {
            crate::evidence::truncate_provider_authored_command_text(&handoff.command)
        } else {
            crate::evidence::redact_provider_command_text(&block.command)
        };
        Self {
            approval_id: Some(handoff.approval_id.clone()),
            origin,
            provider_request_id: handoff.request_id.clone(),
            tool_use_id: handoff.tool_use_id.clone(),
            shell_session_id: block.session_id.clone(),
            command_block_id: block.id.clone(),
            command: block.command.clone(),
            provider_command,
            cwd: block.cwd.clone(),
            end_cwd: block.end_cwd.clone(),
            status,
            exit_code: block.exit_code,
            duration_ms: block.duration_ms,
            terminal_output_ref: block.output.terminal_output_ref.clone(),
            redaction_status: "ref_only",
            provider_result_delivered: delivery.delivered,
            provider_result_delivery_status: delivery.status,
            recovery_reason: delivery.recovery_reason,
            continuation_state: ShellEvidenceContinuationState::PendingRecovery,
        }
    }

    pub(crate) fn apply_provider_result_delivery(&mut self, delivery: ShellEvidenceDelivery) {
        self.provider_result_delivered = delivery.delivered;
        self.provider_result_delivery_status = delivery.status;
        self.recovery_reason = delivery.recovery_reason;
        self.continuation_state = if delivery.delivered {
            ShellEvidenceContinuationState::DeliveredToProvider
        } else {
            ShellEvidenceContinuationState::PendingRecovery
        };
    }

    pub(crate) fn selected_execution_path(&self) -> &'static str {
        if self.provider_result_delivered {
            "control_protocol_host_executed_shell_result"
        } else {
            "foreground_shell_handoff_recovery"
        }
    }

    pub(crate) fn path_selection_reason(&self) -> &'static str {
        if self.provider_result_delivered {
            "provider advertised host-executed shell result support"
        } else if let Some(reason) = self.recovery_reason {
            reason
        } else {
            "provider result was not delivered; shell evidence continuation required"
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::prelude::AgentRunOrigin;

    use super::{
        EvidenceState, RuntimeShellCommandCompleted, ShellEvidenceContinuationState,
        ShellEvidenceDelivery,
    };

    #[test]
    fn evidence_state_claims_pending_continuations_once() {
        let mut state = EvidenceState::default();
        state.record_shell_command_completed(shell_evidence(
            Some("req-1"),
            false,
            "provider_run_not_active",
            Some("provider run was not active"),
        ));
        state.record_shell_command_completed(shell_evidence(
            Some("req-2"),
            true,
            "delivered",
            None,
        ));

        let first = state.claim_pending_shell_handoff_continuations();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].approval_id.as_deref(), Some("req-1"));
        assert_eq!(
            first[0].continuation_state,
            ShellEvidenceContinuationState::RecoveryQueued
        );

        let second = state.claim_pending_shell_handoff_continuations();
        assert!(second.is_empty());
    }

    // Two handoffs can complete in one dispatch batch. Claiming both at once
    // would gamble the second on a start path that may already be closed by the
    // time it runs, and a lost claim can never be retried — so each boundary
    // takes exactly one and the rest stay claimable.
    #[test]
    fn evidence_state_claims_one_pending_continuation_per_boundary() {
        let mut state = EvidenceState::default();
        for approval_id in ["req-1", "req-2"] {
            state.record_shell_command_completed(shell_evidence(
                Some(approval_id),
                false,
                "provider_run_not_active",
                Some("provider run was not active"),
            ));
        }

        let first = state.claim_pending_shell_handoff_continuations();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].approval_id.as_deref(), Some("req-1"));

        let second = state.claim_pending_shell_handoff_continuations();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].approval_id.as_deref(), Some("req-2"));

        assert!(state.claim_pending_shell_handoff_continuations().is_empty());
    }

    #[test]
    fn evidence_state_tracks_latest_recovery() {
        let mut state = EvidenceState::default();
        state.record_shell_command_completed(shell_evidence(
            Some("req-1"),
            false,
            "unsupported",
            Some("provider unsupported"),
        ));
        state.record_shell_command_completed(shell_evidence(
            Some("req-2"),
            false,
            "provider_run_not_active",
            Some("provider missing"),
        ));

        let latest = state.latest_recovery().expect("latest recovery");
        assert_eq!(latest.approval_id.as_deref(), Some("req-2"));
        assert_eq!(
            latest.provider_result_delivery_status,
            "provider_run_not_active"
        );
    }

    #[test]
    fn evidence_state_applies_provider_delivery_metadata() {
        let mut evidence = shell_evidence(Some("req-1"), false, "not_attempted", None);

        evidence.apply_provider_result_delivery(ShellEvidenceDelivery {
            delivered: true,
            status: "delivered",
            recovery_reason: None,
            provider_preview_complete: true,
        });

        assert!(evidence.provider_result_delivered);
        assert_eq!(evidence.provider_result_delivery_status, "delivered");
        assert_eq!(
            evidence.selected_execution_path(),
            "control_protocol_host_executed_shell_result"
        );
        assert_eq!(
            evidence.path_selection_reason(),
            "provider advertised host-executed shell result support"
        );
        assert_eq!(
            evidence.continuation_state,
            ShellEvidenceContinuationState::DeliveredToProvider
        );
    }

    #[test]
    fn provider_progress_observed_closes_delivered_evidence_for_recovery_claims() {
        let mut state = EvidenceState::default();
        state.record_shell_command_completed(shell_evidence(
            Some("req-1"),
            true,
            "delivered",
            None,
        ));

        state.mark_provider_progress_observed(false);
        assert!(state.claim_pending_shell_handoff_continuations().is_empty());
        assert_eq!(
            state.shell_command_completed[0].continuation_state,
            ShellEvidenceContinuationState::ProviderProgressObserved
        );

        state.mark_provider_progress_observed(true);
        assert_eq!(
            state.shell_command_completed[0].continuation_state,
            ShellEvidenceContinuationState::Closed
        );
    }

    #[test]
    fn stalled_provider_claim_only_marks_one_open_evidence_at_a_time() {
        let mut state = EvidenceState::default();
        state.record_shell_command_completed(shell_evidence(
            Some("req-1"),
            true,
            "delivered",
            None,
        ));
        state.record_shell_command_completed(shell_evidence(
            Some("req-2"),
            true,
            "delivered",
            None,
        ));

        let first = state.claim_stalled_provider_shell_handoff_continuations();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].approval_id.as_deref(), Some("req-1"));
        assert_eq!(first[0].recovery_reason, Some("evidence_idle_timeout"));
        assert_eq!(
            state.shell_command_completed[0].continuation_state,
            ShellEvidenceContinuationState::RecoveryQueued
        );
        assert_eq!(
            state.shell_command_completed[1].continuation_state,
            ShellEvidenceContinuationState::DeliveredToProvider
        );

        let second = state.claim_stalled_provider_shell_handoff_continuations();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].approval_id.as_deref(), Some("req-2"));
    }

    #[test]
    fn recovery_paths_share_one_claim_per_evidence() {
        let mut state = EvidenceState::default();
        state.record_shell_command_completed(shell_evidence(
            Some("req-1"),
            true,
            "delivered",
            None,
        ));

        assert_eq!(
            state
                .claim_stalled_provider_shell_handoff_continuations()
                .len(),
            1
        );
        assert!(state.claim_pending_shell_handoff_continuations().is_empty());
        assert!(state
            .claim_stalled_provider_shell_handoff_continuations()
            .is_empty());
    }

    #[test]
    fn failed_resume_updates_claimed_evidence_reason() {
        let mut state = EvidenceState::default();
        state.record_shell_command_completed(shell_evidence(
            Some("req-1"),
            false,
            "provider_run_not_active",
            Some("provider run was not active"),
        ));
        assert_eq!(state.claim_pending_shell_handoff_continuations().len(), 1);

        state.mark_recovery_reason("req-1", "resume_failed");

        assert_eq!(
            state
                .latest_recovery()
                .and_then(|item| item.recovery_reason),
            Some("resume_failed")
        );
    }

    fn shell_evidence(
        approval_id: Option<&str>,
        provider_result_delivered: bool,
        provider_result_delivery_status: &'static str,
        recovery_reason: Option<&'static str>,
    ) -> RuntimeShellCommandCompleted {
        RuntimeShellCommandCompleted {
            approval_id: approval_id.map(ToString::to_string),
            origin: AgentRunOrigin::Standard,
            provider_request_id: Some("ctrl-1".to_string()),
            tool_use_id: Some("toolu-1".to_string()),
            shell_session_id: "raw-test".to_string(),
            command_block_id: "cmd-1".to_string(),
            command: "df -h".to_string(),
            provider_command: "df -h".to_string(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            status: "completed",
            exit_code: 0,
            duration_ms: 10,
            terminal_output_ref: None,
            redaction_status: "ref_only",
            provider_result_delivered,
            provider_result_delivery_status,
            recovery_reason,
            continuation_state: if provider_result_delivered {
                ShellEvidenceContinuationState::DeliveredToProvider
            } else {
                ShellEvidenceContinuationState::PendingRecovery
            },
        }
    }
}
