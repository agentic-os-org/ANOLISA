use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::runtime::prelude::{
    AgentRunHandle, AgentRunOrigin, AgentRunPoll, CoshApprovalMode, CoshCoreAdapter,
    GovernanceDecision, GovernancePolicyDecision, GovernedEvent, Language, OutputRefs,
    RatatuiInlineRenderer,
};

use crate::adapter::serialize_host_executed_shell_result;
use crate::agent::run::ActiveAgentRun;

#[test]
fn interactive_shell_handoff_completion_keeps_run_origin() {
    let mut state = InlineState::default();
    assert!(state.control.record_provider_shell_command_from_tool_call(
        "run-1",
        "tool-1",
        r#"{"command":"sudo systemctl status sshd"}"#,
    ));
    state.control.record_provider_tool_output_delta(
        "run-1",
        "tool-1",
        "stderr",
        "sudo: a terminal is required\n",
    );
    state
        .control
        .queue_interactive_shell_handoff_for_tool_failure(
            "run-1",
            "tool-1",
            "error",
            AgentRunOrigin::InsightPrompt,
        )
        .expect("interactive handoff");
    let handoff = ShellHandoffRequest::new(
        "sudo systemctl status sshd",
        "$ sudo systemctl status sshd",
        "provider-tool-call",
        "agent",
        "handoff-1",
        "run-1",
        10,
    )
    .expect("shell handoff request");
    let block = CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "raw-session".to_string(),
        command: "sudo systemctl status sshd".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };

    let evidence = record_shell_handoff_completion(&mut state, &handoff, &block, "completed");

    assert_eq!(evidence.origin, AgentRunOrigin::InsightPrompt);
}

#[test]
fn host_executed_shell_result_uses_opaque_output_id_without_path() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-host-executed-result-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let output_ref = dir.join("cmd-1.txt");
    std::fs::write(&output_ref, "Filesystem\n/dev/disk1 10G 5G 5G\n").expect("write output ref");
    let output_ref_str = output_ref.to_str().expect("utf8 output ref");

    let command = "df -h --token cli-secret";
    let mut handoff = ShellHandoffRequest::new(
        command,
        "$ df -h --token cli-secret",
        "provider-tool-call",
        "agent",
        "req-1",
        "run-1",
        10,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-1".to_string());
    handoff.tool_use_id = Some("toolu-1".to_string());
    let block = CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "raw-session".to_string(),
        command: command.to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: Some(output_ref_str.to_string()),
            terminal_output_bytes: 32,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "completed",
        AgentRunOrigin::AutoFailure,
    );
    assert_eq!(evidence.origin, AgentRunOrigin::AutoFailure);
    let (_, continuation_origin) = shell_handoff_continuation_request(&evidence, None);
    assert_eq!(continuation_origin, AgentRunOrigin::AutoFailure);

    let result = host_executed_shell_result(&handoff, &evidence);

    assert!(
        result
            .llm_content
            .contains("output_id: terminal-output://raw-session/cmd-1"),
        "{}",
        result.llm_content
    );
    assert!(
        result.llm_content.contains("bounded_output_summary:"),
        "{}",
        result.llm_content
    );
    assert!(
        result.llm_content.contains("Filesystem"),
        "{}",
        result.llm_content
    );
    assert!(
        !result.llm_content.contains(output_ref_str),
        "{}",
        result.llm_content
    );
    assert_eq!(
        result.metadata.output_ref.as_deref(),
        Some("terminal-output://raw-session/cmd-1")
    );
    assert!(
        result.metadata.command.contains("--token <redacted>"),
        "{:?}",
        result.metadata.command
    );
    assert!(
        !result.metadata.command.contains("cli-secret"),
        "{:?}",
        result.metadata.command
    );
    assert!(
        !result.llm_content.contains("cli-secret"),
        "{}",
        result.llm_content
    );
    assert_eq!(result.metadata.tool_use_id.as_deref(), Some("toolu-1"));
    assert_eq!(result.return_display, None);
    let serialized = serialize_host_executed_shell_result("ctrl-1", &result);
    assert!(!serialized.contains("provider_visible_complete"));
    assert!(!serialized.contains("provider_visible_truncated"));

    let _ = std::fs::remove_dir_all(&dir);
}

