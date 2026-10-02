use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::cosh_core::{CoshCoreAdapter, SessionRecoveryState, SessionRuntimeState};
use super::{
    AdapterError, AgentAdapter, AgentRunHandle, AgentRunPoll, ApprovalDecision, ApprovalResponse,
    FreshSessionOutcome,
};
use crate::types::{
    AgentEvent, AgentMode, AgentRequest, CommandBlock, CommandStatus, CoshApprovalMode, OutputRefs,
};

fn test_workspace_scope() -> String {
    std::fs::canonicalize(std::env::temp_dir())
        .expect("canonical test workspace")
        .to_string_lossy()
        .into_owned()
}

fn test_request() -> AgentRequest {
    let workspace_scope = test_workspace_scope();
    AgentRequest {
        id: "test".to_string(),
        session_id: "sess".to_string(),
        command_block: CommandBlock {
            id: "blk".to_string(),
            session_id: "sess".to_string(),
            command: "echo test".to_string(),
            origin: Default::default(),
            cwd: workspace_scope.clone(),
            end_cwd: workspace_scope,
            started_at_ms: 0,
            ended_at_ms: 0,
            duration_ms: 0,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: vec![],
        context_hints: vec![],
        user_input: Some("test".to_string()),
        findings: vec![],
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

fn test_adapter() -> CoshCoreAdapter {
    CoshCoreAdapter::new("cosh-core", false)
}

fn write_mock_core(label: &str, script: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("cosh-core-{label}-{}.sh", std::process::id()));
    std::fs::write(&path, script).expect("write mock cosh-core");
    let mut permissions = std::fs::metadata(&path)
        .expect("mock cosh-core metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod mock cosh-core");
    path
}

fn adapter_with_active_session(program: &std::path::Path) -> CoshCoreAdapter {
    let adapter = CoshCoreAdapter::new(program.to_string_lossy().into_owned(), true);
    *adapter.session.lock().unwrap() = SessionRuntimeState::with_active(
        "00000000-0000-4000-8000-000000000000",
        test_workspace_scope(),
    );
    adapter
}

fn adapter_with_selected_session(program: &std::path::Path) -> CoshCoreAdapter {
    let adapter = adapter_with_active_session(program);
    if let Ok(mut session) = adapter.session.lock() {
        session.recovery.state = SessionRecoveryState::Selected;
        session.recovery.selected_session_id =
            Some("11111111-1111-4111-8111-111111111111".to_string());
        session.recovery.selected_workspace_scope = Some(test_workspace_scope());
    }
    adapter
}

fn assert_failed_selection_was_released(adapter: &CoshCoreAdapter) {
    let recovery = adapter.recovery_snapshot();
    assert_eq!(recovery.state, SessionRecoveryState::Failed);
    assert_eq!(recovery.selected_session_id, None);
    assert_eq!(recovery.selected_workspace_scope, None);
    assert_eq!(
        adapter.protected_session_ids(),
        vec!["00000000-0000-4000-8000-000000000000"]
    );
}

fn collect_cancellable_run(handle: &AgentRunHandle) -> Vec<AgentEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut events = Vec::new();
    loop {
        match handle
            .poll_event_timeout(Duration::from_millis(100))
            .expect("poll persistent cosh-core run")
        {
            AgentRunPoll::Event(event) => events.push(event),
            AgentRunPoll::Finished => return events,
            AgentRunPoll::Timeout if Instant::now() < deadline => {}
            AgentRunPoll::Timeout => panic!("persistent cosh-core run timed out"),
        }
    }
}

fn persistent_mock_paths(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "cosh-core-persistent-{name}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create persistent mock directory");
    (root.join("mock-cosh-core.sh"), root)
}

fn write_persistent_mock(
    script: &std::path::Path,
    gate: &std::path::Path,
    started: &std::path::Path,
) {
    let source = r#"#!/bin/sh
session_id=00000000-0000-4000-8000-000000000000
resume_next=0
for arg in "$@"; do
  if [ "$arg" = "--registry" ]; then
    IFS= read -r line || exit 1
    request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
    printf '{"type":"registry_response","request_id":"%s","success":true,"data":{"transport":"short"}}\n' "$request_id"
    exit 0
  fi
  if [ "$resume_next" -eq 1 ]; then
    session_id=$arg
    resume_next=0
  elif [ "$arg" = "--resume" ]; then
    resume_next=1
  fi
done

turns=0
reloads=0
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}\n' "$request_id"
      ;;
    *'"type":"user"'*)
      turns=$((turns + 1))
      if [ "$turns" -eq 1 ] && [ ! -f "__GATE__" ]; then
        : > "__STARTED__"
        while [ ! -f "__GATE__" ]; do sleep 0.02; done
      fi
      printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"result":"done"}\n' "$session_id"
      ;;
    *'"type":"registry_request"'*)
      request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      case "$line" in
        *'"action":"reload"'*) reloads=$((reloads + 1));;
      esac
      printf '{"type":"registry_response","request_id":"%s","success":true,"data":{"pid":%s,"turns":%s,"reloads":%s,"transport":"live"}}\n' "$request_id" "$$" "$turns" "$reloads"
      ;;
    *'"subtype":"shutdown"'*) exit 0;;
  esac
