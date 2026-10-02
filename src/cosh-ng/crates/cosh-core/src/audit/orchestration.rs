//! Typed audit lifecycle projections used by the Core control loop.

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
    }

    pub(crate) fn record_turn_terminal(
        &mut self,
        scope: CoreAuditScope<'_>,
        status: AuditOutcomeStatus,
        reason_code: Option<&str>,
    ) {
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
        self.barrier(
            KnownAuditEventType::ProviderRequestStarted,
            identity,
            AuditOutcomeStatus::Started,
            "provider",
            Some(provider),
            data,
        )
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