// D-6 (#2142): the marker redacts a secret-bearing approved provider command
// to a placeholder, but the provider authored that command — its result
// metadata echoes the request's original text so the model can correlate
// result with call. Durable evidence keeps the redacted block text.
#[test]
fn provider_authored_handoff_result_metadata_keeps_original_command_text() {
    let command = "deploy --api-key sk-live-secret-value";
    let mut handoff = ShellHandoffRequest::new(
        command,
        "$ deploy --api-key sk-live-secret-value",
        "approved_provider_shell_tool",
        "agent",
        "req-1",
        "run-1",
        10,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-1".to_string());
    handoff.tool_use_id = Some("toolu-1".to_string());
    let block = CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "raw-session".to_string(),
        // The marker replaced the reported text; the token claim carried the
        // block anyway.
        command: "<redacted sensitive command>".to_string(),
        origin: crate::types::CommandOrigin::ProviderTool,
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 1,
        status: CommandStatus::Failed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: Some(crate::types::ShellCommandAuditIdentity {
            run_id: "run-1".to_string(),
            request_id: Some("ctrl-1".to_string()),
            tool_use_id: Some("toolu-1".to_string()),
            handoff_token: Some(handoff.token.clone()),
        }),
    };

    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "failed",
        AgentRunOrigin::Standard,
    );

    // Durable face: the redacted placeholder, never the original.
    assert_eq!(evidence.command, "<redacted sensitive command>");
    // Provider face: the request's original text, bounded but not redacted.
    assert_eq!(evidence.provider_command, command);

    let result = host_executed_shell_result(&handoff, &evidence);
    assert_eq!(result.metadata.command, command);
    // The provider summary body keeps the durable (redacted) text.
    assert!(
        !result.llm_content.contains("sk-live-secret-value"),
        "{}",
        result.llm_content
    );
}

// A user-authored handoff source keeps full provider-face redaction even
// when the block text survived unredacted.
#[test]
fn user_sourced_handoff_result_metadata_stays_redacted() {
    let command = "df -h --token cli-secret";
    let handoff = ShellHandoffRequest::new(
        command,
        "$ df -h --token cli-secret",
        "send_to_shell",
        "user",
        "req-1",
        "run-1",
        10,
    )
    .expect("handoff");
    let block = CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "raw-session".to_string(),
        command: command.to_string(),
        origin: crate::types::CommandOrigin::UserSendToShell,
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };

    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "completed",
        AgentRunOrigin::Standard,
    );

    assert!(
        evidence.provider_command.contains("--token <redacted>"),
        "{:?}",
        evidence.provider_command
    );
    assert!(
        !evidence.provider_command.contains("cli-secret"),
        "{:?}",
        evidence.provider_command
    );
}