done
"#
    .replace("__GATE__", &gate.to_string_lossy())
    .replace("__STARTED__", &started.to_string_lossy());
    std::fs::write(script, source).expect("write persistent cosh-core mock");
    let mut permissions = std::fs::metadata(script)
        .expect("persistent cosh-core mock metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(script, permissions).expect("chmod persistent cosh-core mock");
}

#[test]
fn fresh_session_detaches_active_and_selected_without_protecting_them() {
    let active = "00000000-0000-4000-8000-000000000000".to_string();
    let selected = "11111111-1111-4111-8111-111111111111".to_string();
    let mut session = SessionRuntimeState::with_active(active.clone(), "/tmp");
    session.recovery.state = SessionRecoveryState::Selected;
    session.recovery.selected_session_id = Some(selected);
    session.recovery.selected_workspace_scope = Some("/tmp".to_string());
    let adapter = CoshCoreAdapter {
        program: "unused".to_string(),
        allow_model_call: false,
        session: Arc::new(Mutex::new(session)),
        ..CoshCoreAdapter::default()
    };

    assert_eq!(
        adapter.start_fresh_session(),
        FreshSessionOutcome::Detached {
            previous_session_id: Some(active),
        }
    );
    assert_eq!(adapter.committed_session_id(), None);
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::None
    );
    assert!(adapter.protected_session_ids().is_empty());

    assert_eq!(
        adapter.start_fresh_session(),
        FreshSessionOutcome::Detached {
            previous_session_id: None,
        }
    );
}

#[test]
fn prepare_invocation_headless_flag() {
    let inv = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Auto);
    assert_eq!(inv.program, "cosh-core");
    assert!(inv.args.contains(&"--headless".to_string()));
    assert!(inv
        .args
        .contains(&"--enable-shell-evidence-tool".to_string()));
    assert!(inv.args.contains(&"--cosh-shell-transport".to_string()));
}

#[test]
fn agent_request_does_not_serialize_internal_context_binding() {
    let request = test_request();

    let json = serde_json::to_string(&request).expect("serialize request");

    assert!(!json.contains("context_binding"), "{json}");
}

#[test]
fn prepare_invocation_approval_modes() {
    let recommend = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Recommend);
    assert!(recommend.args.contains(&"recommend".to_string()));

    let auto = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Auto);
    assert!(auto.args.contains(&"auto".to_string()));

    let trust = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Trust);
    assert!(trust.args.contains(&"trust".to_string()));
}

#[test]
fn shell_handoff_continuation_keeps_recommend_args_without_recommend_claim() {
    let mut request = test_request();
    request.context_hints = vec![
        crate::types::SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
        format!("{}auto", crate::types::USER_APPROVAL_MODE_HINT_PREFIX),
    ];
    let inv = test_adapter().prepare_invocation(&request, CoshApprovalMode::Recommend);

    assert!(inv.args.contains(&"recommend".to_string()));
    assert!(!inv.prompt.contains("recommend mode"), "{}", inv.prompt);
    assert!(
        inv.prompt
            .contains("approval mode is auto and has not changed"),
        "{}",
        inv.prompt
    );
    assert!(
        inv.prompt.contains("Do not emit tool calls in this turn"),
        "{}",
        inv.prompt
    );
}

#[test]
fn prepare_invocation_prompt_includes_cosh_shell_contract() {
    let inv = test_adapter().prepare_invocation(&test_request(), CoshApprovalMode::Auto);

    assert!(inv
        .prompt
        .contains("Handle this natural-language shell prompt request"));
    assert!(inv.prompt.contains("cosh-shell Agent contract"));
    assert!(inv
        .prompt
        .contains("Always emit a provider permission request"));
    assert!(inv.prompt.contains("State the diagnostic conclusion first"));
    assert!(inv
        .prompt
        .contains("at most one primary recommendation command"));
}

// cosh-core recovers the raw user input from this envelope for session
// summaries, so the anchors it matches on must keep their exact spelling and
// order. Changing them silently degrades `/session list` previews.
#[test]
fn prepare_invocation_prompt_keeps_session_summary_anchors() {
    const PREFIX: &str =
        "Handle this natural-language shell prompt request for a Shell-first assistant.\n";
    const INPUT_MARKER: &str = "\nuser_input: ";
    const RUNTIME_MARKER: &str = "\n\nruntime_frame:\n";
    const CONTRACT_MARKER: &str = "\n\ncosh-shell Agent contract:\n";
    let mut request = test_request();
    request.user_input = Some("查看当前目录下的文件".to_string());

    let prompt = test_adapter()
        .prepare_invocation(&request, CoshApprovalMode::Auto)
        .prompt;

    assert!(prompt.starts_with(PREFIX), "{prompt}");
    let input_start = prompt.find(INPUT_MARKER).expect("input marker") + INPUT_MARKER.len();
    let runtime_start = prompt.find(RUNTIME_MARKER).expect("runtime marker");
    let contract_start = prompt.rfind(CONTRACT_MARKER).expect("contract marker");
    assert!(input_start < runtime_start, "{prompt}");
    assert!(runtime_start < contract_start, "{prompt}");
    assert_eq!(
        prompt[input_start..runtime_start].trim(),
        "查看当前目录下的文件"
    );
}

