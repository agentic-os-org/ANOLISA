use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

pub(super) fn render_debug_command<W: Write>(
    sub: Option<&str>,
    adapter: &AdapterInstance,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    match sub {
        Some("session") | None => {
            let i18n = state.i18n();
            let debug_line = |id, value: String| i18n.format(id, &[("value", value.as_str())]);
            let mut body = vec![
                debug_line(MessageId::DebugAdapterLine, adapter.name().to_string()),
                debug_line(
                    MessageId::DebugProviderInvocationLine,
                    adapter
                        .provider_invocation()
                        .unwrap_or_else(|| "<none>".to_string()),
                ),
                debug_line(
                    MessageId::DebugProviderCommittedSessionLine,
                    adapter
                        .committed_session_id()
                        .unwrap_or_else(|| "<none>".to_string()),
                ),
                debug_line(
                    MessageId::DebugActiveRunLine,
                    state.agent_run.active.is_some().to_string(),
                ),
                debug_line(
                    MessageId::DebugQueuedRunsLine,
                    state.agent_run.queued_requests.len().to_string(),
                ),
            ];
            if let Some(active_run) = state.agent_run.active.as_ref() {
                let capabilities = active_run.handle.control_capabilities();
                body.push(debug_line(
                    MessageId::DebugProviderPendingSessionLine,
                    active_run
                        .handle
                        .pending_provider_session_id()
                        .unwrap_or_else(|| "<none>".to_string()),
                ));
                body.push(debug_line(
                    MessageId::DebugProviderInitializeSeenLine,
                    capabilities.provider_initialize_seen.to_string(),
                ));
                body.push(debug_line(
                    MessageId::DebugHostExecutedShellResultLine,
                    capabilities
                        .can_handle_host_executed_shell_tool_result
                        .to_string(),
                ));
                body.push(debug_line(
                    MessageId::DebugSelectedShellExecutionPathLine,
                    if capabilities.can_handle_host_executed_shell_tool_result {
                        "control_protocol_host_executed_shell_result"
                    } else if adapter.capabilities().control_protocol {
                        "provider_native_shell_tool_execution"
                    } else {
                        "unsupported"
                    }
                    .to_string(),
                ));
            } else {
                let latest_shell = state.evidence.latest_shell_command_completed();
                body.push(debug_line(
                    MessageId::DebugProviderPendingSessionLine,
                    "<none>".to_string(),
                ));
                body.push(debug_line(
                    MessageId::DebugProviderInitializeSeenLine,
                    "<none>".to_string(),
                ));
                body.push(debug_line(
                    MessageId::DebugHostExecutedShellResultLine,
                    latest_shell
                        .map(|evidence| evidence.provider_result_delivery_status.to_string())
                        .unwrap_or_else(|| "<none>".to_string()),
                ));
                body.push(debug_line(
                    MessageId::DebugSelectedShellExecutionPathLine,
                    latest_shell
                        .map(|evidence| evidence.selected_execution_path().to_string())
                        .unwrap_or_else(|| "<none>".to_string()),
                ));
            }
            let latest_shell = state.evidence.latest_shell_command_completed();
            body.push(debug_line(
                MessageId::DebugLatestProviderRequestLine,
                latest_shell
                    .and_then(|evidence| evidence.provider_request_id.as_deref())
                    .unwrap_or("<none>")
                    .to_string(),
            ));
            body.push(debug_line(
                MessageId::DebugLatestToolUseLine,
                latest_shell
                    .and_then(|evidence| evidence.tool_use_id.as_deref())
                    .unwrap_or("<none>")
                    .to_string(),
            ));
            if let Some(evidence) = state.evidence.latest_recovery() {
                body.push(debug_line(
                    MessageId::DebugLatestRecoveryStatusLine,
                    evidence.provider_result_delivery_status.to_string(),
                ));
                body.push(debug_line(
                    MessageId::DebugLatestRecoveryReasonLine,
                    evidence.recovery_reason.unwrap_or("<none>").to_string(),
                ));
            } else {
                body.push(debug_line(
                    MessageId::DebugLatestRecoveryStatusLine,
                    "<none>".to_string(),
                ));
                body.push(debug_line(
                    MessageId::DebugLatestRecoveryReasonLine,
                    "<none>".to_string(),
                ));
            }
            body.push(debug_line(
                MessageId::DebugEvidenceAccessLine,
                evidence_access_mode(adapter, state).to_string(),
            ));
            body.push(debug_line(
                MessageId::DebugEvidenceToolRegisteredLine,
                evidence_tool_registered(adapter, state).to_string(),
            ));
            body.push(debug_line(
                MessageId::DebugEvidenceNamespaceLine,
                evidence_namespaces(state),
            ));
            body.push(debug_line(
                MessageId::DebugEvidenceLedgerCountLine,
                state.session_blocks.len().to_string(),
            ));
            body.push(debug_line(
                MessageId::DebugLatestShellOutputReadLine,
                latest_shell_evidence_action(state),
            ));
            body.extend(continuity_debug_lines(state));
            render_notice_panel(output, i18n.t(MessageId::DebugSessionTitle), body, None)
        }
        Some(other) => {
            let i18n = state.i18n();
            render_notice_panel(
                output,
                i18n.t(MessageId::DebugSessionTitle),
                vec![i18n.format(MessageId::DebugUnknownTargetBody, &[("target", other)])],
                Some(i18n.t(MessageId::DebugUnknownTargetFooter)),
            )
        }
    }
}

