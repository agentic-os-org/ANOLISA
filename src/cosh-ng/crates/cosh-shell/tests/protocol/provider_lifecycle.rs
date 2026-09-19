use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cosh_shell::adapter::{
    AdapterError, AgentAdapter, AgentRunHandle, AgentRunPoll, ClaudeCodeAdapter, CoshCoreAdapter,
    QwenCliAdapter, SessionRecoveryState, SessionRuntimeState,
};
use cosh_shell::types::{
    AgentEvent, AgentMode, AgentRequest, CommandBlock, CommandStatus, CoshApprovalMode, OutputRefs,
};

fn test_workspace_scope() -> String {
    fs::canonicalize(std::env::temp_dir())
        .expect("canonical test workspace")
        .to_string_lossy()
        .into_owned()
}

fn test_workspace_child(name: &str) -> String {
    Path::new(&test_workspace_scope())
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn mock_provider_script(name: &str, body: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "cosh-provider-lifecycle-{name}-{}-{nonce}.sh",
        std::process::id()
    ));
    let persistent_handshake = if name.starts_with("cosh-core-") && !body.contains("read -r init") {
        concat!(
            "IFS= read -r _\n",
            "printf '%s\\n' '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"init-1\",\"response\":{\"subtype\":\"initialize\",\"capabilities\":{}}}}'\n",
            "IFS= read -r _\n",
        )
    } else {
        ""
    };
    fs::write(&path, format!("#!/bin/sh\n{persistent_handshake}{body}\n"))
        .expect("write mock provider");
    let mut permissions = fs::metadata(&path)
        .expect("mock provider metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod mock provider");
    path
}

fn qwen_adapter(program: &Path, session_id: Arc<Mutex<Option<String>>>) -> QwenCliAdapter {
    QwenCliAdapter {
        program: program.display().to_string(),
        allow_model_call: true,
        session_id,
    }
}

fn claude_adapter(program: &Path) -> ClaudeCodeAdapter {
    ClaudeCodeAdapter {
        program: program.display().to_string(),
        model: "mock".to_string(),
        max_budget_usd: "1".to_string(),
        allow_model_call: true,
        session_id: Arc::new(Mutex::new(None)),
    }
}

fn cosh_core_restore_adapter(program: &Path) -> CoshCoreAdapter {
    let workspace_scope = test_workspace_scope();
    let mut session = SessionRuntimeState::with_active(
        "00000000-0000-4000-8000-000000000000",
        workspace_scope.clone(),
    );
    session.recovery.state = SessionRecoveryState::Selected;
    session.recovery.selected_session_id = Some("11111111-1111-4111-8111-111111111111".to_string());
    session.recovery.selected_workspace_scope = Some(workspace_scope);
    let adapter = CoshCoreAdapter::new(program.display().to_string(), true);
    *adapter.session.lock().unwrap() = session;
    adapter
}

fn cosh_core_active_adapter(program: &Path) -> CoshCoreAdapter {
    let adapter = CoshCoreAdapter::new(program.display().to_string(), true);
    *adapter.session.lock().unwrap() = SessionRuntimeState::with_active(
        "00000000-0000-4000-8000-000000000000",
        test_workspace_scope(),
    );
    adapter
}

fn cosh_core_active_with_unrelated_selection(program: &Path) -> CoshCoreAdapter {
    let mut session = SessionRuntimeState::with_active(
        "00000000-0000-4000-8000-000000000000",
        test_workspace_child("workspace-b"),
    );
    session.recovery.state = SessionRecoveryState::Selected;
    session.recovery.selected_session_id = Some("11111111-1111-4111-8111-111111111111".to_string());
    session.recovery.selected_workspace_scope = Some(test_workspace_child("workspace-a"));
    let adapter = CoshCoreAdapter::new(program.display().to_string(), true);
    *adapter.session.lock().unwrap() = session;
    adapter
}

fn make_request(id: &str) -> AgentRequest {
    let workspace_scope = test_workspace_scope();
    AgentRequest {
        id: id.to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "echo test".to_string(),
            origin: Default::default(),
            cwd: workspace_scope.clone(),
            end_cwd: workspace_scope,
            started_at_ms: 0,
            ended_at_ms: 1,
            duration_ms: 1,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("test provider lifecycle".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

fn assert_process_is_gone(pid_file: &Path) {
    let pid: i32 = fs::read_to_string(pid_file)
        .expect("read provider pid")
        .trim()
        .parse()
        .expect("parse provider pid");
    let result = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None);
    assert_eq!(
        result,
        Err(nix::errno::Errno::ESRCH),
        "provider PID {pid} is still alive or returned an unexpected probe result"
    );
}

#[test]
fn cosh_core_persistent_user_message_carries_raw_input_separately() {
    let capture = std::env::temp_dir().join(format!(
        "cosh-core-protocol-persistent-raw-{}.json",
        std::process::id()
    ));
    let script = mock_provider_script(
        "cosh-core-persistent-raw-input",
        &format!(
            r#"IFS= read -r initialize
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","capabilities":{{}}}}}}}}'
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","model":"mock","tools":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"type":"user"'*)
      printf '%s\n' "$line" > "{}"
      printf '%s\n' '{{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"result":"done"}}'
      ;;
    *'"subtype":"shutdown"'*) exit 0;;
  esac
done"#,
            capture.display()
        ),
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);
    let events = collect_events_until_finished(
        &adapter.start_cancellable(make_request("persistent-raw-input"), CoshApprovalMode::Auto),
        Duration::from_secs(3),
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));

    let line = fs::read_to_string(&capture).expect("captured persistent user message");
    let message: serde_json::Value = serde_json::from_str(&line).expect("valid user message");
    assert_eq!(
        message["message"]["raw_user_input"],
        "test provider lifecycle"
    );
    assert!(message["message"]["content"]
        .as_str()
        .is_some_and(|content| content.contains("user_input: test provider lifecycle")));
    assert_ne!(
        message["message"]["content"],
        message["message"]["raw_user_input"]
    );

    drop(adapter);
    let _ = fs::remove_file(capture);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_sync_user_message_carries_raw_input_and_skips_session_start() {
    let capture = std::env::temp_dir().join(format!(
        "cosh-core-protocol-sync-raw-{}.json",
        std::process::id()
    ));
    let initialize_capture = capture.with_extension("init.json");
    let script = mock_provider_script(
        "cosh-core-sync-raw-input",
        &format!(
            r#"IFS= read -r initialize
printf '%s\n' "$initialize" > "{initialize_capture}"
IFS= read -r user_message
printf '%s\n' "$user_message" > "{}"
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","model":"mock","tools":[]}}'
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"result":"done"}}'"#,
            capture.display(),
            initialize_capture = initialize_capture.display()
        ),
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);
    let mut events = Vec::new();
    adapter
        .run_stream(&make_request("sync-raw-input"), &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect("run synchronous cosh-core mock");

    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    let initialize_line =
        fs::read_to_string(&initialize_capture).expect("captured synchronous initialize message");
    let initialize: serde_json::Value =
        serde_json::from_str(&initialize_line).expect("valid initialize message");
    assert_eq!(initialize["type"], "control_request");
    assert_eq!(initialize["request"]["subtype"], "initialize");
    assert_eq!(initialize["request"]["fire_session_start"], false);
    let line = fs::read_to_string(&capture).expect("captured synchronous user message");
    let message: serde_json::Value = serde_json::from_str(&line).expect("valid user message");
    assert_eq!(
        message["message"]["raw_user_input"],
        "test provider lifecycle"
    );
    assert!(message["message"]["content"]
        .as_str()
        .is_some_and(|content| content.contains("user_input: test provider lifecycle")));

    let _ = fs::remove_file(capture);
    let _ = fs::remove_file(initialize_capture);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_sync_stdin_write_failure_cleans_up_and_preserves_recovery_state() {
    let pid_file = std::env::temp_dir().join(format!(
        "cosh-core-protocol-sync-write-failure-{}.pid",
        std::process::id()
    ));
    let script = mock_provider_script(
        "cosh-core-sync-write-failure",
        &format!(
            r#"IFS= read -r init
printf '%s\n' "$$" > "{}"
exec 0<&-
trap '' TERM
exec sleep 30"#,
            pid_file.display()
        ),
    );
    let adapter = cosh_core_active_adapter(&script);
    let mut request = make_request("sync-write-failure");
    request.user_input = Some("x".repeat(2 * 1024 * 1024));

    let error = adapter
        .run_stream(&request, &mut |_| Ok(()))
        .expect_err("closed cosh-core stdin should fail the sync write");
    assert!(
        error
            .message
            .contains("failed to write cosh-core user message"),
        "unexpected sync write error: {}",
        error.message
    );
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Active
    );
    assert_process_is_gone(&pid_file);
    let _ = fs::remove_file(pid_file);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_sync_drains_child_output_while_writing_large_prompt() {
    let pid_file = std::env::temp_dir().join(format!(
        "cosh-core-protocol-sync-large-prompt-{}.pid",
        std::process::id()
    ));
    let script = mock_provider_script(
        "cosh-core-sync-large-prompt",
        &format!(
            r#"printf '%s\n' "$$" > "{}"
IFS= read -r init
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","capabilities":{{}}}}}}}}'
large_tool_name=$(head -c 262144 /dev/zero | tr '\0' x)
printf '{{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","model":"mock","tools":["%s"]}}\n' "$large_tool_name"
IFS= read -r user_message
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"result":"done"}}'"#,
            pid_file.display()
        ),
    );
    let adapter = cosh_core_active_adapter(&script);
    let mut request = make_request("sync-large-prompt");
    request.user_input = Some("x".repeat(2 * 1024 * 1024));
    let (result_tx, result_rx) = mpsc::channel();
    let run_thread = thread::spawn(move || {
        let mut events = Vec::new();
        let result = adapter.run_stream(&request, &mut |event| {
            events.push(event);
            Ok(())
        });
        result_tx
            .send((result, events))
            .expect("send synchronous run result");
    });

    // The 2 MB prompt and 256 KB tool-name output are intentionally larger
    // than pipe buffers, so this test exercises drain-while-write behavior.
    // Shell reads of multi-megabyte lines can take ~2 s even on fast hosts
    // and longer under parallel test load, so give the transport ample headroom.
    let run_result = match result_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(error) => {
            let pid = fs::read_to_string(&pid_file)
                .expect("read provider PID after sync transport timeout")
                .trim()
                .parse::<i32>()
                .expect("parse provider PID after sync transport timeout");
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-pid),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = run_thread.join();
            let _ = fs::remove_file(&pid_file);
            let _ = fs::remove_file(&script);
            panic!("synchronous cosh-core transport timed out: {error}");
        }
    };
    run_thread.join().expect("join synchronous run thread");
    let (result, events) = run_result;
    assert_process_is_gone(&pid_file);
    let _ = fs::remove_file(&pid_file);
    let _ = fs::remove_file(&script);
    assert!(result.is_ok(), "large prompt transport failed: {result:?}");
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "initialized")
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
}

