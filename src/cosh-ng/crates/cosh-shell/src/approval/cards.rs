use crate::runtime::prelude::*;
use crate::tools::{classify_command_interaction, PtyRequirement};

use super::handoff::shell_handoff_command_from_request;

pub(crate) fn render_approval_journal<W: Write>(
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let entries = state
        .approvals
        .journal
        .iter()
        .map(|entry| ApprovalJournalEntryModel {
            id: &entry.id,
            run_id: &entry.run_id,
            source: entry.source,
            decision: entry.decision.label(),
            kind: entry.kind.label(),
            risk: entry.risk,
            subject: &entry.subject,
            preview: &entry.preview,
            preview_hash: &entry.preview_hash,
            request_id: entry.request_id.as_deref(),
            tool_use_id: entry.tool_use_id.as_deref(),
            actor: entry.actor,
            execution_path: entry.execution_path,
            command_block_id: entry.command_block_id.as_deref(),
            redaction_status: entry.redaction_status,
            assessment: entry.assessment.as_ref().map(assessment_summary_model),
            audit_ref: entry.audit_ref.as_deref(),
        })
        .collect::<Vec<_>>();
    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_approval_journal_panel(output, ApprovalJournalPanelModel { entries: &entries })
        .map(|_| ())
}

pub(super) fn write_approval_receipt<W: Write>(
    language: Language,
    request: &RuntimeApprovalRequest,
    title: &str,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = I18n::new(language);
    let foreground_shell_handoff = request.status == ApprovalRequestStatus::Approved
        && request_is_executable_bash_tool(request)
        && request.execution_path != Some("provider_native_shell_tool_execution");
    let provider_native_shell = request.status == ApprovalRequestStatus::Approved
        && request_is_executable_bash_tool(request)
        && request.execution_path == Some("provider_native_shell_tool_execution");
    let decision = approval_receipt_decision(
        &i18n,
        request,
        foreground_shell_handoff,
        provider_native_shell,
    );

    let message = if foreground_shell_handoff {
        foreground_handoff_message(&i18n, request)
    } else if provider_native_shell {
        i18n.t(MessageId::ApprovalReceiptProviderNativeAllowedMessage)
            .to_string()
    } else {
        String::new()
    };

    let kind = approval_receipt_kind(&i18n, request, foreground_shell_handoff);
    let subject = approval_receipt_subject(
        &i18n,
        request,
        foreground_shell_handoff,
        provider_native_shell,
    );

    RatatuiInlineRenderer::for_terminal()
        .with_language(language)
        .write_approval_receipt_panel(
            output,
            ApprovalReceiptPanelModel {
                title,
                negative: approval_receipt_is_negative(request.status),
                id: &request.id,
                kind,
                decision,
                subject,
                preview: &request.preview,
                message: &message,
            },
        )
        .map(|_| ())
}

/// Receipt message for a command handed to the foreground shell. Commands that
/// need a terminal of their own get one extra sentence so the user knows their
/// keystrokes now go to that program; ordinary agent forensics commands stay
/// noise-free.
fn foreground_handoff_message(i18n: &I18n, request: &RuntimeApprovalRequest) -> String {
    let sent = i18n.t(MessageId::ApprovalReceiptBashSentToShellMessage);
    if !handoff_runs_interactively(request) {
        return sent.to_string();
    }
    format!(
        "{sent} {}",
        i18n.t(MessageId::ApprovalReceiptForegroundInteractiveHint)
    )
}

fn handoff_runs_interactively(request: &RuntimeApprovalRequest) -> bool {
    let Ok(command) = shell_handoff_command_from_request(request) else {
        return false;
    };
    classify_command_interaction(&command).pty_requirement == PtyRequirement::Required
}

fn approval_receipt_is_negative(status: ApprovalRequestStatus) -> bool {
    matches!(
        status,
        ApprovalRequestStatus::Denied
            | ApprovalRequestStatus::Cancelled
            | ApprovalRequestStatus::Blocked
    )
}

