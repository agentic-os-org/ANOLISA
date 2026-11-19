//! Typed audit lifecycle projections used by the Core control loop.

use std::time::Instant;

use cosh_types::audit::{
    AuditApprovalData, AuditDecisionData, AuditLifecycleData, AuditOutcomeStatus,
    AuditProviderData, AuditToolData, KnownAuditEventType,
};

use super::CoreAuditRecorder;

#[derive(Clone, Copy)]
pub(crate) struct CoreAuditScope<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) turn_id: Option<&'a str>,
    pub(crate) request_id: Option<&'a str>,
    pub(crate) tool_use_id: Option<&'a str>,
}

impl<'a> CoreAuditScope<'a> {
    pub(crate) fn run(run_id: &'a str) -> Self {
        Self {
            run_id,
            turn_id: None,
            request_id: None,
            tool_use_id: None,
        }
    }

    pub(crate) fn turn(run_id: &'a str, turn_id: &'a str) -> Self {
        Self {
            run_id,
            turn_id: Some(turn_id),
            request_id: None,
            tool_use_id: None,
        }
    }

    pub(crate) fn request(
        run_id: &'a str,
        turn_id: Option<&'a str>,
        request_id: &'a str,
        tool_use_id: Option<&'a str>,
    ) -> Self {
        Self {
            run_id,
            turn_id,
            request_id: Some(request_id),
            tool_use_id,
        }
    }

    pub(crate) fn tool(run_id: &'a str, turn_id: &'a str, tool_use_id: &'a str) -> Self {
        Self {
            run_id,
            turn_id: Some(turn_id),
            request_id: None,
            tool_use_id: Some(tool_use_id),
        }
    }
}

impl CoreAuditRecorder {
    fn scope_identity(&self, scope: CoreAuditScope<'_>) -> cosh_types::audit::AuditIdentity {
        self.identity(
            Some(scope.run_id),
            scope.turn_id,
            scope.request_id,
            scope.tool_use_id,
        )
    }

    pub(crate) fn record_hook_decision(
        &mut self,
        scope: CoreAuditScope<'_>,
        hook: &str,
        status: AuditOutcomeStatus,
        decision: &str,
    ) {
        let identity = self.scope_identity(scope);
        self.ordinary(
            KnownAuditEventType::HookDecision,
            identity,
            status,
            "hook",
            Some(hook),
            &AuditDecisionData {
                decision: decision.to_string(),
                reason_code: None,
                policy_version: Some("core-hooks-v1".to_string()),
                duration_ms: None,
            },
        );
    }

    pub(crate) fn record_session_hook_decision(&mut self, hook: &str, decision: &str) {
        let identity = self.identity(None, None, None, None);
        self.ordinary(
            KnownAuditEventType::HookDecision,
            identity,
            AuditOutcomeStatus::Success,
            "hook",
            Some(hook),
            &AuditDecisionData {
                decision: decision.to_string(),
                reason_code: None,
                policy_version: Some("core-hooks-v1".to_string()),
                duration_ms: None,
            },
        );
    }

    pub(crate) fn record_turn_started(&mut self, scope: CoreAuditScope<'_>) {
        let identity = self.scope_identity(scope);
        self.ordinary(
            KnownAuditEventType::TurnStarted,
            identity,
            AuditOutcomeStatus::Started,
            "turn",
            None,
            &AuditLifecycleData::default(),
        );
        self.open_turn = Some(super::OpenTurnAudit {
            run_id: scope.run_id.to_string(),
            turn_id: scope.turn_id.unwrap_or_default().to_string(),
            provider: None,
        });
    }

    pub(crate) fn record_turn_terminal(
        &mut self,
        scope: CoreAuditScope<'_>,
        status: AuditOutcomeStatus,
        reason_code: Option<&str>,
    ) {
        self.open_turn = None;
        let event_type = if status == AuditOutcomeStatus::Success {
            KnownAuditEventType::TurnCompleted
        } else {
            KnownAuditEventType::TurnFailed
        };
        let identity = self.scope_identity(scope);
        self.ordinary(
            event_type,
            identity,
            status,
            "turn",
            None,
            &AuditLifecycleData {
                reason_code: reason_code.map(str::to_string),
                ..AuditLifecycleData::default()
            },
        );
    }

    pub(crate) fn record_provider_started(
        &mut self,
        scope: CoreAuditScope<'_>,
        provider: &str,
        data: &AuditProviderData,
    ) -> Result<(), String> {
        let identity = self.scope_identity(scope);
        let result = self.barrier(
            KnownAuditEventType::ProviderRequestStarted,
            identity,
            AuditOutcomeStatus::Started,
            "provider",
            Some(provider),
            data,
        );
        match result {
            Ok(()) => {
                if let Some(open) = &mut self.open_turn {
                    open.provider = Some(super::OpenProviderAudit {
                        request_id: scope.request_id.unwrap_or_default().to_string(),
                        provider_type: provider.to_string(),
                        data: data.clone(),
                        started: Instant::now(),
                    });
                }
            }
            Err(_) => {
                // The provider was never called; end the turn with an audit
                // failure so a later parent loss cannot append a cancelled
                // terminal for a request that never started.
                self.record_turn_terminal(
                    CoreAuditScope::turn(scope.run_id, scope.turn_id.unwrap_or_default()),
                    AuditOutcomeStatus::Failed,
                    Some("audit_barrier_failed"),
                );
            }
        }
        result
    }