fn collect_events_until(
    handle: &AgentRunHandle,
    timeout: Duration,
    predicate: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(event)) => {
                let done = predicate(&event);
                events.push(event);
                if done {
                    break;
                }
            }
            Ok(AgentRunPoll::Timeout) => {}
            Ok(AgentRunPoll::Finished) | Err(_) => break,
        }
    }
    events
}

fn collect_events_until_finished(handle: &AgentRunHandle, timeout: Duration) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        assert!(
            Instant::now() < deadline,
            "provider did not finish; events: {events:?}"
        );
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(event)) => events.push(event),
            Ok(AgentRunPoll::Timeout) => {}
            Ok(AgentRunPoll::Finished) => return events,
            Err(error) => panic!("provider event stream failed: {}", error.message),
        }
    }
}

fn assert_restore_identity_failure(events: &[AgentEvent]) {
    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error.contains("identity mismatch"))
        ),
        "expected identity mismatch failure, got: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "restore failure must suppress AgentCompleted: {events:?}"
    );
}

fn assert_provider_failure_is_preserved(events: &[AgentEvent]) {
    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error == "Reached maximum budget ($0.05)")
        ),
        "expected original provider failure, got: {events:?}"
    );
    assert!(
        !events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error.contains("provider session did not complete"))
        ),
        "generic restore failure replaced provider detail: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "provider failure must not emit AgentCompleted: {events:?}"
    );
}

fn assert_selected_structured_failure(
    adapter: &CoshCoreAdapter,
    code: &str,
    message: &str,
    hint_fragment: &str,
) {
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    let recovery = adapter.recovery_snapshot();
    assert_eq!(recovery.state, SessionRecoveryState::Failed);
    assert_eq!(recovery.selected_session_id, None);
    let error = recovery.last_error.as_ref().expect("typed session failure");
    assert_eq!(error.code, code);
    assert_eq!(error.message, message);
    assert!(error
        .hint
        .as_deref()
        .is_some_and(|hint| hint.contains(hint_fragment)));
}

fn assert_recorded_process_is_gone(pid_file: &Path) {
    let pid: i32 = fs::read_to_string(pid_file)
        .expect("read mock provider pid")
        .trim()
        .parse()
        .expect("parse mock provider pid");
    let result = unsafe { nix::libc::kill(pid, 0) };
    let error = std::io::Error::last_os_error();
    assert_eq!(result, -1, "mock provider PID {pid} is still alive");
    assert_eq!(
        error.raw_os_error(),
        Some(nix::libc::ESRCH),
        "unexpected PID probe error for {pid}: {error}"
    );
}

fn assert_recorded_process_is_not_running(pid_file: &Path) {
    let pid = fs::read_to_string(pid_file)
        .expect("read mock provider pid")
        .trim()
        .to_string();
    let status = fs::read_to_string(format!("/proc/{pid}/status"));
    if let Ok(status) = status {
        assert!(
            status
                .lines()
                .any(|line| { line.starts_with("State:\tZ") || line.starts_with("State:\tX") }),
            "mock provider descendant PID {pid} is still running: {status}"
        );
    }
}

#[test]
fn qwen_provider_lifecycle_cancellable_process_emits_cancelled_event() {
    let script = mock_provider_script("qwen-sleep", "exec /bin/sleep 10");
    let adapter = qwen_adapter(&script, Arc::new(Mutex::new(None)));
    let handle =
        adapter.start_cancellable(make_request("qwen-cancel"), CoshApprovalMode::Recommend);

    let starting = collect_events_until(
        &handle,
        Duration::from_secs(2),
        |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "starting"),
    );
    assert!(
        starting.iter().any(
            |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "starting")
        ),
        "expected starting event, got: {starting:?}"
    );

    handle.cancel();

    let cancelled = collect_events_until(&handle, Duration::from_secs(3), |event| {
        matches!(event, AgentEvent::AgentCancelled { .. })
    });
    let _ = fs::remove_file(script);
    assert!(
        cancelled
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
        "expected AgentCancelled after cancel, got: {cancelled:?}"
    );
}