#[test]
fn prepare_invocation_prompt_preserves_user_provided_secret() {
    let secret = "api_key=sk-cosh-shell-runtime-secret";
    let mut request = test_request();
    request.user_input = Some(format!("write this exact value: {secret}"));

    let inv = test_adapter().prepare_invocation(&request, CoshApprovalMode::Auto);

    assert!(inv.prompt.contains(secret), "{}", inv.prompt);
    assert!(!inv.prompt.contains("<redacted>"), "{}", inv.prompt);
}

#[test]
fn prepare_invocation_prompt_uses_shell_output_tool_mode() {
    let mut request = test_request();
    let mut context = request.command_block.clone();
    context.id = "cmd-1".to_string();
    context.session_id = "session-1".to_string();
    context.exit_code = 0;
    context.status = CommandStatus::Completed;
    context.output.terminal_output_ref = Some("/tmp/cosh-output.txt".to_string());
    context.output.terminal_output_bytes = 42;
    request.context_blocks = vec![context];

    let inv = test_adapter().prepare_invocation(&request, CoshApprovalMode::Auto);

    assert!(inv.prompt.contains("cosh_shell_evidence"), "{}", inv.prompt);
    assert!(
        inv.prompt.contains("action=list_commands"),
        "{}",
        inv.prompt
    );
    assert!(inv.prompt.contains("action=read_output"), "{}", inv.prompt);
    assert!(
        inv.prompt.contains("Use current tool results first"),
        "{}",
        inv.prompt
    );
    assert!(
        inv.prompt
            .contains("Use read_output only for older shell ledger output"),
        "{}",
        inv.prompt
    );
    assert!(
        inv.prompt.contains("activity recaps or command lists"),
        "{}",
        inv.prompt
    );
    assert!(
        inv.prompt.contains("output_available=false"),
        "{}",
        inv.prompt
    );
    assert!(inv.prompt.contains("output_bytes=0"), "{}", inv.prompt);
    assert!(
        inv.prompt
            .contains("call cosh_shell_evidence with action=list_commands"),
        "{}",
        inv.prompt
    );
    assert!(!inv.prompt.contains("```cosh-request"), "{}", inv.prompt);
    assert!(
        !inv.prompt.contains("```cosh-request\noutput"),
        "{}",
        inv.prompt
    );
}

#[test]
fn prepare_invocation_prompt_suppresses_shell_output_requests_in_recommend_mode() {
    let mut request = test_request();
    let mut context = request.command_block.clone();
    context.id = "cmd-1".to_string();
    context.session_id = "session-1".to_string();
    context.output.terminal_output_ref = Some("/tmp/cosh-output.txt".to_string());
    context.output.terminal_output_bytes = 42;
    request.context_blocks = vec![context];

    let inv = test_adapter().prepare_invocation(&request, CoshApprovalMode::Recommend);

    assert!(
        inv.prompt
            .contains("do not request shell output automatically"),
        "{}",
        inv.prompt
    );
    assert!(
        !inv.prompt.contains("cosh_shell_evidence"),
        "{}",
        inv.prompt
    );
    assert!(!inv.prompt.contains("```cosh-request"), "{}", inv.prompt);
}

#[test]
fn prepare_invocation_session_resume() {
    let adapter = CoshCoreAdapter::new("cosh-core", false);
    *adapter.session.lock().unwrap() =
        SessionRuntimeState::with_active("prev-sess", test_workspace_scope());
    let inv = adapter.prepare_invocation(&test_request(), CoshApprovalMode::Auto);
    assert!(inv.args.contains(&"--resume".to_string()));
    assert!(inv.args.contains(&"prev-sess".to_string()));
}

#[test]
fn prepare_invocation_does_not_resume_across_cwd_scope() {
    let adapter = CoshCoreAdapter::new("cosh-core", false);
    *adapter.session.lock().unwrap() = SessionRuntimeState::with_active("prev-sess", "/other");
    let inv = adapter.prepare_invocation(&test_request(), CoshApprovalMode::Auto);
    assert!(!inv.args.contains(&"--resume".to_string()));
    assert!(!inv.args.contains(&"prev-sess".to_string()));
}

// The same-session retry fallback (T2) carries no
// "disable provider resume" hint, so it must resume the active session;
// the final fresh safety net (T3) carries the hint and must not.
#[test]
fn prepare_invocation_same_session_retry_fallback_resumes_active_session() {
    let adapter = CoshCoreAdapter::new("cosh-core", false);
    *adapter.session.lock().unwrap() =
        SessionRuntimeState::with_active("prev-sess", test_workspace_scope());
    let mut request = test_request();
    request.context_hints = vec![
        "analysis-only continuation after foreground shell handoff".to_string(),
        "shell handoff recovery owner: req-1/<none>/toolu-1".to_string(),
        "same-session retry for shell handoff fallback".to_string(),
    ];
    let inv = adapter.prepare_invocation(&request, CoshApprovalMode::Auto);
    assert!(inv.args.contains(&"--resume".to_string()), "{:?}", inv.args);
    assert!(
        inv.args.contains(&"prev-sess".to_string()),
        "{:?}",
        inv.args
    );
}

