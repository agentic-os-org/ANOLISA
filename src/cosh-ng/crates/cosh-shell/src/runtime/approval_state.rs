//! Approval runtime state and the request and journal records it owns.

use std::collections::{HashMap, HashSet};

use crate::agent::run::AgentRunOrigin;
use crate::runtime::state_prelude::ApprovalPanelAction;

#[derive(Default)]
pub(crate) struct ApprovalState {
    pub(crate) handled_actions: HashSet<String>,
    pub(crate) requests: Vec<RuntimeApprovalRequest>,
    pub(crate) focus: HashMap<String, ApprovalPanelAction>,
    pub(crate) expanded_cards: HashSet<String>,
    pub(crate) active_panel_id: Option<String>,
    pub(crate) active_panel_height: usize,
    pub(crate) journal: Vec<RuntimeApprovalJournalEntry>,
}

impl ApprovalState {
    pub(crate) fn next_request_id(&self) -> String {
        format!("req-{}", self.requests.len() + 1)
    }

    pub(crate) fn mark_foreground_shell_execution(
        &mut self,
        approval_id: &str,
        command_block_id: &str,
    ) {
        for request in &mut self.requests {
            if request.id == approval_id {
                request.execution_path = Some("foreground_shell_pty");
                request.command_block_id = Some(command_block_id.to_string());
                request.redaction_status = Some("ref_only");
            }
        }
        for entry in &mut self.journal {
            if entry.id == approval_id {
                entry.execution_path = Some("foreground_shell_pty");
                entry.command_block_id = Some(command_block_id.to_string());
                entry.redaction_status = Some("ref_only");
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeApprovalRequest {
    pub(crate) id: String,
    pub(crate) audit_ref: Option<String>,
    pub(crate) run_id: String,
    pub(crate) origin: AgentRunOrigin,
    pub(crate) session_id: String,
    pub(crate) cwd: String,
    pub(crate) source: &'static str,
    pub(crate) provider_shell_request_kind: ProviderShellRequestKind,
    pub(crate) kind: ApprovalRequestKind,
    pub(crate) subject: String,
    pub(crate) preview: String,
    pub(crate) risk: &'static str,
    pub(crate) request_id: Option<String>,
    pub(crate) tool_use_id: Option<String>,
    pub(crate) tool_input: Option<serde_json::Value>,
    pub(crate) original_user_request: Option<String>,
    pub(crate) status: ApprovalRequestStatus,
    pub(crate) execution_path: Option<&'static str>,
    pub(crate) command_block_id: Option<String>,
    pub(crate) redaction_status: Option<&'static str>,
    pub(crate) assessment: Option<RuntimeCommandAssessmentSummary>,
    pub(crate) hook_requires_approval: bool,
    pub(crate) hook_warnings: Vec<HookWarning>,
}

/// Structured hook warning carrying its source and decision.
#[derive(Debug, Clone)]
pub(crate) struct HookWarning {
    pub(crate) hook_name: String,
    pub(crate) message: String,
    pub(crate) decision: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeCommandAssessmentSummary {
    pub(crate) impact: &'static str,
    pub(crate) execution: &'static str,
    pub(crate) confidence: &'static str,
    pub(crate) primary_reason: &'static str,
    pub(crate) reason_trace: String,
    pub(crate) auto_allow: Option<&'static str>,
    pub(crate) output_stability: &'static str,
    pub(crate) output_exposure: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderShellRequestKind {
    ControlPermission,
    StreamedToolCallFallback,
    LocalApproval,
}

impl ProviderShellRequestKind {
    pub(crate) fn is_control_permission(self) -> bool {
        matches!(self, Self::ControlPermission)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeApprovalJournalEntry {
    pub(crate) id: String,
    pub(crate) audit_ref: Option<String>,
    pub(crate) run_id: String,
    pub(crate) source: &'static str,
    pub(crate) kind: ApprovalRequestKind,
    pub(crate) subject: String,
    pub(crate) preview: String,
    pub(crate) preview_hash: String,
    pub(crate) risk: &'static str,
    pub(crate) request_id: Option<String>,
    pub(crate) tool_use_id: Option<String>,
    pub(crate) actor: &'static str,
    pub(crate) decision: ApprovalRequestStatus,
    pub(crate) execution_path: Option<&'static str>,
    pub(crate) command_block_id: Option<String>,
    pub(crate) redaction_status: Option<&'static str>,
    pub(crate) assessment: Option<RuntimeCommandAssessmentSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalRequestKind {
    Tool,
    ShellCommand,
    TurnExtension,
}

impl ApprovalRequestKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Tool => "tool request",
            Self::ShellCommand => "shell command request",
            Self::TurnExtension => "turn budget extension",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalRequestStatus {
    Pending,
    Approved,
    Blocked,
    Denied,
    Cancelled,
}

impl ApprovalRequestStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Blocked => "blocked",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
        }
    }
}