#[test]
fn claude_provider_lifecycle_cancellable_process_emits_cancelled_event() {
    let script = mock_provider_script("claude-sleep", "exec /bin/sleep 10");
    let adapter = claude_adapter(&script);
    let handle =
        adapter.start_cancellable(make_request("claude-cancel"), CoshApprovalMode::Recommend);

    let starting = collect_events_until(
        &handle,
        Duration::from_secs(2),
        |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "starting"),
    );
    assert!(
        starting.iter().any(
            |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "starting")
        ),
        "expected starting event, got: {starting:?}"
    );

    handle.cancel();

    let cancelled = collect_events_until(&handle, Duration::from_secs(3), |event| {
        matches!(event, AgentEvent::AgentCancelled { .. })
    });
    let _ = fs::remove_file(script);
    assert!(
        cancelled
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
        "expected AgentCancelled after cancel, got: {cancelled:?}"
    );
}

#[test]
fn qwen_provider_lifecycle_commits_session_only_after_successful_completion() {
    let script = mock_provider_script(
        "qwen-success",
        "printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-ok\",\"model\":\"qwen\"}'\nprintf '%s\\n' '{\"type\":\"result\",\"session_id\":\"sess-ok\",\"result\":\"done\"}'",
    );
    let committed = Arc::new(Mutex::new(None));
    let adapter = qwen_adapter(&script, Arc::clone(&committed));
    let handle =
        adapter.start_cancellable(make_request("qwen-success"), CoshApprovalMode::Recommend);

    let completed = collect_events_until(&handle, Duration::from_secs(3), |event| {
        matches!(event, AgentEvent::AgentCompleted { .. })
    });
    let _ = fs::remove_file(script);
    assert!(
        completed
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "expected AgentCompleted, got: {completed:?}"
    );
    assert_eq!(
        committed.lock().expect("committed session").as_deref(),
        Some("sess-ok")
    );
}

#[test]
fn qwen_provider_lifecycle_does_not_commit_session_after_provider_failure() {
    let script = mock_provider_script(
        "qwen-failure",
        "printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sess-bad\",\"model\":\"qwen\"}'\nexit 2",
    );
    let committed = Arc::new(Mutex::new(Some("sess-prev".to_string())));
    let adapter = qwen_adapter(&script, Arc::clone(&committed));
    let handle =
        adapter.start_cancellable(make_request("qwen-failure"), CoshApprovalMode::Recommend);

    let failed = collect_events_until(&handle, Duration::from_secs(3), |event| {
        matches!(event, AgentEvent::AgentFailed { .. })
    });
    let _ = fs::remove_file(script);
    assert!(
        failed
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentFailed { .. })),
        "expected AgentFailed, got: {failed:?}"
    );
    assert_eq!(
        committed.lock().expect("committed session").as_deref(),
        Some("sess-prev")
    );
}