fn evidence_access_mode(adapter: &AdapterInstance, state: &InlineState) -> &'static str {
    if evidence_tool_registered(adapter, state) {
        "control_protocol_tool"
    } else {
        "fenced_request_fallback"
    }
}

fn evidence_tool_registered(adapter: &AdapterInstance, state: &InlineState) -> bool {
    state
        .agent_run
        .active
        .as_ref()
        .map(|active| {
            active
                .handle
                .control_capabilities()
                .can_handle_shell_evidence_tool
        })
        .unwrap_or_else(|| adapter.name() == "cosh-core" && adapter.capabilities().control_protocol)
}

fn evidence_namespaces(state: &InlineState) -> String {
    let mut namespaces = state
        .session_blocks
        .iter()
        .map(|block| block.session_id.as_str())
        .collect::<Vec<_>>();
    namespaces.sort_unstable();
    namespaces.dedup();
    if namespaces.is_empty() {
        "<none>".to_string()
    } else {
        namespaces.join(",")
    }
}

fn latest_shell_evidence_action(state: &InlineState) -> String {
    let Some(action) = state.shell_evidence.last_action.as_ref() else {
        return "<none>".to_string();
    };
    format!(
        "mode={} request_id={} tool_use_id={} action={} output_id={} status={} reason={}",
        action.mode,
        action.request_id,
        action.tool_use_id,
        action.action,
        action.output_id.as_deref().unwrap_or("<none>"),
        action.status,
        action.failure_reason.as_deref().unwrap_or("<none>")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zh_state() -> InlineState {
        InlineState {
            language: Language::ZhCn,
            ..InlineState::default()
        }
    }

    #[test]
    fn debug_session_uses_zh_catalog_labels() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let state = zh_state();
        let mut output = Vec::new();

        render_debug_command(Some("session"), &adapter, &state, &mut output)
            .expect("render debug session");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("会话调试"), "{output}");
        assert!(output.contains("适配器: fake"), "{output}");
        assert!(output.contains("provider 已提交会话: <none>"), "{output}");
        assert!(output.contains("活跃运行: false"), "{output}");
        assert!(output.contains("已选择 shell 执行路径: <none>"), "{output}");
        assert!(!output.contains("Session debug"), "{output}");
        assert!(!output.contains("provider committed session"), "{output}");
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn debug_session_shows_shell_evidence_state() {
        let adapter = AdapterInstance::CoshCore(CoshCoreAdapter::default());
        let mut state = InlineState::default();
        state.session_blocks = vec![CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "raw-session-123".to_string(),
            command: "echo test".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 0,
            ended_at_ms: 1,
            duration_ms: 1,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: Some("/tmp/out.txt".to_string()),
                terminal_output_bytes: 4,
            },
            shell_environment_generation: None,
            audit_identity: None,
        }];
        state.shell_evidence.last_action = Some(crate::runtime::state::ShellEvidenceActionRecord {
            mode: "control_protocol_tool",
            request_id: "read-1".to_string(),
            tool_use_id: "toolu-1".to_string(),
            action: "read_output".to_string(),
            output_id: Some("terminal-output://raw-session-123/cmd-1".to_string()),
            status: "available".to_string(),
            failure_reason: None,
        });
        let mut output = Vec::new();

        render_debug_command(Some("session"), &adapter, &state, &mut output)
            .expect("render debug session");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(
            output.contains("evidence access: control_protocol_tool"),
            "{output}"
        );
        assert!(
            output.contains("evidence tool registered: true"),
            "{output}"
        );
        assert!(
            output.contains("current evidence namespace: raw-session-123"),
            "{output}"
        );
        assert!(output.contains("evidence ledger commands: 1"), "{output}");
        assert!(
            output.contains(
                "latest shell evidence action: mode=control_protocol_tool request_id=read-1"
            ),
            "{output}"
        );
        assert!(output.contains("action=read_output"), "{output}");
        assert!(output.contains("tool_use_id=toolu-1"), "{output}");
        assert!(
            output.contains("output_id=terminal-output://raw-session-123/cmd-1"),
            "{output}"
        );
        assert!(output.contains("status=available"), "{output}");
        assert!(output.contains("reason=<none>"), "{output}");
    }

    #[test]
    fn debug_unknown_target_uses_zh_catalog_notice() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let state = zh_state();
        let mut output = Vec::new();

        render_debug_command(Some("bad"), &adapter, &state, &mut output)
            .expect("render debug unknown target");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("未知 debug 目标: bad"), "{output}");
        assert!(output.contains("使用 /debug session。"), "{output}");
        assert!(!output.contains("Unknown debug target"), "{output}");
    }
}