#[test]
fn prepare_invocation_fresh_fallback_disables_resume() {
    let adapter = CoshCoreAdapter::new("cosh-core", false);
    *adapter.session.lock().unwrap() =
        SessionRuntimeState::with_active("prev-sess", test_workspace_scope());
    let mut request = test_request();
    request.context_hints = vec![
        "analysis-only continuation after foreground shell handoff".to_string(),
        "shell handoff recovery owner: req-1/<none>/toolu-1".to_string(),
        "same-session retry for shell handoff fallback".to_string(),
        "disable provider resume for shell handoff fallback".to_string(),
    ];
    let inv = adapter.prepare_invocation(&request, CoshApprovalMode::Auto);
    assert!(
        !inv.args.contains(&"--resume".to_string()),
        "{:?}",
        inv.args
    );
    assert!(
        !inv.args.contains(&"prev-sess".to_string()),
        "{:?}",
        inv.args
    );
}

#[test]
fn prepare_invocation_ignores_failed_selected_session() {
    let adapter = test_adapter();
    if let Ok(mut session) = adapter.session.lock() {
        session.recovery.state = SessionRecoveryState::Failed;
        session.recovery.selected_session_id =
            Some("11111111-1111-4111-8111-111111111111".to_string());
        session.recovery.selected_workspace_scope = Some(test_workspace_scope());
    }

    let invocation = adapter.prepare_invocation(&test_request(), CoshApprovalMode::Auto);

    assert!(!invocation.args.contains(&"--resume".to_string()));
}

#[test]
fn prepare_invocation_uses_process_cwd_for_unknown_intercept_scope() {
    let mut request = test_request();
    request.command_block.cwd = "<unknown>".to_string();
    request.command_block.end_cwd = "<unknown>".to_string();

    let invocation = test_adapter().prepare_invocation(&request, CoshApprovalMode::Recommend);
    let workspace_index = invocation
        .args
        .iter()
        .position(|argument| argument == "--workspace")
        .expect("workspace argument");
    let expected = std::fs::canonicalize(std::env::current_dir().expect("current dir"))
        .expect("canonical current dir")
        .to_string_lossy()
        .into_owned();

    assert_eq!(invocation.args[workspace_index + 1], expected);
}

#[test]
fn capabilities_match_expected() {
    let adapter = test_adapter();
    let caps = adapter.capabilities();
    assert!(caps.text_stream);
    assert!(caps.session_resume);
    assert!(caps.tool_intent);
    assert!(caps.user_question);
    assert!(caps.cancellable);
    assert!(caps.control_protocol);
}

#[test]
fn list_sessions_returns_one_page_and_preserves_opaque_cursor() {
    let script =
        std::env::temp_dir().join(format!("cosh-core-session-pages-{}.sh", std::process::id()));
    std::fs::write(
        &script,
        r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'"cursor":null'*)
    printf '%s\n' '{"ok":true,"data":{"action":"list","sessions":[{"session_id":"00000000-0000-4000-8000-000000000000","workspace_scope":"/tmp","created_at_ms":1,"updated_at_ms":3,"model":"mock","message_count":1,"first_prompt":"first","schema_version":1,"health":"ready"}],"next_cursor":"cursor-1"}}'
    ;;
  *'"cursor":"cursor-1"'*)
    printf '%s\n' '{"ok":true,"data":{"action":"list","sessions":[{"session_id":"11111111-1111-4111-8111-111111111111","workspace_scope":"/tmp","created_at_ms":1,"updated_at_ms":2,"model":"mock","message_count":1,"first_prompt":"second","schema_version":1,"health":"ready"}],"next_cursor":"cursor-2"}}'
    ;;
  *'"cursor":"cursor-2"'*)
    printf '%s\n' '{"ok":true,"data":{"action":"list","sessions":[{"session_id":"22222222-2222-4222-8222-222222222222","workspace_scope":"/tmp","created_at_ms":1,"updated_at_ms":1,"model":"mock","message_count":1,"first_prompt":"third","schema_version":1,"health":"ready"}],"next_cursor":null}}'
    ;;
esac
"#,
    )
    .expect("write paginated session mock");
    let mut permissions = std::fs::metadata(&script)
        .expect("paginated session mock metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod paginated session mock");
    let adapter = CoshCoreAdapter {
        program: script.to_string_lossy().into_owned(),
        ..test_adapter()
    };

    let first = adapter
        .list_sessions("/tmp", false)
        .expect("first session page");
    let second = adapter
        .list_sessions_page("/tmp", 100, first.next_cursor.as_deref(), false)
        .expect("second session page");
    let third = adapter
        .list_sessions_page("/tmp", 100, second.next_cursor.as_deref(), false)
        .expect("third session page");
    let _ = std::fs::remove_file(&script);

    assert_eq!(
        first
            .sessions
            .iter()
            .chain(&second.sessions)
            .chain(&third.sessions)
            .map(|summary| summary.first_prompt.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("first"), Some("second"), Some("third")]
    );
    assert_eq!(first.next_cursor.as_deref(), Some("cursor-1"));
    assert_eq!(second.next_cursor.as_deref(), Some("cursor-2"));
    assert!(third.next_cursor.is_none());
}