fn approval_receipt_decision<'a>(
    i18n: &'a I18n,
    request: &RuntimeApprovalRequest,
    foreground_shell_handoff: bool,
    provider_native_shell: bool,
) -> &'a str {
    match request.status {
        ApprovalRequestStatus::Pending => i18n.t(MessageId::ApprovalReceiptDecisionPending),
        ApprovalRequestStatus::Approved => {
            if foreground_shell_handoff {
                i18n.t(MessageId::ApprovalReceiptDecisionSentToShell)
            } else if provider_native_shell {
                i18n.t(MessageId::ApprovalReceiptDecisionProviderNativeAllowed)
            } else if matches!(
                request.kind,
                ApprovalRequestKind::Tool | ApprovalRequestKind::TurnExtension
            ) {
                i18n.t(MessageId::ApprovalReceiptDecisionApproved)
            } else {
                i18n.t(MessageId::ApprovalReceiptDecisionApprovedDisplayOnly)
            }
        }
        ApprovalRequestStatus::Denied => i18n.t(MessageId::ApprovalReceiptDecisionDenied),
        ApprovalRequestStatus::Cancelled => i18n.t(MessageId::ApprovalReceiptDecisionCancelled),
        ApprovalRequestStatus::Blocked => i18n.t(MessageId::ApprovalReceiptDecisionBlocked),
    }
}

fn approval_receipt_kind<'a>(
    i18n: &'a I18n,
    request: &RuntimeApprovalRequest,
    foreground_shell_handoff: bool,
) -> &'a str {
    if foreground_shell_handoff {
        return i18n.t(MessageId::ApprovalReceiptKindBashTool);
    }
    match request.kind {
        ApprovalRequestKind::Tool => i18n.t(MessageId::ApprovalReceiptKindToolRequest),
        ApprovalRequestKind::ShellCommand => {
            i18n.t(MessageId::ApprovalReceiptKindShellCommandRequest)
        }
        ApprovalRequestKind::TurnExtension => i18n.t(MessageId::ApprovalReceiptKindTurnExtension),
    }
}

fn approval_receipt_subject<'a>(
    i18n: &'a I18n,
    request: &'a RuntimeApprovalRequest,
    foreground_shell_handoff: bool,
    provider_native_shell: bool,
) -> &'a str {
    if foreground_shell_handoff {
        i18n.t(MessageId::ApprovalReceiptSubjectBashSentToShell)
    } else if provider_native_shell {
        i18n.t(MessageId::ApprovalReceiptSubjectBashProviderNative)
    } else {
        &request.subject
    }
}

pub(crate) fn render_approval_details<W: Write>(
    language: Language,
    request: &RuntimeApprovalRequest,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = I18n::new(language);
    let preview_label = match request.kind {
        ApprovalRequestKind::Tool => i18n.t(MessageId::ApprovalToolInputLabel),
        ApprovalRequestKind::ShellCommand => i18n.t(MessageId::ApprovalCommandLabel),
        ApprovalRequestKind::TurnExtension => i18n.t(MessageId::ApprovalTurnExtensionLabel),
    };

    RatatuiInlineRenderer::for_terminal()
        .with_language(language)
        .write_approval_details_panel(
            output,
            ApprovalDetailsPanelModel {
                id: &request.id,
                run_id: &request.run_id,
                source: request.source,
                kind: request.kind.label(),
                status: request.status.label(),
                risk: request.risk,
                subject: &request.subject,
                preview_label,
                preview: &request.preview,
                request_id: request.request_id.as_deref(),
                tool_use_id: request.tool_use_id.as_deref(),
                execution_path: request.execution_path,
                command_block_id: request.command_block_id.as_deref(),
                redaction_status: request.redaction_status,
                assessment: request.assessment.as_ref().map(assessment_summary_model),
                audit_ref: request.audit_ref.as_deref(),
            },
        )
        .map(|_| ())
}

