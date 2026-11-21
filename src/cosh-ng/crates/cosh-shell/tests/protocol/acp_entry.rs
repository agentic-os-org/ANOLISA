//! Entry-selection and wire-isolation tests for the Shell ACP client.
//!
//! These cover the H2-4 boundary: the entry is chosen by name before any side
//! effect, selection never probes the peer with a real prompt, a failing ACP
//! run does not fall back to the legacy wire, and neither wire accepts the
//! other's frames.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cosh_shell::adapter::{
    adapter_for_kind, AcpAgentAdapter, AdapterInstance, AdapterKind, AgentAdapter, AgentRunPoll,
};
use cosh_shell::types::{
    AgentEvent, AgentMode, AgentRequest, CommandBlock, CommandStatus, CoshApprovalMode, OutputRefs,
};

pub(crate) fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos()
}

pub(crate) fn agent_script(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cosh-acp-entry-{name}-{}-{}.sh",
        std::process::id(),
        nonce()
    ));
    fs::write(&path, format!("#!/bin/bash\n{body}\n")).expect("write acp entry fixture");
    let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod acp entry fixture");
    path
}

pub(crate) fn make_request(id: &str) -> AgentRequest {
    let cwd = fs::canonicalize(std::env::temp_dir())
        .expect("canonical test workspace")
        .to_string_lossy()
        .into_owned();
    AgentRequest {
        id: id.to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "echo test".to_string(),
            origin: Default::default(),
            cwd: cwd.clone(),
            end_cwd: cwd,
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
        user_input: Some("explain the failing command".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

fn acp_instance(program: &Path) -> AdapterInstance {
    let mut acp = AcpAgentAdapter::new(program.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    AdapterInstance::Acp(acp.with_model_call(true))
}

#[test]
fn acp_entry_is_named_without_disturbing_legacy_aliases() {
    assert_eq!(AdapterKind::parse("acp"), Some(AdapterKind::Acp));
    assert_eq!(AdapterKind::parse("cosh-core"), Some(AdapterKind::CoshCore));
    assert_eq!(AdapterKind::parse("core"), Some(AdapterKind::CoshCore));
    assert_eq!(AdapterKind::parse("claude"), Some(AdapterKind::ClaudeCode));
    assert_eq!(AdapterKind::parse("co"), Some(AdapterKind::QwenCli));
    assert_eq!(AdapterKind::parse("fake"), Some(AdapterKind::Fake));
    assert_eq!(AdapterKind::parse("acp-core"), None);
    assert_eq!(AdapterKind::parse(""), None);
    assert_eq!(adapter_for_kind(AdapterKind::Acp).name(), "acp");
    assert_eq!(adapter_for_kind(AdapterKind::CoshCore).name(), "cosh-core");
}

#[test]
fn selecting_and_inspecting_the_acp_entry_never_starts_the_agent() {
    let marker = std::env::temp_dir().join(format!("cosh-acp-entry-probe-{}.txt", nonce()));
    let script = agent_script(
        "probe",
        &format!("printf 'started\\n' > '{}'\nexit 0\n", marker.display()),
    );
    let adapter = acp_instance(&script);

    assert!(adapter.capabilities().text_stream);
    // C1b: the ACP entry resumes via session/load, so resume is advertised;
    // inspecting capabilities still must not start the agent.
    assert!(adapter.capabilities().session_resume);
    assert_eq!(
        adapter.provider_invocation(),
        Some(script.to_string_lossy().into_owned())
    );
    assert_eq!(adapter.committed_session_id(), None);

    assert!(
        !marker.exists(),
        "selecting or inspecting the entry must not start the agent"
    );
    let _ = fs::remove_file(&script);
}

#[test]
fn acp_entry_without_a_program_fails_before_starting_anything() {
    let adapter = AdapterInstance::Acp(AcpAgentAdapter::default().with_model_call(true));
    let mut events = Vec::new();
    let error = adapter
        .run_stream(&make_request("run-acp-no-program"), &mut |event| {
            events.push(event);
            Ok(())
        })
        .expect_err("an unset agent program must fail explicitly");
    assert!(
        error.message.contains("agent program"),
        "unexpected error: {error:?}"
    );
    assert!(events.is_empty(), "no fact may be invented: {events:?}");
}

#[test]
fn legacy_jsonl_peer_fails_the_acp_entry_without_falling_back() {
    let script = agent_script(
        "legacy-wire",
        concat!(
            "printf '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"legacy-1\"}\\n'\n",
            "printf '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"legacy text\"}]}}\\n'\n",
            "printf '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"legacy done\"}\\n'\n",
            "exit 0\n",
        ),
    );
    let adapter = acp_instance(&script);
    let mut events = Vec::new();
    let result = adapter.run_stream(&make_request("run-acp-legacy-wire"), &mut |event| {
        events.push(event);
        Ok(())
    });
    let _ = fs::remove_file(&script);

    assert!(
        result.is_err(),
        "legacy JSONL must not be accepted as ACP: {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::TextDelta { .. } | AgentEvent::AgentCompleted { .. }
        )),
        "the legacy wire must not be projected into ACP facts: {events:?}"
    );
}

fn cancel_fixture(marker: &Path) -> PathBuf {
    agent_script(
        "cancel",
        &format!(
            r#"
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
IFS= read -r init
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{}}}}}}\n' "$(req_id "$init")"
IFS= read -r new_session
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"mock-session"}}}}\n' "$(req_id "$new_session")"
IFS= read -r next_frame
case "$next_frame" in
  *'"method":"session/cancel"'*)
    printf 'cancelled\n' > '{marker}'
    sleep 8
    ;;
  *'"method":"session/prompt"'*)
    prompt_id=$(req_id "$next_frame")
    printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"mock-session","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"working"}}}}}}}}\n'
    IFS= read -r cancel
    case "$cancel" in
      *'"method":"session/cancel"'*) printf 'cancelled\n' > '{marker}' ;;
      *) echo "fixture: expected session/cancel, got $cancel" >&2; exit 1 ;;
    esac
    printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"cancelled"}}}}\n' "$prompt_id"
    ;;
  *) echo "fixture: expected session/prompt or session/cancel, got $next_frame" >&2; exit 1 ;;