#[test]
fn stream_parser_uses_neutral_status_messages() {
    let script =
        std::env::temp_dir().join(format!("cosh-tui-neutral-status-{}.sh", std::process::id()));
    std::fs::write(
        &script,
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hidden reasoning"}}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"s","is_error":false,"result":"done"}'
"#,
    )
    .expect("write mock cosh-tui");
    let mut permissions = std::fs::metadata(&script)
        .expect("mock cosh-tui metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod mock cosh-tui");

    let adapter = CoshCoreAdapter::new(script.to_string_lossy().to_string(), true);
    let mut events = Vec::new();
    let result = adapter.run_stream(&test_request(), &mut |event| {
        events.push(event);
        Ok(())
    });
    let _ = std::fs::remove_file(&script);
    result.expect("run mock cosh-tui");

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StatusChanged { phase, message, .. }
            if phase == "thinking" && message == "thinking"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentCompleted { summary, .. } if summary == "analysis completed"
    )));
    let debug = format!("{events:?}");
    assert!(!debug.contains("claude"), "{debug}");
    assert!(!debug.contains("co thinking"), "{debug}");
}

#[test]
fn selected_session_transitions_through_restoring_to_active() {
    let workspace_scope = test_workspace_scope();
    let script = std::env::temp_dir().join(format!(
        "cosh-core-session-recovery-{}.sh",
        std::process::id()
    ));
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--session-control" ]; then
  cat >/dev/null
  printf '%s\n' '{{"ok":true,"data":{{"action":"validate","session":{{"session_id":"00000000-0000-4000-8000-000000000000","workspace_scope":"{workspace_scope}","created_at_ms":1,"updated_at_ms":2,"model":"mock","message_count":2,"first_prompt":"remember","schema_version":1,"health":"ready"}}}}}}'
  exit 0
fi
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      printf '{{"type":"control_response","response":{{"subtype":"success","request_id":"%s","response":{{"subtype":"initialize","protocol_version":1,"capabilities":{{}}}}}}}}\n' "$request_id"
      ;;
    *'"type":"user"'*)
      printf '%s\n' '{{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"duration_ms":1,"result":"done"}}'
      ;;
    *'"subtype":"shutdown"'*) exit 0;;
  esac
done
"#
        ),
    )
    .expect("write session recovery mock");
    let mut permissions = std::fs::metadata(&script)
        .expect("session recovery mock metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod session recovery mock");

    let adapter = CoshCoreAdapter::new(script.to_string_lossy().into_owned(), true);
    let selected = adapter
        .select_session(&workspace_scope, "00000000-0000-4000-8000-000000000000")
        .expect("select persisted session");
    assert_eq!(selected.session_id, "00000000-0000-4000-8000-000000000000");
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Selected
    );

    let mut request = test_request();
    request.command_block.cwd.clone_from(&workspace_scope);
    request.command_block.end_cwd.clone_from(&workspace_scope);
    let handle = adapter.start_cancellable(request, CoshApprovalMode::Recommend);
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Restoring
    );
    while let AgentRunPoll::Event(_) | AgentRunPoll::Timeout = handle
        .poll_event_timeout(std::time::Duration::from_secs(2))
        .expect("poll session recovery")
    {}
    let _ = std::fs::remove_file(&script);

    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Active
    );
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
}

#[test]
fn active_and_selected_sessions_are_protected_only_while_selection_is_live() {
    let adapter = CoshCoreAdapter {
        session: Arc::new(Mutex::new(SessionRuntimeState::with_active(
            "00000000-0000-4000-8000-000000000000",
            "/tmp",
        ))),
        ..test_adapter()
    };
    if let Ok(mut session) = adapter.session.lock() {
        session.recovery.state = SessionRecoveryState::Selected;
        session.recovery.selected_session_id =
            Some("11111111-1111-4111-8111-111111111111".to_string());
        session.recovery.selected_workspace_scope = Some(test_workspace_scope());
    }

    assert_eq!(
        adapter.protected_session_ids(),
        vec![
            "00000000-0000-4000-8000-000000000000",
            "11111111-1111-4111-8111-111111111111"
        ]
    );

    if let Ok(mut session) = adapter.session.lock() {
        session.recovery.state = SessionRecoveryState::Failed;
    }
    assert_eq!(
        adapter.protected_session_ids(),
        vec!["00000000-0000-4000-8000-000000000000"]
    );
}

#[test]
fn failed_session_selection_clears_previous_selection() {
    let script = write_mock_core(
        "selection-failure",
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"ok":false,"error":{"code":"not_found","message":"session disappeared","recoverable":true,"hint":"Refresh and retry."}}'
"#,
    );
    let adapter = CoshCoreAdapter {
        program: script.to_string_lossy().into_owned(),
        ..test_adapter()
    };
    if let Ok(mut session) = adapter.session.lock() {
        session.recovery.state = SessionRecoveryState::Selected;
        session.recovery.selected_session_id =
            Some("00000000-0000-4000-8000-000000000000".to_string());
        session.recovery.selected_workspace_scope = Some(test_workspace_scope());
    }

    let result = adapter.select_session("/tmp", "11111111-1111-4111-8111-111111111111");
    let _ = std::fs::remove_file(&script);

    assert!(result.is_err());
    let recovery = adapter.recovery_snapshot();
    assert_eq!(recovery.state, SessionRecoveryState::Failed);
    assert_eq!(recovery.selected_session_id, None);
    assert_eq!(recovery.selected_workspace_scope, None);
}