#[test]
fn host_executed_shell_result_budget_does_not_duplicate_preview() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-host-executed-budget-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let output_ref = dir.join("cmd-budget.txt");
    const PROVIDER_PREVIEW_MAX_CHARS: usize = 6_000;
    let preview = "a".repeat(PROVIDER_PREVIEW_MAX_CHARS);
    std::fs::write(&output_ref, &preview).expect("write output ref");
    let output_ref_str = output_ref.to_str().expect("utf8 output ref");

    let mut handoff = ShellHandoffRequest::new(
        "printf budget",
        "$ printf budget",
        "provider-tool-call",
        "agent",
        "req-budget",
        "run-budget",
        10,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-budget".to_string());
    handoff.tool_use_id = Some("toolu-budget".to_string());
    let block = CommandBlock {
        id: "cmd-budget".to_string(),
        session_id: "raw-session".to_string(),
        command: "printf budget".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: Some(output_ref_str.to_string()),
            terminal_output_bytes: preview.len() as u64,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "completed",
        AgentRunOrigin::Standard,
    );
    let result = host_executed_shell_result(&handoff, &evidence);

    assert_eq!(result.return_display, None);
    let serialized = serialize_host_executed_shell_result("ctrl-budget", &result);
    assert!(!serialized.contains("provider_visible_complete"));
    assert!(!serialized.contains("provider_visible_truncated"));
    assert!(!serialized.contains("provider_visible_chars"));
    let preview_count = serialized.matches(&preview).count();
    assert_eq!(preview_count, 1, "{serialized}");
    assert!(
        serialized.len() <= 8_500,
        "serialized_len={} must stay bounded",
        serialized.len()
    );

    let baseline = serialized.replace(&preview, "");
    assert!(
        serialized.len() <= baseline.len() + 6_500,
        "serialized_len={} baseline_len={}",
        serialized.len(),
        baseline.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_executed_shell_result_escaped_preview_stays_within_delta_budget() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-host-executed-escaped-budget-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let output_ref = dir.join("cmd-escaped.txt");
    let mut preview = "a".repeat(5_990);
    preview.push_str("\"\\\nend");
    std::fs::write(&output_ref, &preview).expect("write output ref");
    let output_ref_str = output_ref.to_str().expect("utf8 output ref");

    let mut handoff = ShellHandoffRequest::new(
        "printf escaped",
        "$ printf escaped",
        "provider-tool-call",
        "agent",
        "req-escaped",
        "run-escaped",
        10,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-escaped".to_string());
    handoff.tool_use_id = Some("toolu-escaped".to_string());
    let block = CommandBlock {
        id: "cmd-escaped".to_string(),
        session_id: "raw-session".to_string(),
        command: "printf escaped".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: Some(output_ref_str.to_string()),
            terminal_output_bytes: preview.len() as u64,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "completed",
        AgentRunOrigin::Standard,
    );
    let result = host_executed_shell_result(&handoff, &evidence);

    let serialized = serialize_host_executed_shell_result("ctrl-escaped", &result);
    assert_eq!(result.return_display, None);
    assert!(!serialized.contains("provider_visible_complete"));
    assert!(!serialized.contains("provider_visible_truncated"));
    assert!(!serialized.contains("provider_visible_chars"));
    let mut baseline_result = result.clone();
    baseline_result.llm_content = baseline_result.llm_content.replace(&preview, "");
    let baseline = serialize_host_executed_shell_result("ctrl-escaped", &baseline_result);

    assert!(
        serialized.len() <= baseline.len() + 6_500,
        "serialized_len={} baseline_len={}",
        serialized.len(),
        baseline.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_executed_delivery_channel_closed_records_recovery_and_releases_claim() {
    let request = test_request();
    let handle = closed_cosh_core_control_handle(&request);
    assert!(
        handle
            .control_capabilities()
            .can_handle_host_executed_shell_tool_result,
        "mock provider must advertise host-executed support before exiting"
    );

    let mut state = InlineState::default();
    state.agent_run.active = Some(test_active_run(request, handle));
    let mut handoff = ShellHandoffRequest::new(
        "df -h",
        "$ df -h",
        "provider-tool-call",
        "agent",
        "req-1",
        "run-1",
        10,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-closed".to_string());
    handoff.tool_use_id = Some("toolu-closed".to_string());
    let block = CommandBlock {
        id: "cmd-closed".to_string(),
        session_id: "raw-session".to_string(),
        command: "df -h".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 32,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "completed",
        AgentRunOrigin::Standard,
    );

    let delivery = deliver_host_executed_shell_result_if_supported(&mut state, &handoff, &evidence);

    assert!(!delivery.delivered);
    assert_eq!(delivery.status, "provider_channel_closed");
    assert!(
        delivery
            .recovery_reason
            .unwrap_or_default()
            .contains("approval channel closed"),
        "{delivery:?}"
    );
    assert!(
        state
            .control
            .provider_tool_mut()
            .claim_host_executed_shell_result("run-1", "ctrl-closed", Some("toolu-closed"))
            .is_some(),
        "failed delivery must release duplicate guard claim"
    );
}

#[test]
fn host_executed_delivery_does_not_use_an_unrelated_active_run() {
    let mut request = test_request();
    request.id = "unrelated-run".to_string();
    let handle = closed_cosh_core_control_handle(&request);
    let mut state = InlineState::default();
    state.agent_run.active = Some(test_active_run(request, handle));
    let mut handoff = ShellHandoffRequest::new(
        "df -h",
        "$ df -h",
        "provider-tool-call",
        "agent",
        "req-owner",
        "owner-run",
        10,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-owner".to_string());
    handoff.tool_use_id = Some("toolu-owner".to_string());
    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &CommandBlock {
            id: "cmd-owner".to_string(),
            session_id: "raw-session".to_string(),
            command: "df -h".to_string(),
            origin: Default::default(),
            cwd: "/repo".to_string(),
            end_cwd: "/repo".to_string(),
            started_at_ms: 10,
            ended_at_ms: 20,
            duration_ms: 10,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        "completed",
        AgentRunOrigin::Standard,
    );

    let delivery = deliver_host_executed_shell_result_if_supported(&mut state, &handoff, &evidence);

    assert!(!delivery.delivered);
    assert_eq!(delivery.status, "provider_run_not_owner");
    assert!(state
        .control
        .provider_tool_mut()
        .claim_host_executed_shell_result("owner-run", "ctrl-owner", Some("toolu-owner"))
        .is_some());
}

#[test]
fn host_executed_delivery_refreshes_active_run_idle_clock() {
    let request = test_request();
    let (dir, handle) = open_cosh_core_control_handle(&request);

    let mut state = InlineState::default();
    state.agent_run.active = Some(test_active_run(request, handle));
    state
        .agent_run
        .active
        .as_mut()
        .expect("active run")
        .last_activity_at = Instant::now() - Duration::from_secs(60);
    let mut handoff = ShellHandoffRequest::new(
        "df -h",
        "$ df -h",
        "provider-tool-call",
        "agent",
        "req-1",
        "run-1",
        10,
    )
    .expect("handoff");
    handoff.request_id = Some("ctrl-open".to_string());
    handoff.tool_use_id = Some("toolu-open".to_string());
    let block = CommandBlock {
        id: "cmd-open".to_string(),
        session_id: "raw-session".to_string(),
        command: "df -h".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 20_000,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 32,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let evidence = RuntimeShellCommandCompleted::from_shell_handoff(
        &handoff,
        &block,
        "completed",
        AgentRunOrigin::Standard,
    );

    let delivery = deliver_host_executed_shell_result_if_supported(&mut state, &handoff, &evidence);

    let refreshed = state
        .agent_run
        .active
        .as_ref()
        .expect("active run")
        .last_activity_at;
    assert!(delivery.delivered, "{delivery:?}");
    assert!(
        refreshed.elapsed() < Duration::from_secs(2),
        "host-executed delivery should reset provider idle clock; elapsed={:?}",
        refreshed.elapsed()
    );

    let _ = std::fs::remove_dir_all(dir);
}

fn open_cosh_core_control_handle(request: &AgentRequest) -> (PathBuf, AgentRunHandle) {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-open-control-{}-{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let program = dir.join("cosh-core-open-control.sh");
    std::fs::write(
            &program,
            r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","model":"mock-cosh-core","session_id":"mock-open-control"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"ctrl-open","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"df -h"},"tool_use_id":"toolu-open"}}'
if IFS= read -r response; then
  case "$response" in
    *'"behavior":"host_executed_shell"'*'df -h'*)
      printf '%s\n' '{"type":"assistant","session_id":"mock-open-control","message":{"content":[{"type":"text","text":"host executed accepted"}]}}'
      printf '%s\n' '{"type":"result","subtype":"success","session_id":"mock-open-control","is_error":false,"result":"done"}'
      exit 0
      ;;
  esac
fi
printf '%s\n' '{"type":"result","subtype":"error","session_id":"mock-open-control","is_error":true,"result":"missing host executed response"}'
exit 1
"#,
        )
        .expect("write mock cosh-core");
    let mut permissions = std::fs::metadata(&program)
        .expect("mock metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&program, permissions).expect("chmod mock cosh-core");
    let adapter = CoshCoreAdapter::new(program.to_string_lossy().to_string(), true);
    let handle = adapter.start_cancellable(request.clone(), CoshApprovalMode::Auto);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_request = false;
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(AgentEvent::ToolPermissionRequest { .. })) => {
                saw_request = true;
                break;
            }
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => continue,
            Ok(AgentRunPoll::Finished) => break,
            Err(err) => panic!("mock cosh-core control run failed: {err:?}"),
        }
    }
    assert!(saw_request, "mock provider did not emit tool permission");
    assert!(
        handle
            .control_capabilities()
            .can_handle_host_executed_shell_tool_result,
        "mock provider must advertise host-executed support"
    );
    (dir, handle)
}

fn closed_cosh_core_control_handle(request: &AgentRequest) -> AgentRunHandle {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let adapter = CoshCoreAdapter::new(
        manifest_dir
            .join("tests")
            .join("fixtures")
            .join("provider")
            .join("qwen")
            .join("mock_qwen_control_capabilities.sh")
            .to_string_lossy()
            .to_string(),
        true,
    );
    let handle = adapter.start_cancellable(request.clone(), CoshApprovalMode::Auto);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(AgentEvent::AgentCompleted { .. })) => break,
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => continue,
            Ok(AgentRunPoll::Finished) => break,
            Err(err) => panic!("mock cosh-core control run failed: {err:?}"),
        }
    }
    std::thread::sleep(Duration::from_millis(200));
    handle
}