    pub(crate) fn record_provider_terminal(
        &mut self,
        scope: CoreAuditScope<'_>,
        provider: &str,
        data: &AuditProviderData,
        status: AuditOutcomeStatus,
        finish_category: &str,
        duration_ms: u64,
    ) {
        if let Some(open) = &mut self.open_turn {
            open.provider = None;
        }
        let event_type = match status {
            AuditOutcomeStatus::Success => KnownAuditEventType::ProviderRequestCompleted,
            AuditOutcomeStatus::Cancelled => KnownAuditEventType::ProviderRequestCancelled,
            _ => KnownAuditEventType::ProviderRequestFailed,
        };
        let identity = self.scope_identity(scope);
        self.ordinary(
            event_type,
            identity,
            status,
            "provider",
            Some(provider),
            &AuditProviderData {
                duration_ms: Some(duration_ms),
                finish_category: Some(finish_category.to_string()),
                ..data.clone()
            },
        );
    }

    /// Closes the lifecycles a parent-loss cancellation leaves open, with their
    /// original IDs. Normal terminals clear the tracking, so a turn that already
    /// completed can never receive a second terminal here.
    pub(crate) fn finalize_parent_lost(&mut self) {
        let Some(open) = self.open_turn.take() else {
            return;
        };
        if let Some(provider) = open.provider {
            self.record_provider_terminal(
                CoreAuditScope::request(
                    &open.run_id,
                    Some(&open.turn_id),
                    &provider.request_id,
                    None,
                ),
                &provider.provider_type,
                &provider.data,
                AuditOutcomeStatus::Cancelled,
                "parent_lost",
                provider.started.elapsed().as_millis() as u64,
            );
        }
        self.record_turn_terminal(
            CoreAuditScope::turn(&open.run_id, &open.turn_id),
            AuditOutcomeStatus::Cancelled,
            Some("parent_lost"),
        );
    }

    pub(crate) fn record_tool_requested(
        &mut self,
        scope: CoreAuditScope<'_>,
        name: &str,
        data: &AuditToolData,
    ) {
        let identity = self.scope_identity(scope);
        self.ordinary(
            KnownAuditEventType::ToolRequested,
            identity,
            AuditOutcomeStatus::Started,
            "tool",
            Some(name),
            data,
        );
    }

    pub(crate) fn record_tool_execution_started(
        &mut self,
        scope: CoreAuditScope<'_>,
        name: &str,
        data: &AuditToolData,
    ) -> Result<(), String> {
        let identity = self.scope_identity(scope);
        self.barrier(
            KnownAuditEventType::ToolExecutionStarted,
            identity,
            AuditOutcomeStatus::Started,
            "tool",
            Some(name),
            data,
        )
    }

    pub(crate) fn record_tool_terminal(
        &mut self,
        scope: CoreAuditScope<'_>,
        name: &str,
        base: &AuditToolData,
        is_error: bool,
        duration_ms: u64,
        output_bytes: u64,
    ) {
        let identity = self.scope_identity(scope);
        self.ordinary(
            if is_error {
                KnownAuditEventType::ToolFailed
            } else {
                KnownAuditEventType::ToolCompleted
            },
            identity,
            if is_error {
                AuditOutcomeStatus::Failed
            } else {
                AuditOutcomeStatus::Success
            },
            "tool",
            Some(name),
            &AuditToolData {
                duration_ms: Some(duration_ms),
                result_category: Some(if is_error {
                    "error".to_string()
                } else {
                    "success".to_string()
                }),
                output_bytes: Some(output_bytes),
                ..base.clone()
            },
        );
    }

    pub(crate) fn record_approval_requested(
        &mut self,
        scope: CoreAuditScope<'_>,
        subject: &str,
        assessment: &str,
        preview_hash: Option<String>,
    ) -> Option<String> {
        let identity = self.scope_identity(scope);
        self.ordinary_ref(
            KnownAuditEventType::ApprovalRequested,
            identity,
            AuditOutcomeStatus::Started,
            "approval",
            Some(subject),
            &AuditApprovalData {
                assessment: Some(assessment.to_string()),
                preview_hash,
                ..AuditApprovalData::default()
            },
        )
    }

    /// Records an approval request that could not be sent to its decider.
    ///
    /// The approval still owes a terminal event, so this is an
    /// `approval.resolved` with `decision=emit_failed` and the transport
    /// failure class as the reason code. Delivery is unknown, not proven
    /// absent — what the record asserts is that this core never obtained a
    /// decision, which production audit must not fold into a user deny (#1994).
    pub(crate) fn record_approval_emit_failed(
        &mut self,
        scope: CoreAuditScope<'_>,
        subject: &str,
        assessment: Option<&str>,
        error_class: &str,
    ) -> Result<(), String> {
        let identity = self.scope_identity(scope);
        self.barrier(
            KnownAuditEventType::ApprovalResolved,
            identity,
            AuditOutcomeStatus::Failed,
            "approval",
            Some(subject),
            &AuditApprovalData {
                assessment: assessment.map(str::to_string),
                decision: Some("emit_failed".to_string()),
                reason_code: Some(format!("control_transport_{error_class}")),
                ..AuditApprovalData::default()
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_approval_resolved(
        &mut self,
        scope: CoreAuditScope<'_>,
        subject: &str,
        status: AuditOutcomeStatus,
        assessment: Option<&str>,
        decision: &str,
        wait_ms: Option<u64>,
    ) -> Result<(), String> {
        let identity = self.scope_identity(scope);
        self.barrier(
            KnownAuditEventType::ApprovalResolved,
            identity,
            status,
            "approval",
            Some(subject),
            &AuditApprovalData {
                assessment: assessment.map(str::to_string),
                decision: Some(decision.to_string()),
                wait_ms,
                ..AuditApprovalData::default()
            },
        )
    }
}