#[test]
fn synchronous_status_sink_error_releases_restoring_selection() {
    let program = std::path::Path::new("/unused/cosh-core");
    let adapter = adapter_with_selected_session(program);

    let result = adapter.run_stream(&test_request(), &mut |_| {
        Err(AdapterError {
            message: "status sink failed".to_string(),
        })
    });

    assert_eq!(
        result.expect_err("status sink failure").message,
        "status sink failed"
    );
    assert_failed_selection_was_released(&adapter);
}

#[test]
fn synchronous_spawn_error_releases_restoring_selection() {
    let missing = std::env::temp_dir().join(format!(
        "missing-cosh-core-{}-{}",
        std::process::id(),
        "spawn"
    ));
    let adapter = adapter_with_selected_session(&missing);

    let result = adapter.run_stream(&test_request(), &mut |_| Ok(()));

    assert!(result
        .expect_err("spawn failure")
        .message
        .contains("failed to run cosh-core"));
    assert_failed_selection_was_released(&adapter);
}

#[test]
fn synchronous_stream_read_error_releases_restoring_selection() {
    let script = write_mock_core(
        "invalid-utf8-stream",
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}'
printf '\377\n'
"#,
    );
    let adapter = adapter_with_selected_session(&script);

    let result = adapter.run_stream(&test_request(), &mut |_| Ok(()));

    let error = result.expect_err("stream read failure");
    assert!(
        error.message.contains("failed to read cosh-core stream"),
        "{}",
        error.message
    );
    assert_failed_selection_was_released(&adapter);
    let _ = std::fs::remove_file(&script);
}

#[test]
fn non_resumable_error_result_discards_active_session() {
    let script = write_mock_core(
        "non-resumable-error",
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":false,"model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"result":"failed"}'
"#,
    );
    let adapter = adapter_with_active_session(&script);

    adapter
        .run_stream(&test_request(), &mut |_| Ok(()))
        .expect("run non-resumable error result");
    let _ = std::fs::remove_file(&script);

    assert_eq!(adapter.committed_session_id(), None);
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::None
    );
}

#[test]
fn non_resumable_nonzero_exit_discards_active_session() {
    let script = write_mock_core(
        "non-resumable-nonzero",
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":false,"model":"mock","tools":[]}'
printf '%s\n' 'provider failed' >&2
exit 7
"#,
    );
    let adapter = adapter_with_active_session(&script);

    adapter
        .run_stream(&test_request(), &mut |_| Ok(()))
        .expect("run non-resumable nonzero exit");
    let _ = std::fs::remove_file(&script);

    assert_eq!(adapter.committed_session_id(), None);
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::None
    );
}

#[test]
fn cancellable_runs_and_registry_share_one_persistent_core() {
    let (script, root) = persistent_mock_paths("shared");
    let gate = root.join("gate");
    let started = root.join("started");
    std::fs::write(&gate, "ready").expect("open persistent mock gate");
    write_persistent_mock(&script, &gate, &started);
    let adapter = CoshCoreAdapter::new(script.to_string_lossy(), true);

    let first =
        collect_cancellable_run(&adapter.start_cancellable(test_request(), CoshApprovalMode::Auto));
    assert!(first
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    let first_info = adapter
        .registry_query("extensions", "info", serde_json::Value::Null)
        .expect("query first live registry state");

    let mut second_request = test_request();
    second_request.id = "test-2".to_string();
    let second =
        collect_cancellable_run(&adapter.start_cancellable(second_request, CoshApprovalMode::Auto));
    assert!(second
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    let second_info = adapter
        .registry_query("extensions", "info", serde_json::Value::Null)
        .expect("query second live registry state");

    assert_eq!(first_info["transport"], "live");
    assert_eq!(second_info["transport"], "live");
    assert_eq!(first_info["pid"], second_info["pid"]);
    assert_eq!(first_info["turns"], 1);
    assert_eq!(second_info["turns"], 2);
    drop(adapter);
    let _ = std::fs::remove_dir_all(root);
}

// #1940 regression: the persistent process announces `initialize` capabilities
// once, on the first turn. Later turns must inherit them from the process
// record — otherwise the receipt gate reads the default "not capable" and
// silently stops emitting `approval_receipt` from the second turn on, leaving
// the core's residual approval timeout armed against legitimate pending cards.
#[test]
fn persistent_core_keeps_receipt_capability_across_turns() {
    let (script, root) = persistent_mock_paths("receipt-capability");
    let receipts = root.join("receipts");
    let source = r#"#!/bin/sh
turns=0
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_approval_receipt":true}}}}'
      ;;
    *'"type":"approval_receipt"'*)
      printf '%s\n' "$line" >> "__RECEIPTS__"
      ;;
    *'"type":"user"'*)
      turns=$((turns + 1))
      printf '{"type":"control_request","request_id":"ctrl-%s","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"path":"/tmp/receipt-capability"},"tool_use_id":"toolu-%s"}}\n' "$turns" "$turns"
      ;;
    *'"behavior":"allow"'*)
      printf '{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"result":"done"}\n'
      ;;
    *'"subtype":"shutdown"'*) exit 0;;
  esac
