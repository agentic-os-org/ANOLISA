use crate::runtime::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovedBashExecutionPath {
    ForegroundShellPty,
    Blocked,
}

pub(super) fn fallback_bash_execution_path(command: &str) -> ApprovedBashExecutionPath {
    if build_shell_handoff_request(ShellHandoffBuildInput {
        command,
        exact_preview: format!("$ {command}"),
        source: "validation",
        actor: "policy",
        approval_id: "validation".to_string(),
        run_id: "validation".to_string(),
        request_id: None,
        tool_use_id: None,
    })
    .is_err()
    {
        ApprovedBashExecutionPath::Blocked
    } else {
        ApprovedBashExecutionPath::ForegroundShellPty
    }
}

pub(super) fn raw_bash_command(preview: &str) -> &str {
    preview.strip_prefix("$ ").unwrap_or(preview).trim()
}

pub(crate) fn queue_approved_shell_handoff(
    state: &mut InlineState,
    request: &RuntimeApprovalRequest,
) {
    if request.execution_path == Some("provider_native_shell_tool_execution") {
        return;
    }
    let Ok(command) = shell_handoff_command_from_request(request) else {
        return;
    };
    let exact_preview = shell_handoff_exact_preview(request, &command);
    let request_id = request
        .provider_shell_request_kind
        .is_control_permission()
        .then(|| request.request_id.clone())
        .flatten();
    let Ok(handoff_request) = build_shell_handoff_request(ShellHandoffBuildInput {
        command: &command,
        exact_preview,
        source: approved_shell_handoff_source(state, request),
        actor: "user",
        approval_id: request.id.clone(),
        run_id: request.run_id.clone(),
        request_id,
        tool_use_id: request.tool_use_id.clone(),
    }) else {
        return;
    };
    state
        .control
        .mark_provider_shell_handoff_run(&request.run_id);
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(handoff_request);
}

fn approved_shell_handoff_source(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> &'static str {
    if state
        .agent_run
        .active
        .as_ref()
        .is_some_and(|run| run.request.id == request.run_id && run.request.hook_finding.is_some())
    {
        return "user_analysis_action";
    }
    if request.source == "provider-tool-call" {
        return "approved_provider_shell_tool";
    }
    match request.provider_shell_request_kind {
        ProviderShellRequestKind::ControlPermission => "approved_provider_shell_tool",
        ProviderShellRequestKind::StreamedToolCallFallback
        | ProviderShellRequestKind::LocalApproval => "approved_fallback",
    }
}

pub(crate) fn queue_interactive_shell_handoff<W: Write>(
    state: &mut InlineState,
    handoff_id: &str,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(handoff) = state.control.find_interactive_shell_handoff(handoff_id) else {
        let i18n = state.i18n();
        RatatuiInlineRenderer::for_terminal().write_notice_panel(
            output,
            NoticePanelModel {
                title: i18n.t(MessageId::ApprovalShellHandoffNotFoundTitle),
                body: vec![i18n.format(
                    MessageId::ApprovalShellHandoffNotFoundBody,
                    &[("id", handoff_id)],
                )],
                footer: None,
            },
        )?;
        return Ok(());
    };

    let handoff_request = match build_shell_handoff_request(ShellHandoffBuildInput {
        command: &handoff.command,
        exact_preview: handoff.exact_preview.clone(),
        source: "send_to_shell",
        actor: "user",
        approval_id: handoff.id.clone(),
        run_id: handoff.run_id.clone(),
        request_id: None,
        tool_use_id: Some(handoff.tool_id.clone()),
    }) {
        Ok(request) => request,
        Err(message) => {
            let i18n = state.i18n();
            let body = approval_shell_handoff_validation_message(&i18n, &message);
            RatatuiInlineRenderer::for_terminal().write_notice_panel(
                output,
                NoticePanelModel {
                    title: i18n.t(MessageId::ApprovalShellHandoffBlockedTitle),
                    body: vec![body],
                    footer: Some(i18n.t(MessageId::ApprovalShellHandoffBlockedFooter)),
                },
            )?;
            return Ok(());
        }
    };

    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(handoff_request);
    let i18n = state.i18n();
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title: i18n.t(MessageId::ApprovalShellHandoffSendingTitle),
            body: vec![
                i18n.format(
                    MessageId::ApprovalShellHandoffSendingBody,
                    &[("id", handoff.id.as_str())],
                ),
                handoff.exact_preview,
            ],
            footer: None,
        },
    )?;
    Ok(())
}