#[test]
fn cosh_core_sync_restore_identity_failure_replaces_completed_event() {
    let script = mock_provider_script(
        "cosh-core-sync-identity-mismatch",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"22222222-2222-4222-8222-222222222222","model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"22222222-2222-4222-8222-222222222222","is_error":false,"duration_ms":1,"result":"done"}'"#,
    );
    let adapter = cosh_core_restore_adapter(&script);

    let events = adapter
        .run(&make_request("cosh-core-sync-identity-mismatch"))
        .expect("run mismatched restore");

    assert_restore_identity_failure(&events);
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Failed
    );
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_async_restore_identity_failure_replaces_completed_event() {
    let script = mock_provider_script(
        "cosh-core-async-identity-mismatch",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"22222222-2222-4222-8222-222222222222","model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"22222222-2222-4222-8222-222222222222","is_error":false,"duration_ms":1,"result":"done"}'"#,
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_restore_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-identity-mismatch")),
            mode,
        );

        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert_restore_identity_failure(&events);
        assert_eq!(
            adapter.committed_session_id().as_deref(),
            Some("00000000-0000-4000-8000-000000000000")
        );
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Failed
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn active_resume_identity_mismatch_is_rejected_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-active-identity-mismatch",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"22222222-2222-4222-8222-222222222222","model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"22222222-2222-4222-8222-222222222222","is_error":false,"duration_ms":1,"result":"done"}'"#,
    );

    let sync = cosh_core_active_adapter(&script);
    let sync_events = sync
        .run(&make_request("cosh-core-sync-active-identity-mismatch"))
        .expect("run mismatched active resume");
    assert_restore_identity_failure(&sync_events);
    assert_eq!(sync.committed_session_id(), None);
    assert_eq!(sync.recovery_snapshot().state, SessionRecoveryState::Failed);

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_active_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-active-identity-mismatch")),
            mode,
        );
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert_restore_identity_failure(&events);
        assert_eq!(adapter.committed_session_id(), None);
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Failed
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_sync_restore_preserves_provider_result_error() {
    let script = mock_provider_script(
        "cosh-core-sync-result-error",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error_max_budget_usd","session_id":"11111111-1111-4111-8111-111111111111","is_error":true,"errors":["Reached maximum budget ($0.05)"]}'"#,
    );
    let adapter = cosh_core_restore_adapter(&script);

    let events = adapter
        .run(&make_request("cosh-core-sync-result-error"))
        .expect("run provider result error");

    assert_provider_failure_is_preserved(&events);
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Failed
    );
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_async_restore_preserves_provider_result_error() {
    let script = mock_provider_script(
        "cosh-core-async-result-error",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error_max_budget_usd","session_id":"11111111-1111-4111-8111-111111111111","is_error":true,"errors":["Reached maximum budget ($0.05)"]}'"#,
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_restore_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-result-error")),
            mode,
        );

        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert_provider_failure_is_preserved(&events);
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Failed
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_active_load_failure_releases_resume_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-active-not-found",
        r#"printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["session recovery failed [not_found]: session not found"],"session_error_code":"not_found","session_error_phase":"load"}'"#,
    );

    let sync = cosh_core_active_adapter(&script);
    let sync_events = sync
        .run(&make_request("cosh-core-sync-active-not-found"))
        .expect("run sync active resume failure");
    assert!(sync_events.iter().any(
        |event| matches!(event, AgentEvent::AgentFailed { error, .. }
            if error.contains("[not_found]"))
    ));
    assert_eq!(sync.committed_session_id(), None);
    assert!(sync.protected_session_ids().is_empty());
    assert_eq!(sync.recovery_snapshot().state, SessionRecoveryState::Failed);
    assert_eq!(
        sync.recovery_snapshot()
            .last_error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("not_found")
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_active_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-active-not-found")),
            mode,
        );
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error.contains("[not_found]"))
        ));
        assert_eq!(adapter.committed_session_id(), None);
        assert!(adapter.protected_session_ids().is_empty());
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Failed
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn active_load_failure_preserves_unrelated_selection_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-active-not-found-with-selection",
        r#"printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["session recovery failed"],"session_error_code":"not_found","session_error_phase":"load"}'"#,
    );
    let mut request = make_request("cosh-core-sync-active-not-found-with-selection");
    request.command_block.cwd = test_workspace_child("workspace-b");
    request.command_block.end_cwd = test_workspace_child("workspace-b");

    let sync = cosh_core_active_with_unrelated_selection(&script);
    let _ = sync.run(&request).expect("run sync active resume failure");
    assert_eq!(sync.committed_session_id(), None);
    assert_eq!(
        sync.recovery_snapshot().state,
        SessionRecoveryState::Selected
    );
    assert_eq!(
        sync.recovery_snapshot().selected_session_id.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_active_with_unrelated_selection(&script);
        let mut request = request.clone();
        request.id = format!("cosh-core-{label}-active-not-found-with-selection");
        let handle = adapter.start_cancellable(request, mode);
        let _ = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert_eq!(adapter.committed_session_id(), None);
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Selected
        );
        assert_eq!(
            adapter.recovery_snapshot().selected_session_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn disable_resume_hint_preserves_selection_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-disable-resume",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"22222222-2222-4222-8222-222222222222","model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"22222222-2222-4222-8222-222222222222","is_error":false,"duration_ms":1,"result":"done"}'"#,
    );
    let mut request = make_request("cosh-core-sync-disable-resume");
    request
        .context_hints
        .push("disable provider resume for shell handoff fallback".to_string());

    let sync = cosh_core_restore_adapter(&script);
    let sync_events = sync.run(&request).expect("run sync without resume");
    assert!(sync_events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    assert_eq!(
        sync.recovery_snapshot().state,
        SessionRecoveryState::Selected
    );
    assert_eq!(
        sync.recovery_snapshot().selected_session_id.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(
        sync.committed_session_id().as_deref(),
        Some("22222222-2222-4222-8222-222222222222")
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_restore_adapter(&script);
        let mut request = request.clone();
        request.id = format!("cosh-core-{label}-disable-resume");
        let handle = adapter.start_cancellable(request, mode);
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentFailed { .. })));
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Selected
        );
        assert_eq!(
            adapter.recovery_snapshot().selected_session_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn disabled_resume_non_resumable_turn_preserves_unattempted_ids_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-disable-non-resumable",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"22222222-2222-4222-8222-222222222222","session_resumable":false,"model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"22222222-2222-4222-8222-222222222222","is_error":false,"duration_ms":1,"result":"done"}'"#,
    );
    let mut request = make_request("cosh-core-sync-disable-non-resumable");
    request
        .context_hints
        .push("disable provider resume for shell handoff fallback".to_string());

    let sync = cosh_core_restore_adapter(&script);
    let sync_events = sync.run(&request).expect("run sync non-resumable turn");
    assert!(sync_events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    assert_eq!(
        sync.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    assert_eq!(
        sync.recovery_snapshot().selected_session_id.as_deref(),
        Some("11111111-1111-4111-8111-111111111111")
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_restore_adapter(&script);
        let mut request = request.clone();
        request.id = format!("cosh-core-{label}-disable-non-resumable");
        let handle = adapter.start_cancellable(request, mode);
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
        assert_eq!(
            adapter.committed_session_id().as_deref(),
            Some("00000000-0000-4000-8000-000000000000")
        );
        assert_eq!(
            adapter.recovery_snapshot().selected_session_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn ordinary_active_provider_failure_keeps_committed_resume() {
    let script = mock_provider_script(
        "cosh-core-active-budget-error",
        r#"printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["Reached maximum budget ($0.05)"]}'"#,
    );
    let adapter = cosh_core_active_adapter(&script);

    let events = adapter
        .run(&make_request("cosh-core-active-budget-error"))
        .expect("run ordinary provider failure");

    assert_provider_failure_is_preserved(&events);
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Active
    );
    let _ = fs::remove_file(script);
}

#[test]
fn active_persistence_failure_releases_resume_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-active-persist-conflict",
        r#"printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["session persistence failed [conflict]: session changed concurrently"],"session_error_code":"conflict","session_error_phase":"persist"}'"#,
    );

    let sync = cosh_core_active_adapter(&script);
    let sync_events = sync
        .run(&make_request("cosh-core-sync-active-persist-conflict"))
        .expect("run active persistence failure");
    assert!(sync_events.iter().any(
        |event| matches!(event, AgentEvent::AgentFailed { error, .. }
            if error.contains("persistence failed [conflict]"))
    ));
    assert_eq!(sync.committed_session_id(), None);
    assert_eq!(sync.recovery_snapshot().state, SessionRecoveryState::Failed);
    assert_eq!(
        sync.recovery_snapshot()
            .last_error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("conflict")
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_active_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-active-persist-conflict")),
            mode,
        );
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error.contains("persistence failed [conflict]"))
        ));
        assert_eq!(adapter.committed_session_id(), None);
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Failed
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn selected_structured_failures_preserve_metadata_for_every_runner() {
    for (label, code, phase, message, hint_fragment) in [
        (
            "load",
            "scope_mismatch",
            "load",
            "session recovery failed [scope_mismatch]: selected workspace changed",
            "Refresh the session list",
        ),
        (
            "persist",
            "conflict",
            "persist",
            "session persistence failed [conflict]: selected session changed",
            "Resolve the persistence failure",
        ),
    ] {
        let script = mock_provider_script(
            &format!("cosh-core-selected-{label}-failure"),
            &format!(
                "printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"error\",\
                 \"session_id\":\"11111111-1111-4111-8111-111111111111\",\
                 \"is_error\":true,\"errors\":[\"{message}\"],\
                 \"session_error_code\":\"{code}\",\"session_error_phase\":\"{phase}\"}}'"
            ),
        );

        let sync = cosh_core_restore_adapter(&script);
        let sync_events = sync
            .run(&make_request(&format!("cosh-core-sync-selected-{label}")))
            .expect("run selected structured failure");
        assert!(sync_events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. } if error == message)
        ));
        assert_selected_structured_failure(&sync, code, message, hint_fragment);

        for (runner, mode) in [
            ("recommend", CoshApprovalMode::Recommend),
            ("control", CoshApprovalMode::Auto),
        ] {
            let adapter = cosh_core_restore_adapter(&script);
            let handle = adapter.start_cancellable(
                make_request(&format!("cosh-core-{runner}-selected-{label}")),
                mode,
            );
            let events = collect_events_until_finished(&handle, Duration::from_secs(3));

            assert!(events.iter().any(
                |event| matches!(event, AgentEvent::AgentFailed { error, .. } if error == message)
            ));
            assert_selected_structured_failure(&adapter, code, message, hint_fragment);
        }
        let _ = fs::remove_file(script);
    }
}

#[test]
fn cancelled_async_runners_apply_already_parsed_session_failure() {
    let script = mock_provider_script(
        "cosh-core-persist-conflict-then-cancel",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","model":"mock","tools":[]}'
IFS= read -r _
printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["session persistence failed [conflict]: parsed before cancellation"],"session_error_code":"conflict","session_error_phase":"persist"}'
exec sleep 30"#,
    );

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_active_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-persist-then-cancel")),
            mode,
        );
        let initialized = collect_events_until(
            &handle,
            Duration::from_secs(3),
            |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "initialized"),
        );
        assert!(
            initialized.iter().any(
                |event| matches!(event, AgentEvent::StatusChanged { phase, .. }
                    if phase == "initialized")
            ),
            "{label} runner did not parse the marker after the structured failure: {initialized:?}"
        );

        handle.cancel();
        let cancelled = collect_events_until(&handle, Duration::from_secs(3), |event| {
            matches!(event, AgentEvent::AgentCancelled { .. })
        });

        assert!(
            cancelled
                .iter()
                .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
            "{label} runner did not preserve cancellation semantics: {cancelled:?}"
        );
        assert_eq!(adapter.committed_session_id(), None);
        let recovery = adapter.recovery_snapshot();
        assert_eq!(recovery.state, SessionRecoveryState::Failed);
        assert_eq!(
            recovery
                .last_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("conflict")
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn cancelled_selected_runners_preserve_already_parsed_session_failure() {
    for (failure, code, phase, message, hint_fragment) in [
        (
            "load",
            "not_found",
            "load",
            "session recovery failed [not_found]: selected session disappeared",
            "Refresh the session list",
        ),
        (
            "persist",
            "conflict",
            "persist",
            "session persistence failed [conflict]: selected session changed",
            "Resolve the persistence failure",
        ),
    ] {
        let script = mock_provider_script(
            &format!("cosh-core-selected-{failure}-then-cancel"),
            &format!(
                r#"printf '%s\n' '{{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}}'
IFS= read -r _
printf '%s\n' '{{"type":"result","subtype":"error","session_id":"11111111-1111-4111-8111-111111111111","is_error":true,"errors":["{message}"],"session_error_code":"{code}","session_error_phase":"{phase}"}}'
exec sleep 30"#
            ),
        );

        for (runner, mode) in [
            ("recommend", CoshApprovalMode::Recommend),
            ("control", CoshApprovalMode::Auto),
        ] {
            let adapter = cosh_core_restore_adapter(&script);
            let handle = adapter.start_cancellable(
                make_request(&format!("cosh-core-{runner}-selected-{failure}-cancel")),
                mode,
            );
            let initialized = collect_events_until(
                &handle,
                Duration::from_secs(3),
                |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "initialized"),
            );
            assert!(
                initialized.iter().any(
                    |event| matches!(event, AgentEvent::StatusChanged { phase, .. }
                        if phase == "initialized")
                ),
                "{runner} runner did not parse selected {failure} failure: {initialized:?}"
            );

            handle.cancel();
            let cancelled = collect_events_until(&handle, Duration::from_secs(3), |event| {
                matches!(event, AgentEvent::AgentCancelled { .. })
            });
            assert!(
                cancelled
                    .iter()
                    .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
                "{runner} runner did not preserve cancellation: {cancelled:?}"
            );
            assert_selected_structured_failure(&adapter, code, message, hint_fragment);
        }
        let _ = fs::remove_file(script);
    }
}