esac
"#,
            marker = marker.display()
        ),
    )
}

#[test]
fn cancelling_the_acp_entry_reaches_the_wire() {
    let marker = std::env::temp_dir().join(format!("cosh-acp-entry-cancel-{}.txt", nonce()));
    let script = cancel_fixture(&marker);
    let handle = acp_instance(&script).start_cancellable(
        make_request("run-acp-entry-cancel"),
        CoshApprovalMode::Recommend,
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut saw_text = false;
    while std::time::Instant::now() < deadline && !saw_text {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(AgentEvent::TextDelta { .. })) => saw_text = true,
            Ok(AgentRunPoll::Event(_)) => {}
            Ok(AgentRunPoll::Timeout) => {}
            Ok(AgentRunPoll::Finished) => break,
            Err(error) => panic!("acp entry run failed before cancellation: {error:?}"),
        }
    }
    assert!(saw_text, "the peer must stream before cancellation");

    handle.cancel();

    let mut cancelled = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline && !cancelled {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(AgentEvent::AgentCancelled { .. })) => cancelled = true,
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => {}
            Ok(AgentRunPoll::Finished) => break,
            Err(error) => panic!("acp entry cancellation failed: {error:?}"),
        }
    }
    let _ = fs::remove_file(&script);
    let reached_wire = marker.exists();
    let _ = fs::remove_file(&marker);

    assert!(cancelled, "cancellation must produce a terminal fact");
    assert!(
        reached_wire,
        "cancellation must reach the agent as session/cancel, not only flip a local flag"
    );
}

#[test]
fn a_cancel_requested_before_the_run_starts_is_not_discarded() {
    let marker = std::env::temp_dir().join(format!("cosh-acp-entry-precancel-{}.txt", nonce()));
    let script = cancel_fixture(&marker);
    let mut acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    let acp = acp.with_model_call(true);
    acp.cancel_handle()
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let events = acp
        .run(&make_request("run-acp-entry-precancel"))
        .expect("a cancelled session still ends with a terminal fact");
    let _ = fs::remove_file(&script);
    let reached_wire = marker.exists();
    let _ = fs::remove_file(&marker);

    assert!(
        matches!(events.last(), Some(AgentEvent::AgentCancelled { .. })),
        "a pending cancel must not be cleared by the run: {events:?}"
    );
    assert!(reached_wire, "the pending cancel must reach session/cancel");
}