pub(crate) fn approval_shell_handoff_validation_message(i18n: &I18n, message: &str) -> String {
    let id = match message {
        "empty shell handoff command" => {
            Some(MessageId::ApprovalShellHandoffValidationEmptyCommand)
        }
        "shell handoff command contains an unsupported line break" => {
            Some(MessageId::ApprovalShellHandoffValidationMultilineCommand)
        }
        "shell handoff command contains blocked control character" => {
            Some(MessageId::ApprovalShellHandoffValidationControlCharacter)
        }
        "shell handoff preview is empty" => {
            Some(MessageId::ApprovalShellHandoffValidationEmptyPreview)
        }
        "shell handoff approval id is empty" => {
            Some(MessageId::ApprovalShellHandoffValidationEmptyApprovalId)
        }
        "shell handoff run id is empty" => {
            Some(MessageId::ApprovalShellHandoffValidationEmptyRunId)
        }
        _ => None,
    };
    id.map(|id| i18n.t(id).to_string())
        .unwrap_or_else(|| message.to_string())
}

pub(crate) fn shell_handoff_command_from_request(
    request: &RuntimeApprovalRequest,
) -> Result<String, String> {
    if request.request_id.is_none() {
        return Ok(raw_bash_command(&request.preview).to_string());
    }

    let input = request
        .tool_input
        .as_ref()
        .ok_or_else(|| "provider shell tool input is missing".to_string())?;
    if let Some(command) = input.get("command").and_then(|value| value.as_str()) {
        return Ok(command.to_string());
    }
    if let Some(command) = input.get("cmd").and_then(|value| value.as_str()) {
        return Ok(command.to_string());
    }
    if let Some(command) = input.as_str() {
        return Ok(command.to_string());
    }
    Err("provider shell tool input has no command field".to_string())
}

fn shell_handoff_exact_preview(request: &RuntimeApprovalRequest, command: &str) -> String {
    let expected = format!("$ {command}");
    if request.preview == expected {
        request.preview.clone()
    } else {
        expected
    }
}

pub(super) struct ShellHandoffBuildInput<'a> {
    pub(super) command: &'a str,
    pub(super) exact_preview: String,
    pub(super) source: &'static str,
    pub(super) actor: &'static str,
    pub(super) approval_id: String,
    pub(super) run_id: String,
    pub(super) request_id: Option<String>,
    pub(super) tool_use_id: Option<String>,
}

pub(super) fn build_shell_handoff_request(
    input: ShellHandoffBuildInput<'_>,
) -> Result<ShellHandoffRequest, String> {
    let mut request = ShellHandoffRequest::new(
        input.command.to_string(),
        input.exact_preview,
        input.source,
        input.actor,
        input.approval_id,
        input.run_id,
        now_ms(),
    )?;
    request.request_id = input.request_id;
    request.tool_use_id = input.tool_use_id;
    // Every handoff carries an agent-proposed command, so the pager decision is
    // made here rather than at the transport: user-typed commands never build a
    // handoff request and keep their own pager configuration untouched.
    // A compound line receives one policy from its aggregated interaction
    // profile; the marker applies it at preexec and restores it at precmd.
    request.implicit_pager_policy = agent_implicit_pager_policy(input.command);
    request.validate()?;
    Ok(request)
}

pub(crate) fn trust_key_from_command(command: &str) -> Option<String> {
    let cmd = raw_bash_command(command);
    if cmd.trim().is_empty() || cmd.contains('\0') {
        return None;
    }
    Some(cmd.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub(crate) fn command_matches_trust_key(command: &str, trusted: &HashSet<String>) -> bool {
    trust_key_from_command(command).is_some_and(|key| trusted.contains(&key))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