#[test]
fn cosh_core_cancel_after_completed_result_emits_only_cancelled() {
    // If a genuine AgentCompleted races with the cancel signal, cancellation
    // must still win: the terminal event stream should contain AgentCancelled
    // and no AgentCompleted.
    let script = mock_provider_script(
        "cosh-core-cancel-after-completed-result",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000"}'
exec sleep 30"#,
    );

    let adapter = cosh_core_active_adapter(&script);
    let handle = adapter.start_cancellable(
        make_request("cosh-core-cancel-after-completed-result"),
        CoshApprovalMode::Recommend,
    );
    // Give the provider enough time to emit init + result before cancelling.
    thread::sleep(Duration::from_millis(200));
    handle.cancel();
    let events = collect_events_until_finished(&handle, Duration::from_secs(3));

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
        "cancelled run should emit AgentCancelled: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "cancelled run must not emit AgentCompleted: {events:?}"
    );
    let _ = fs::remove_file(script);
}

#[test]
fn structured_session_failure_survives_nonzero_exit_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-active-persist-conflict-exit-one",
        r#"printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["session persistence failed [conflict]: retained detail"],"session_error_code":"conflict","session_error_phase":"persist"}'
exit 1"#,
    );

    let sync = cosh_core_active_adapter(&script);
    let sync_events = sync
        .run(&make_request("cosh-core-sync-persist-exit-one"))
        .expect("run structured nonzero persistence failure");
    assert!(sync_events.iter().any(
        |event| matches!(event, AgentEvent::AgentFailed { error, .. }
            if error == "session persistence failed [conflict]: retained detail")
    ));
    assert_eq!(sync.committed_session_id(), None);

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_active_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-persist-exit-one")),
            mode,
        );
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error == "session persistence failed [conflict]: retained detail")
        ));
        assert_eq!(adapter.committed_session_id(), None);
        assert_eq!(
            adapter
                .recovery_snapshot()
                .last_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("conflict")
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_pending_question_nonzero_exit_reports_only_protocol_failure() {
    let script = mock_provider_script(
        "cosh-core-question-then-nonzero",
        r#"read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","model":"mock"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"ask-pending","request":{"subtype":"ask_user","question":"Choose","options":[{"label":"One"}],"allow_free_text":false,"multi_select":false}}'
printf '%s\n' 'provider stderr must stay hidden' >&2
exit 7"#,
    );
    let adapter = cosh_core_active_adapter(&script);
    let handle = adapter.start_cancellable(
        make_request("cosh-core-question-nonzero"),
        CoshApprovalMode::Auto,
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Vec::new();
    let mut errors = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "provider did not finish");
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(event)) => events.push(event),
            Ok(AgentRunPoll::Timeout) => {}
            Ok(AgentRunPoll::Finished) => break,
            Err(error) => errors.push(error.message),
        }
    }

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::UserQuestion { .. })),
        "question was not emitted: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentFailed { .. })),
        "generic process failure must not precede protocol failure: {events:?}"
    );
    assert_eq!(
        errors,
        vec!["cosh-core-question-protocol:premature-completion"]
    );
    assert!(
        !format!("{events:?}{errors:?}").contains("provider stderr must stay hidden"),
        "provider stderr leaked through the protocol failure"
    );
    let _ = fs::remove_file(script);
}

#[test]
fn structured_session_failure_is_finalized_before_read_error_for_every_runner() {
    let script = mock_provider_script(
        "cosh-core-persist-conflict-invalid-utf8",
        r#"printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["session persistence failed [conflict]: retained before read failure"],"session_error_code":"conflict","session_error_phase":"persist"}'
printf '\377\n'
exec sleep 30"#,
    );

    let sync = cosh_core_active_adapter(&script);
    let mut sync_events = Vec::new();
    let sync_error = sync
        .run_stream(
            &make_request("cosh-core-sync-persist-read-error"),
            &mut |event| {
                sync_events.push(event);
                Ok(())
            },
        )
        .expect_err("invalid UTF-8 must remain a transport error");
    assert!(sync_events.iter().any(
        |event| matches!(event, AgentEvent::AgentFailed { error, .. }
            if error == "session persistence failed [conflict]: retained before read failure")
    ));
    assert!(sync_error
        .message
        .contains("failed to read cosh-core stream"));
    assert_eq!(sync.committed_session_id(), None);

    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let adapter = cosh_core_active_adapter(&script);
        let handle = adapter.start_cancellable(
            make_request(&format!("cosh-core-{label}-persist-read-error")),
            mode,
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_structured_failure = false;
        let mut saw_transport_error_after_failure = false;
        loop {
            assert!(Instant::now() < deadline, "{label} runner did not finish");
            match handle.poll_event_timeout(Duration::from_millis(100)) {
                Ok(AgentRunPoll::Event(AgentEvent::AgentFailed { error, .. }))
                    if error
                        == "session persistence failed [conflict]: retained before read failure" =>
                {
                    saw_structured_failure = true;
                }
                Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => {}
                Ok(AgentRunPoll::Finished) => break,
                Err(error) => {
                    assert!(
                        saw_structured_failure,
                        "{label} transport error arrived before structured failure: {}",
                        error.message
                    );
                    assert!(
                        error.message.contains("failed to read cosh-core stream"),
                        "{}",
                        error.message
                    );
                    saw_transport_error_after_failure = true;
                }
            }
        }
        assert!(saw_structured_failure, "{label} lost structured failure");
        assert!(
            saw_transport_error_after_failure,
            "{label} lost transport failure"
        );
        assert_eq!(adapter.committed_session_id(), None);
        assert_eq!(
            adapter
                .recovery_snapshot()
                .last_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("conflict")
        );
    }
    let _ = fs::remove_file(script);
}

#[test]
fn ordinary_provider_error_marker_cannot_release_active_resume() {
    let script = mock_provider_script(
        "cosh-core-active-marker-error",
        r#"printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"errors":["model output mentioned [not_found] without a session error code"]}'"#,
    );
    let adapter = cosh_core_active_adapter(&script);

    let events = adapter
        .run(&make_request("cosh-core-active-marker-error"))
        .expect("run ordinary marker provider failure");

    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::AgentFailed { error, .. }
            if error.contains("[not_found]"))
    ));
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Active
    );
    let _ = fs::remove_file(script);
}

