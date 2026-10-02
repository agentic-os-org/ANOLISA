use crate::approval::handoff::raw_bash_command;
use crate::journal::audit::ShellApprovalAuditInput;
use crate::runtime::prelude::*;

pub(crate) fn approval_audit_input(
    request: &RuntimeApprovalRequest,
) -> ShellApprovalAuditInput<'_> {
    ShellApprovalAuditInput {
        id: &request.id,
        audit_ref: request.audit_ref.as_deref(),
        session_id: &request.session_id,
        run_id: &request.run_id,
        request_id: request.request_id.as_deref(),
        tool_use_id: request.tool_use_id.as_deref(),
        subject: &request.subject,
        risk: request.risk,
        assessment: request
            .assessment
            .as_ref()
            .map(|assessment| assessment.primary_reason),
        preview: &request.preview,
        status: request.status.label(),
    }
}

pub(super) fn approval_journal_entry(
    request: &RuntimeApprovalRequest,
    actor: &'static str,
) -> RuntimeApprovalJournalEntry {
    RuntimeApprovalJournalEntry {
        id: request.id.clone(),
        audit_ref: request.audit_ref.clone(),
        run_id: request.run_id.clone(),
        source: request.source,
        kind: request.kind,
        subject: request.subject.clone(),
        preview: request.preview.clone(),
        preview_hash: ShellHandoffRequest::new(
            raw_bash_command(&request.preview).to_string(),
            request.preview.clone(),
            request.source,
            actor,
            request.id.clone(),
            request.run_id.clone(),
            now_ms(),
        )
        .map(|request| request.preview_hash)
        .unwrap_or_else(|_| "<not-applicable>".to_string()),
        risk: request.risk,
        request_id: request.request_id.clone(),
        tool_use_id: request.tool_use_id.clone(),
        actor,
        decision: request.status,
        execution_path: request.execution_path,
        command_block_id: request.command_block_id.clone(),
        redaction_status: request.redaction_status,
        assessment: request.assessment.clone(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