done
"#
    .replace("__RECEIPTS__", &receipts.to_string_lossy());
    std::fs::write(&script, source).expect("write receipt capability mock");
    let mut permissions = std::fs::metadata(&script)
        .expect("receipt capability mock metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod receipt capability mock");
    let adapter = CoshCoreAdapter::new(script.to_string_lossy(), true);

    for turn in 1..=2u32 {
        let mut request = test_request();
        request.id = format!("test-{turn}");
        let handle = adapter.start_cancellable(request, CoshApprovalMode::Auto);
        let deadline = Instant::now() + Duration::from_secs(10);
        let request_id = loop {
            match handle
                .poll_event_timeout(Duration::from_millis(100))
                .expect("poll receipt capability run")
            {
                AgentRunPoll::Event(AgentEvent::ToolPermissionRequest { request_id, .. }) => {
                    break request_id;
                }
                AgentRunPoll::Event(_) => {}
                AgentRunPoll::Finished => {
                    panic!("turn {turn} finished before its permission request")
                }
                AgentRunPoll::Timeout if Instant::now() < deadline => {}
                AgentRunPoll::Timeout => panic!("no permission request on turn {turn}"),
            }
        };
        assert_eq!(request_id, format!("ctrl-{turn}"));
        handle
            .send_approval_receipt(&request_id)
            .expect("send approval receipt");
        handle
            .respond_approval(ApprovalResponse {
                request_id: request_id.clone(),
                tool_use_id: None,
                tool_input: None,
                decision: ApprovalDecision::Allow,
            })
            .expect("respond approval");
        collect_cancellable_run(&handle);
        let receipt_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let logged = std::fs::read_to_string(&receipts).unwrap_or_default();
            if logged.contains(&format!("\"request_id\":\"ctrl-{turn}\"")) {
                break;
            }
            assert!(
                Instant::now() < receipt_deadline,
                "turn {turn} receipt never reached the provider (logged: {logged:?})"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    drop(adapter);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn selecting_another_session_restarts_the_persistent_core() {
    let workspace = std::fs::canonicalize("/tmp")
        .expect("canonical temporary directory")
        .to_string_lossy()
        .into_owned();
    let (script, root) = persistent_mock_paths("session-switch");
    let gate = root.join("gate");
    let started = root.join("started");
    std::fs::write(&gate, "ready").expect("open persistent mock gate");
    write_persistent_mock(&script, &gate, &started);
    let adapter = CoshCoreAdapter::new(script.to_string_lossy(), true);

    let first =
        collect_cancellable_run(&adapter.start_cancellable(test_request(), CoshApprovalMode::Auto));
    assert!(first
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    let first_info = adapter
        .registry_query("extensions", "info", serde_json::Value::Null)
        .expect("query first live registry state");

    if let Ok(mut session) = adapter.session.lock() {
        session.recovery.state = SessionRecoveryState::Selected;
        session.recovery.selected_session_id =
            Some("11111111-1111-4111-8111-111111111111".to_string());
        session.recovery.selected_workspace_scope = Some(workspace.clone());
    }
    let mut second_request = test_request();
    second_request.id = "test-selected".to_string();
    second_request.command_block.cwd.clone_from(&workspace);
    second_request.command_block.end_cwd.clone_from(&workspace);
    let second =
        collect_cancellable_run(&adapter.start_cancellable(second_request, CoshApprovalMode::Auto));
    assert!(second
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    let second_info = adapter
        .registry_query("extensions", "info", serde_json::Value::Null)
        .expect("query selected session runtime");

    assert_ne!(first_info["pid"], second_info["pid"]);
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Active
    );
    drop(adapter);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn busy_external_mutation_reloads_live_core_at_safe_point() {
    let (script, root) = persistent_mock_paths("deferred-reload");
    let gate = root.join("gate");
    let started = root.join("started");
    write_persistent_mock(&script, &gate, &started);
    let adapter = CoshCoreAdapter::new(script.to_string_lossy(), true);

    let run = adapter.start_cancellable(test_request(), CoshApprovalMode::Auto);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        started.exists(),
        "persistent mock did not enter the busy turn"
    );

    let mutation = adapter
        .registry_query("extensions", "enable", serde_json::json!({"id": "demo"}))
        .expect("fall back to short registry mutation while live core is busy");
    assert_eq!(mutation["transport"], "short");
    std::fs::write(&gate, "continue").expect("release persistent mock turn");
    let events = collect_cancellable_run(&run);
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));

    let live = adapter
        .registry_query("extensions", "info", serde_json::Value::Null)
        .expect("query live registry after deferred reload");
    assert_eq!(live["transport"], "live");
    assert_eq!(live["reloads"], 1);
    drop(adapter);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn mcp_mutation_reloads_live_core_at_safe_point() {
    let (script, root) = persistent_mock_paths("mcp-deferred-reload");
    let gate = root.join("gate");
    let started = root.join("started");
    write_persistent_mock(&script, &gate, &started);
    let adapter = CoshCoreAdapter::new(script.to_string_lossy(), true);

    let run = adapter.start_cancellable(test_request(), CoshApprovalMode::Auto);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        started.exists(),
        "persistent mock did not enter the busy turn"
    );

    adapter.note_mcp_mutation();
    std::fs::write(&gate, "continue").expect("release persistent mock turn");
    let events = collect_cancellable_run(&run);
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));

    let live = adapter
        .registry_query("extensions", "info", serde_json::Value::Null)
        .expect("query live registry after deferred mcp reload");
    assert_eq!(live["transport"], "live");
    assert_eq!(live["reloads"], 1);
    drop(adapter);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn idle_mcp_mutation_reloads_before_the_next_turn_starts() {
    let (script, root) = persistent_mock_paths("mcp-idle-reload");
    // Embeds the reload counter into every turn result so the assertion can
    // prove ordering: the reload must be consumed before the next user
    // message reaches the mock, not merely at the end of that turn.
    let source = r#"#!/bin/sh
turns=0
reloads=0
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}\n' "$request_id"
      ;;
    *'"type":"user"'*)
      turns=$((turns + 1))
      printf '{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"result":"done-r%s"}\n' "$reloads"
      ;;
    *'"type":"registry_request"'*)
      request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      case "$line" in
        *'"action":"reload"'*) reloads=$((reloads + 1));;
      esac
      printf '{"type":"registry_response","request_id":"%s","success":true,"data":{"turns":%s,"reloads":%s,"transport":"live"}}\n' "$request_id" "$turns" "$reloads"
      ;;
    *'"subtype":"shutdown"'*) exit 0;;
  esac