#[test]
fn max_turn_failure_keeps_the_persistent_service_session_active_for_continuation() {
    let spawn_log = std::env::temp_dir().join(format!(
        "cosh-core-max-turns-spawns-{}.log",
        std::process::id()
    ));
    let _ = fs::remove_file(&spawn_log);
    let script = mock_provider_script(
        "cosh-core-max-turns-continue",
        &format!(
            r#"echo "$$" >> '{log}'
read -r init
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","capabilities":{{}}}}}}}}'
read -r first_message
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"{id}","session_resumable":true,"model":"mock","tools":[]}}'
printf '%s\n' '{{"type":"result","subtype":"error","session_id":"{id}","is_error":true,"result":"Agent exceeded max turns (50)","error_code":"max_turns","max_turns":50}}'
read -r second_message
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"{id}","session_resumable":true,"model":"mock","tools":[]}}'
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"{id}","is_error":false,"duration_ms":1,"result":"continued"}}'
read -r _"#,
            log = spawn_log.display(),
            id = "00000000-0000-4000-8000-000000000000",
        ),
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);

    let capped = adapter.start_cancellable(
        make_request("cosh-core-max-turns-first"),
        CoshApprovalMode::Auto,
    );
    let capped_events = collect_events_until_finished(&capped, Duration::from_secs(5));

    // The user must still see why the run stopped.
    assert!(
        capped_events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error == "Agent exceeded max turns (50)")
        ),
        "max-turn failure was suppressed: {capped_events:?}"
    );
    assert!(
        !capped_events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "a capped run must not report completion: {capped_events:?}"
    );
    // cosh-core persisted the transcript before reporting the cap, so the
    // session stays committed and resumable.
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Active
    );

    let continued = adapter.start_cancellable(
        make_request("cosh-core-max-turns-continue"),
        CoshApprovalMode::Auto,
    );
    let continued_events = collect_events_until_finished(&continued, Duration::from_secs(5));

    assert!(
        continued_events
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "continuation did not complete: {continued_events:?}"
    );
    assert!(
        !continued_events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error.contains("identity mismatch"))
        ),
        "continuation rejected the retained session identity: {continued_events:?}"
    );
    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Active
    );
    // The retained session must be reached through the same long-lived core
    // process; a reset would have spawned the mock a second time.
    let spawns = fs::read_to_string(&spawn_log).expect("read mock spawn log");
    assert_eq!(
        spawns.lines().count(),
        1,
        "the persistent core process was respawned: {spawns:?}"
    );

    let _ = fs::remove_file(&spawn_log);
    let _ = fs::remove_file(script);
}

#[test]
fn non_resumable_max_turn_failure_commits_no_persistent_service_session() {
    let script = mock_provider_script(
        "cosh-core-max-turns-non-resumable",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":false,"model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"result":"Agent exceeded max turns (50)","error_code":"max_turns","max_turns":50}'"#,
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);

    let handle = adapter.start_cancellable(
        make_request("cosh-core-max-turns-non-resumable"),
        CoshApprovalMode::Auto,
    );
    let events = collect_events_until_finished(&handle, Duration::from_secs(5));

    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error == "Agent exceeded max turns (50)")
        ),
        "max-turn failure was suppressed: {events:?}"
    );
    assert_eq!(adapter.committed_session_id(), None);
    let _ = fs::remove_file(script);
}

#[test]
fn non_resumable_service_process_commits_no_session_on_later_turns() {
    // cosh-core announces `session_resumable` once per process, so a second
    // turn's stream carries no `system/init`. Neither a capped nor a successful
    // later turn may commit a session whose persistence is disabled.
    for (label, second_turn) in [
        (
            "max-turns",
            r#"{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"result":"Agent exceeded max turns (50)","error_code":"max_turns","max_turns":50}"#,
        ),
        (
            "success",
            r#"{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"duration_ms":1,"result":"second"}"#,
        ),
    ] {
        let script = mock_provider_script(
            &format!("cosh-core-non-resumable-{label}"),
            &format!(
                r#"read -r init
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","capabilities":{{}}}}}}}}'
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":false,"model":"mock","tools":[]}}'
read -r first_message
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"00000000-0000-4000-8000-000000000000","is_error":false,"duration_ms":1,"result":"first"}}'
read -r second_message
printf '%s\n' '{second_turn}'
read -r _"#
            ),
        );
        let adapter = CoshCoreAdapter::new(script.display().to_string(), true);

        let first = adapter.start_cancellable(
            make_request(&format!("cosh-core-non-resumable-{label}-first")),
            CoshApprovalMode::Auto,
        );
        let _ = collect_events_until_finished(&first, Duration::from_secs(5));
        assert_eq!(
            adapter.committed_session_id(),
            None,
            "{label}: a non-resumable session must not commit"
        );

        let second = adapter.start_cancellable(
            make_request(&format!("cosh-core-non-resumable-{label}-second")),
            CoshApprovalMode::Auto,
        );
        let _ = collect_events_until_finished(&second, Duration::from_secs(5));

        assert_eq!(
            second.pending_provider_session_id(),
            None,
            "{label}: a non-resumable session leaked into the pending run state"
        );
        assert_eq!(
            adapter.committed_session_id(),
            None,
            "{label}: a later turn lost the process's non-resumable state"
        );
        let _ = fs::remove_file(script);
    }
}

#[test]
fn ordinary_service_failure_commits_no_persistent_session() {
    let script = mock_provider_script(
        "cosh-core-service-api-error",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":true,"model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"result":"API error 500"}'"#,
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);

    let handle = adapter.start_cancellable(
        make_request("cosh-core-service-api-error"),
        CoshApprovalMode::Auto,
    );
    let events = collect_events_until_finished(&handle, Duration::from_secs(5));

    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error == "API error 500")
        ),
        "provider failure was suppressed: {events:?}"
    );
    assert_eq!(adapter.committed_session_id(), None);
    let _ = fs::remove_file(script);
}

#[test]
fn persist_phase_max_turn_failure_commits_no_persistent_session() {
    let script = mock_provider_script(
        "cosh-core-max-turns-persist-failure",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":true,"model":"mock","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"result":"Agent exceeded max turns (50)","session_error_code":"conflict","session_error_phase":"persist"}'"#,
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);

    let handle = adapter.start_cancellable(
        make_request("cosh-core-max-turns-persist-failure"),
        CoshApprovalMode::Auto,
    );
    let events = collect_events_until_finished(&handle, Duration::from_secs(5));

    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                if error == "Agent exceeded max turns (50)")
        ),
        "max-turn failure was suppressed: {events:?}"
    );
    // The transcript never reached the store, so nothing may be resumed.
    assert_eq!(adapter.committed_session_id(), None);
    let _ = fs::remove_file(script);
}