#[test]
fn discarding_pre_result_text_only_drops_the_owning_run() {
    let request = test_request();
    let handle = closed_cosh_core_control_handle(&request);
    let mut active_run = test_active_run(request, handle);
    active_run.held_events.push(held_text("run-1", "stale"));
    let mut state = InlineState::default();
    state.agent_run.active = Some(active_run);
    state.agent_run.held_events = vec![
        held_text("run-1", "stale after the run ended"),
        held_text("other-run", "unrelated text"),
    ];

    discard_text_held_before_shell_result(&mut state, "run-1");

    assert!(state
        .agent_run
        .active
        .as_ref()
        .expect("active run")
        .held_events
        .is_empty());
    let remaining = state
        .agent_run
        .held_events
        .iter()
        .map(|event| event.display_text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(remaining, ["unrelated text"]);
}

fn held_text(run_id: &str, text: &str) -> GovernedEvent {
    GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::TextDelta {
            run_id: run_id.to_string(),
            text: text.to_string(),
        },
        reason: "held".to_string(),
        display_text: text.to_string(),
        auto_execute: false,
    }
}

fn test_active_run(request: AgentRequest, handle: AgentRunHandle) -> ActiveAgentRun {
    let renderer = RatatuiInlineRenderer::for_terminal();
    ActiveAgentRun {
        request,
        origin: AgentRunOrigin::Standard,
        handle,
        provider_name: "cosh-core",
        language: Language::EnUs,
        renderer: renderer.clone(),
        status_animation: renderer.status_animation(),
        markdown_stream: renderer.stream_markdown_agent(),
        governed_events: Vec::new(),
        deferred_events: Vec::new(),
        held_events: Vec::new(),
        cosh_request_filter: crate::evidence::stream::CoshRequestStreamFilter::default(),
        pending_cosh_requests: Vec::new(),
        pending_cosh_request_audits: Vec::new(),
        rendered_governed_event_count: 0,
        selectable_after_event_index: None,
        started_at: Instant::now(),
        last_activity_at: Instant::now(),
        last_heartbeat_at: Instant::now(),
        current_phase: String::new(),
        current_message: String::new(),
        has_visible_text_delta: false,
        completed: false,
        host_completed_tool_ids: Vec::new(),
        pending_hook_notifications: Vec::new(),
    }
}

fn test_request() -> AgentRequest {
    AgentRequest {
        id: "run-1".to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-request".to_string(),
            session_id: "session-1".to_string(),
            command: "df -h".to_string(),
            origin: Default::default(),
            cwd: "/repo".to_string(),
            end_cwd: "/repo".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("check disk".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}