done
"#;
    std::fs::write(&script, source).expect("write idle-reload cosh-core mock");
    let mut permissions = std::fs::metadata(&script)
        .expect("idle-reload cosh-core mock metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("chmod idle-reload cosh-core mock");
    let adapter = CoshCoreAdapter::new(script.to_string_lossy(), true);

    let first =
        collect_cancellable_run(&adapter.start_cancellable(test_request(), CoshApprovalMode::Auto));
    assert!(
        first.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text, .. } if text == "done-r0"
        )),
        "{first:?}"
    );

    adapter.note_mcp_mutation();

    let mut second_request = test_request();
    second_request.id = "test-2".to_string();
    let second =
        collect_cancellable_run(&adapter.start_cancellable(second_request, CoshApprovalMode::Auto));
    assert!(
        second.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { text, .. } if text == "done-r1"
        )),
        "reload must land before the next turn's user message: {second:?}"
    );
    drop(adapter);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that `registry_query_short` forwards `--workspace` to the
/// short-lived `cosh-core --registry` subprocess using the correct
/// priority: `shell_cwd` -> active session scope -> omit `--workspace`.
/// Also pins the last-known-value retention when `set_shell_cwd(None)`
/// is called after a valid cwd.
#[test]
fn registry_query_short_workspace_priority() {
    let script = write_mock_core(
        "workspace-priority",
        r#"#!/bin/sh
workspace=""
while [ $# -gt 0 ]; do
  case "$1" in
    --workspace) shift; workspace="${1:-}";;
  esac
  shift
done
IFS= read -r line || exit 1
request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"registry_response","request_id":"%s","success":true,"data":{"workspace":"%s"}}\n' "$request_id" "$workspace"
exit 0
"#,
    );

    // 1. shell_cwd wins over session scope.
    {
        let adapter = CoshCoreAdapter::new(script.to_string_lossy().into_owned(), false);
        adapter.set_shell_cwd(Some("/shell/cwd/path"));
        *adapter.session.lock().unwrap() = SessionRuntimeState::with_active(
            "00000000-0000-4000-8000-000000000000",
            "/session/scope/path".to_string(),
        );
        let result = adapter
            .registry_query("skills", "list", serde_json::Value::Null)
            .expect("shell_cwd should take priority");
        assert_eq!(result["workspace"], "/shell/cwd/path");
    }

    // 2. Falls back to session scope when shell_cwd is absent.
    {
        let adapter = CoshCoreAdapter::new(script.to_string_lossy().into_owned(), false);
        *adapter.session.lock().unwrap() = SessionRuntimeState::with_active(
            "00000000-0000-4000-8000-000000000000",
            "/session/scope/path".to_string(),
        );
        let result = adapter
            .registry_query("skills", "list", serde_json::Value::Null)
            .expect("session scope should be used as fallback");
        assert_eq!(result["workspace"], "/session/scope/path");
    }

    // 3. set_shell_cwd(None) retains the last known value instead of
    //    clearing it, so subsequent registry queries keep using the real
    //    shell cwd rather than silently degrading to session scope.
    {
        let adapter = CoshCoreAdapter::new(script.to_string_lossy().into_owned(), false);
        adapter.set_shell_cwd(Some("/shell/cwd/path"));
        adapter.set_shell_cwd(None);
        *adapter.session.lock().unwrap() = SessionRuntimeState::with_active(
            "00000000-0000-4000-8000-000000000000",
            "/session/scope/path".to_string(),
        );
        let result = adapter
            .registry_query("skills", "list", serde_json::Value::Null)
            .expect("last-known shell_cwd should survive None update");
        assert_eq!(result["workspace"], "/shell/cwd/path");
    }

    let _ = std::fs::remove_file(&script);
}