#[test]
fn cancelled_max_turn_run_commits_no_fresh_persistent_session() {
    let script = mock_provider_script(
        "cosh-core-max-turns-then-cancel",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000000","session_resumable":true,"model":"mock","tools":[]}'
IFS= read -r _
printf '%s\n' '{"type":"result","subtype":"error","session_id":"00000000-0000-4000-8000-000000000000","is_error":true,"result":"Agent exceeded max turns (50)","error_code":"max_turns","max_turns":50}'
exec sleep 30"#,
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);
    let handle = adapter.start_cancellable(
        make_request("cosh-core-max-turns-then-cancel"),
        CoshApprovalMode::Auto,
    );
    let initialized = collect_events_until(
        &handle,
        Duration::from_secs(5),
        |event| matches!(event, AgentEvent::StatusChanged { phase, .. } if phase == "initialized"),
    );
    assert!(
        initialized.iter().any(
            |event| matches!(event, AgentEvent::StatusChanged { phase, .. }
                if phase == "initialized")
        ),
        "mock core never reported the session: {initialized:?}"
    );

    handle.cancel();
    let cancelled = collect_events_until(&handle, Duration::from_secs(5), |event| {
        matches!(event, AgentEvent::AgentCancelled { .. })
    });

    assert!(
        cancelled
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
        "cancellation semantics were lost: {cancelled:?}"
    );
    assert_eq!(adapter.committed_session_id(), None);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_sync_reaps_descendant_that_inherits_output_pipes() {
    let pid_file = std::env::temp_dir().join(format!(
        "cosh-core-sync-descendant-{}.pid",
        std::process::id()
    ));
    let script = mock_provider_script(
        "cosh-core-sync-descendant",
        &format!(
            r#"printf '%s\n' '{{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}}'
printf '%s\n' '{{"type":"result","subtype":"success","session_id":"11111111-1111-4111-8111-111111111111","is_error":false,"duration_ms":1,"result":"done"}}'
sleep 30 &
printf '%s\n' "$!" > "{}"
exit 0"#,
            pid_file.display()
        ),
    );
    let adapter = cosh_core_restore_adapter(&script);
    let started = Instant::now();

    let events = adapter
        .run(&make_request("cosh-core-sync-descendant"))
        .expect("sync runner must reap inherited pipes");

    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })));
    assert_recorded_process_is_not_running(&pid_file);
    let _ = fs::remove_file(pid_file);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_sync_sink_error_terminates_and_reaps_provider() {
    let pid_file =
        std::env::temp_dir().join(format!("cosh-core-sink-child-{}.pid", std::process::id()));
    let script = mock_provider_script(
        "cosh-core-sink-child",
        &format!(
            r#"printf '%s\n' "$$" > "{}"
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}}'
trap '' TERM
exec sleep 30"#,
            pid_file.display()
        ),
    );
    let adapter = cosh_core_restore_adapter(&script);

    let result = adapter.run_stream(&make_request("cosh-core-sink-child"), &mut |event| {
        if matches!(
            event,
            AgentEvent::StatusChanged { ref phase, .. } if phase == "initialized"
        ) {
            return Err(AdapterError {
                message: "provider sink failed".to_string(),
            });
        }
        Ok(())
    });

    assert_eq!(
        result.expect_err("provider sink failure").message,
        "provider sink failed"
    );
    assert_recorded_process_is_gone(&pid_file);
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Failed
    );
    let _ = fs::remove_file(pid_file);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_rejects_unsupported_initialize_before_user_turn() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let pid_file = std::env::temp_dir().join(format!(
        "cosh-core-version-error-{}-{nonce}.pid",
        std::process::id()
    ));
    let user_file = std::env::temp_dir().join(format!(
        "cosh-core-version-user-{}-{nonce}.txt",
        std::process::id()
    ));
    let script = mock_provider_script(
        "cosh-core-version-error",
        &format!(
            r#"printf '%s\n' "$$" > "{}"
IFS= read -r init
case "$init" in
  *'"protocol_version":1'*) ;;
  *) exit 3 ;;
esac
printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"init-1","response":{{"subtype":"initialize","protocol_version":9,"capabilities":{{}}}}}}}}'
if IFS= read -r unexpected; then
  printf '%s\n' "$unexpected" > "{}"
fi
exec sleep 30"#,
            pid_file.display(),
            user_file.display()
        ),
    );
    let adapter = CoshCoreAdapter::new(script.display().to_string(), true);
    let handle = adapter.start_cancellable(
        make_request("cosh-core-version-error"),
        CoshApprovalMode::Auto,
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Vec::new();
    let mut errors = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "version failure did not finish");
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(event)) => events.push(event),
            Ok(AgentRunPoll::Timeout) => {}
            Ok(AgentRunPoll::Finished) => break,
            Err(error) => errors.push(error.message),
        }
    }

    assert_eq!(errors.len(), 1, "unexpected errors: {errors:?}");
    assert!(errors[0].contains("unsupported control protocol version 9"));
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentCompleted { .. }
            | AgentEvent::AgentFailed { .. }
            | AgentEvent::AgentCancelled { .. }
    )));
    assert_eq!(adapter.committed_session_id(), None);
    assert_recorded_process_is_gone(&pid_file);
    assert!(!user_file.exists(), "user turn was sent before version ack");
    let _ = fs::remove_file(pid_file);
    let _ = fs::remove_file(user_file);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_sync_read_error_terminates_and_reaps_provider() {
    let pid_file =
        std::env::temp_dir().join(format!("cosh-core-read-child-{}.pid", std::process::id()));
    let script = mock_provider_script(
        "cosh-core-read-child",
        &format!(
            r#"printf '%s\n' "$$" > "{}"
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}}'
printf '\377\n'
trap '' TERM
exec sleep 30"#,
            pid_file.display()
        ),
    );
    let adapter = cosh_core_restore_adapter(&script);

    let error = adapter
        .run_stream(&make_request("cosh-core-read-child"), &mut |_| Ok(()))
        .expect_err("stream read failure");

    assert!(
        error.message.contains("failed to read cosh-core stream"),
        "{}",
        error.message
    );
    assert_recorded_process_is_gone(&pid_file);
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Failed
    );
    let _ = fs::remove_file(pid_file);
    let _ = fs::remove_file(script);
}

#[test]
fn cosh_core_async_read_error_terminates_and_reaps_provider() {
    for (label, mode) in [
        ("recommend", CoshApprovalMode::Recommend),
        ("control", CoshApprovalMode::Auto),
    ] {
        let pid_file = std::env::temp_dir().join(format!(
            "cosh-core-async-read-{label}-{}.pid",
            std::process::id()
        ));
        let script = mock_provider_script(
            &format!("cosh-core-async-read-{label}"),
            &format!(
                r#"printf '%s\n' "$$" > "{}"
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","model":"mock","tools":[]}}'
printf '\377\n'
trap '' TERM
exec sleep 30"#,
                pid_file.display()
            ),
        );
        let adapter = cosh_core_restore_adapter(&script);
        let handle =
            adapter.start_cancellable(make_request(&format!("cosh-core-async-read-{label}")), mode);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut observed_error = None;
        loop {
            assert!(Instant::now() < deadline, "async reader did not finish");
            match handle.poll_event_timeout(Duration::from_millis(100)) {
                Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => {}
                Ok(AgentRunPoll::Finished) => break,
                Err(error) => observed_error = Some(error),
            }
        }

        let error = observed_error.expect("stream read error");
        assert!(
            error.message.contains("failed to read cosh-core stream"),
            "{}",
            error.message
        );
        assert_recorded_process_is_gone(&pid_file);
        assert_eq!(
            adapter.recovery_snapshot().state,
            SessionRecoveryState::Failed
        );
        let _ = fs::remove_file(pid_file);
        let _ = fs::remove_file(script);
    }
}

#[test]
fn cosh_core_cancelled_non_resumable_restore_preserves_previous_active_session() {
    let script = mock_provider_script(
        "cosh-core-non-resumable-cancel",
        r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111","session_resumable":false,"model":"mock","tools":[]}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"ready"}]}}'
exec sleep 30"#,
    );
    let adapter = cosh_core_restore_adapter(&script);
    let handle = adapter.start_cancellable(
        make_request("cosh-core-non-resumable-cancel"),
        CoshApprovalMode::Recommend,
    );

    let ready = collect_events_until(
        &handle,
        Duration::from_secs(3),
        |event| matches!(event, AgentEvent::TextDelta { text, .. } if text == "ready"),
    );
    assert!(
        ready
            .iter()
            .any(|event| matches!(event, AgentEvent::TextDelta { text, .. } if text == "ready")),
        "provider init was not observed before cancellation: {ready:?}"
    );

    handle.cancel();
    let _ = collect_events_until_finished(&handle, Duration::from_secs(3));

    assert_eq!(
        adapter.committed_session_id().as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );
    let recovery = adapter.recovery_snapshot();
    assert_eq!(recovery.state, SessionRecoveryState::Failed);
    assert_eq!(recovery.selected_session_id, None);
    assert_eq!(recovery.selected_workspace_scope, None);
    let _ = fs::remove_file(script);
}