fn assessment_summary_model(
    assessment: &RuntimeCommandAssessmentSummary,
) -> CommandAssessmentSummaryModel<'_> {
    CommandAssessmentSummaryModel {
        impact: assessment.impact,
        execution: assessment.execution,
        confidence: assessment.confidence,
        primary_reason: assessment.primary_reason,
        reason_trace: &assessment.reason_trace,
        auto_allow: assessment.auto_allow,
        output_stability: assessment.output_stability,
        output_exposure: assessment.output_exposure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approved_bash_request(execution_path: Option<&'static str>) -> RuntimeApprovalRequest {
        RuntimeApprovalRequest {
            id: "req-zh".to_string(),
            audit_ref: None,
            run_id: "run-1".to_string(),
            origin: AgentRunOrigin::Standard,
            session_id: "sess-1".to_string(),
            cwd: "/tmp".to_string(),
            source: "control-protocol",
            provider_shell_request_kind: ProviderShellRequestKind::ControlPermission,
            kind: ApprovalRequestKind::Tool,
            subject: "Bash".to_string(),
            preview: "$ echo hi".to_string(),
            risk: "medium",
            request_id: Some("ctrl-1".to_string()),
            tool_use_id: Some("toolu-1".to_string()),
            tool_input: None,
            original_user_request: None,
            status: ApprovalRequestStatus::Approved,
            execution_path,
            command_block_id: None,
            redaction_status: None,
            assessment: None,
            hook_requires_approval: false,
            hook_warnings: Vec::new(),
        }
    }

    #[test]
    fn approval_receipt_shell_execution_messages_use_zh_catalog() {
        let foreground = approved_bash_request(None);
        let provider_native = approved_bash_request(Some("provider_native_shell_tool_execution"));
        let mut output = Vec::new();

        write_approval_receipt(Language::ZhCn, &foreground, "Approved", &mut output).unwrap();
        write_approval_receipt(Language::ZhCn, &provider_native, "Approved", &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Bash tool 已发送到 shell"), "{text}");
        assert!(
            text.contains("已允许 provider-native shell tool 执行"),
            "{text}"
        );
        let old_foreground_text = ["Bash tool", " sent to shell"].concat();
        let old_provider_native_text = ["Provider-native shell", " tool allowed"].concat();
        assert!(!text.contains(&old_foreground_text), "{text}");
        assert!(!text.contains(&old_provider_native_text), "{text}");
    }

    #[test]
    fn approval_receipt_never_emits_audit_reference() {
        let mut request = approved_bash_request(None);
        request.audit_ref = Some("audit-event-1".to_string());
        let mut output = Vec::new();

        write_approval_receipt(Language::ZhCn, &request, "已自动批准", &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("audit_ref"), "{text}");
        assert!(!text.contains("audit-event-1"), "{text}");
        assert!(text.contains("Bash tool 已发送到 shell"), "{text}");
    }

    #[test]
    fn approval_receipt_metadata_uses_zh_catalog() {
        let i18n = I18n::new(Language::ZhCn);
        let foreground = approved_bash_request(None);
        let provider_native = approved_bash_request(Some("provider_native_shell_tool_execution"));

        assert_eq!(
            approval_receipt_decision(&i18n, &foreground, true, false),
            "已发送到 shell"
        );
        assert_eq!(approval_receipt_kind(&i18n, &foreground, true), "Bash tool");
        assert_eq!(
            approval_receipt_subject(&i18n, &foreground, true, false),
            "Bash tool: 已发送到 shell"
        );
        assert_eq!(
            approval_receipt_decision(&i18n, &provider_native, false, true),
            "已允许 provider-native 执行"
        );
        assert_eq!(
            approval_receipt_subject(&i18n, &provider_native, false, true),
            "Bash tool: provider-native 执行"
        );
    }

    fn bash_request_with_command(command: &str) -> RuntimeApprovalRequest {
        let mut request = approved_bash_request(None);
        request.preview = format!("$ {command}");
        request.tool_input = Some(serde_json::json!({ "command": command }));
        request
    }

    #[test]
    fn agent_forensics_receipt_stays_free_of_interactive_noise() {
        for command in ["git log --oneline", "systemctl status nginx", "pwd"] {
            let request = bash_request_with_command(command);

            assert!(!handoff_runs_interactively(&request), "{command}");
            for language in [Language::EnUs, Language::ZhCn] {
                let message = foreground_handoff_message(&I18n::new(language), &request);
                assert!(!message.contains('q'), "{command} / {message}");
                assert!(!message.contains("分页器"), "{command} / {message}");
            }
        }
    }

    #[test]
    fn explicit_interactive_receipt_hints_before_the_handoff_in_both_languages() {
        for command in ["less README.md", "man ls", "top"] {
            let request = bash_request_with_command(command);
            assert!(handoff_runs_interactively(&request), "{command}");

            let en = foreground_handoff_message(&I18n::new(Language::EnUs), &request);
            assert!(en.starts_with("Bash tool sent to shell "), "{en}");
            assert!(
                en.contains("keyboard input goes directly to it")
                    && en.contains("Press q to leave a pager"),
                "{en}"
            );

            let zh = foreground_handoff_message(&I18n::new(Language::ZhCn), &request);
            assert!(zh.starts_with("Bash tool 已发送到 shell "), "{zh}");
            assert!(
                zh.contains("键盘输入会直接发送给它") && zh.contains("按 q 返回"),
                "{zh}"
            );
        }
    }

    #[test]
    fn interactive_receipt_never_exposes_the_pager_transport_prefix() {
        let request = bash_request_with_command("less README.md");
        let mut output = Vec::new();

        write_approval_receipt(Language::EnUs, &request, "Approved", &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("$ less README.md"), "{text}");
        assert!(!text.contains("PAGER=cat"), "{text}");
        assert!(!text.contains("GIT_PAGER"), "{text}");
    }
}