#[derive(Clone, Copy, Debug)]
enum SharedDriverProvider {
    Claude,
    Qwen,
}

impl SharedDriverProvider {
    fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Qwen => "qwen",
        }
    }

    fn start(self, program: &Path, mode: CoshApprovalMode, request_id: &str) -> AgentRunHandle {
        match self {
            Self::Claude => {
                claude_adapter(program).start_cancellable(make_request(request_id), mode)
            }
            Self::Qwen => qwen_adapter(program, Arc::new(Mutex::new(None)))
                .start_cancellable(make_request(request_id), mode),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ExpectedTerminal {
    Completed,
    Failed,
    Cancelled,
}

fn shared_driver_cases() -> impl Iterator<Item = (SharedDriverProvider, CoshApprovalMode)> {
    [SharedDriverProvider::Claude, SharedDriverProvider::Qwen]
        .into_iter()
        .flat_map(|provider| {
            [
                CoshApprovalMode::Recommend,
                CoshApprovalMode::Auto,
                CoshApprovalMode::Trust,
            ]
            .into_iter()
            .map(move |mode| (provider, mode))
        })
}

fn approval_mode_label(mode: CoshApprovalMode) -> &'static str {
    match mode {
        CoshApprovalMode::Recommend => "recommend",
        CoshApprovalMode::Auto => "auto",
        CoshApprovalMode::Trust => "trust",
    }
}

fn assert_single_terminal(events: &[AgentEvent], expected: ExpectedTerminal, context: &str) {
    let terminal_events = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::AgentCompleted { .. }
                    | AgentEvent::AgentFailed { .. }
                    | AgentEvent::AgentCancelled { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        terminal_events.len(),
        1,
        "{context} emitted multiple terminal events: {events:?}"
    );
    let matches_expected = match expected {
        ExpectedTerminal::Completed => {
            matches!(terminal_events[0], AgentEvent::AgentCompleted { .. })
        }
        ExpectedTerminal::Failed => {
            matches!(terminal_events[0], AgentEvent::AgentFailed { .. })
        }
        ExpectedTerminal::Cancelled => {
            matches!(terminal_events[0], AgentEvent::AgentCancelled { .. })
        }
    };
    assert!(
        matches_expected,
        "{context} emitted the wrong terminal event: {events:?}"
    );
}

#[test]
fn provider_drivers_emit_one_completed_terminal() {
    for (provider, mode) in shared_driver_cases() {
        let context = format!("{}-{}", provider.label(), approval_mode_label(mode));
        let script = mock_provider_script(
            &format!("shared-completed-{context}"),
            r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-shared","model":"mock"}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-shared","is_error":false,"result":"done"}'"#,
        );
        let handle = provider.start(&script, mode, &format!("shared-completed-{context}"));
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert_single_terminal(&events, ExpectedTerminal::Completed, &context);
        let _ = fs::remove_file(script);
    }
}

#[test]
fn provider_drivers_emit_one_failed_terminal() {
    for (provider, mode) in shared_driver_cases() {
        let context = format!("{}-{}", provider.label(), approval_mode_label(mode));
        let script = mock_provider_script(
            &format!("shared-failed-{context}"),
            r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-shared","model":"mock"}'
printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-shared","is_error":true,"errors":["provider failed"]}'"#,
        );
        let handle = provider.start(&script, mode, &format!("shared-failed-{context}"));
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert_single_terminal(&events, ExpectedTerminal::Failed, &context);
        let _ = fs::remove_file(script);
    }
}

#[test]
fn provider_drivers_nonzero_exit_emits_one_failed_terminal() {
    for (provider, mode) in shared_driver_cases() {
        let context = format!("{}-{}", provider.label(), approval_mode_label(mode));
        let script = mock_provider_script(
            &format!("shared-nonzero-{context}"),
            r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-shared","model":"mock"}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-shared","is_error":false,"result":"must be replaced"}'
printf '%s\n' 'provider exited seven' >&2
exit 7"#,
        );
        let handle = provider.start(&script, mode, &format!("shared-nonzero-{context}"));
        let events = collect_events_until_finished(&handle, Duration::from_secs(3));

        assert_single_terminal(&events, ExpectedTerminal::Failed, &context);
        assert!(
            events.iter().any(
                |event| matches!(event, AgentEvent::AgentFailed { error, .. }
                    if error == "provider exited seven")
            ),
            "{context} did not preserve the nonzero exit failure: {events:?}"
        );
        let _ = fs::remove_file(script);
    }
}

struct ProviderProcessCleanup {
    script: PathBuf,
    leader_pid_file: PathBuf,
    descendant_pid_file: PathBuf,
    armed: bool,
}

impl ProviderProcessCleanup {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProviderProcessCleanup {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(pid) = fs::read_to_string(&self.leader_pid_file) {
                if let Ok(pid) = pid.trim().parse::<i32>() {
                    unsafe {
                        let _ = nix::libc::kill(-pid, nix::libc::SIGKILL);
                        let _ = nix::libc::kill(pid, nix::libc::SIGKILL);
                    }
                }
            }
            if let Ok(pid) = fs::read_to_string(&self.descendant_pid_file) {
                if let Ok(pid) = pid.trim().parse::<i32>() {
                    unsafe {
                        let _ = nix::libc::kill(pid, nix::libc::SIGKILL);
                    }
                }
            }
        }
        let _ = fs::remove_file(&self.script);
        let _ = fs::remove_file(&self.leader_pid_file);
        let _ = fs::remove_file(&self.descendant_pid_file);
    }
}

fn wait_for_pid_files(paths: &[&Path]) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if paths.iter().all(|path| {
            fs::read_to_string(path)
                .ok()
                .and_then(|pid| pid.trim().parse::<i32>().ok())
                .is_some_and(|pid| pid > 1)
        }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("provider did not record process ids: {paths:?}");
}

#[test]
fn provider_drivers_cancel_emit_one_cancelled_terminal() {
    for (provider, mode) in shared_driver_cases() {
        let context = format!("{}-{}", provider.label(), approval_mode_label(mode));
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let leader_pid_file = std::env::temp_dir().join(format!(
            "cosh-shared-driver-leader-{}-{nonce}",
            std::process::id()
        ));
        let descendant_pid_file = std::env::temp_dir().join(format!(
            "cosh-shared-driver-descendant-{}-{nonce}",
            std::process::id()
        ));
        let script = mock_provider_script(
            &format!("shared-cancel-{context}"),
            &format!(
                r#"printf '%s\n' "$$" > "{}"
trap '' TERM
sh -c 'trap "" TERM; while :; do sleep 1; done' &
printf '%s\n' "$!" > "{}"
wait"#,
                leader_pid_file.display(),
                descendant_pid_file.display()
            ),
        );
        let mut cleanup = ProviderProcessCleanup {
            script: script.clone(),
            leader_pid_file: leader_pid_file.clone(),
            descendant_pid_file: descendant_pid_file.clone(),
            armed: true,
        };
        let handle = provider.start(&script, mode, &format!("shared-cancel-{context}"));
        wait_for_pid_files(&[&leader_pid_file, &descendant_pid_file]);

        handle.cancel();
        let events = collect_events_until_finished(&handle, Duration::from_secs(5));

        assert_single_terminal(&events, ExpectedTerminal::Cancelled, &context);
        assert_recorded_process_is_gone(&leader_pid_file);
        assert_recorded_process_is_not_running(&descendant_pid_file);
        cleanup.disarm();
        drop(cleanup);
    }
}
