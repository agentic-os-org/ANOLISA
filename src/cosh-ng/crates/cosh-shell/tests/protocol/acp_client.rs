//! Process-level tests for the Shell ACP v1 client against scripted Agents.
//!
//! The scripted peers are deterministic fixtures, not independent ACP
//! implementations; they prove client behavior, not broad interoperability.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cosh_shell::adapter::{
    AcpAgentAdapter, AcpPermissionDecision, AdapterError, AgentAdapter, FreshSessionOutcome,
};
use cosh_shell::types::{
    AgentEvent, AgentMode, AgentRequest, CommandBlock, CommandStatus, OutputRefs,
};

const HANDSHAKE: &str = r#"
req_id() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
IFS= read -r init
case "$init" in
  *'"protocolVersion":1'*) ;;
  *) echo "fixture: initialize did not request wire v1" >&2; exit 1 ;;
esac
case "$init" in
  *'"terminal":false'*) ;;
  *) echo "fixture: client advertised terminal capability" >&2; exit 1 ;;
esac
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"mock-acp","version":"0.0.1"}}}\n' "$(req_id "$init")"
IFS= read -r new_session
printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"mock-session"}}\n' "$(req_id "$new_session")"
IFS= read -r prompt
prompt_id=$(req_id "$prompt")
"#;

fn update(body: &str) -> String {
    format!(
        "printf '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"sessionId\":\"mock-session\",\"update\":{body}}}}}\n'\n"
    )
}

fn agent_script(name: &str, body: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "cosh-acp-client-{name}-{}-{nonce}.sh",
        std::process::id()
    ));
    fs::write(&path, format!("#!/bin/bash\n{body}\n")).expect("write acp fixture");
    let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod acp fixture");
    path
}

fn adapter(program: &Path) -> AcpAgentAdapter {
    let mut acp = AcpAgentAdapter::new(program.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    acp.with_model_call(true)
}

fn make_request(id: &str) -> AgentRequest {
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

fn text_of(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Runs one turn on a worker thread so a handshake that never converges is
/// reported as a bounded assertion failure instead of hanging the suite.
fn run_bounded(
    acp: AcpAgentAdapter,
    request: AgentRequest,
    limit: Duration,
) -> Result<Result<Vec<AgentEvent>, AdapterError>, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let outcome = acp.run(&request);
        let _ = sender.send(outcome);
    });
    match receiver.recv_timeout(limit) {
        Ok(outcome) => {
            let _ = worker.join();
            Ok(outcome)
        }
        Err(_) => Err(format!(
            "adapter did not converge within {}s",
            limit.as_secs()
        )),
    }
}

const INITIALIZE_ONLY: &str = r#"
req_id() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
IFS= read -r init
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"mock-acp","version":"0.0.1"}}}\n' "$(req_id "$init")"
IFS= read -r new_session
"#;

fn wait_for_marker(path: &Path, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_cancel_during_the_handshake_never_starts_a_prompt() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let parked = std::env::temp_dir().join(format!("cosh-acp-parked-{nonce}"));
    let release = std::env::temp_dir().join(format!("cosh-acp-release-{nonce}"));
    let observed = std::env::temp_dir().join(format!("cosh-acp-observed-{nonce}"));
    let body = format!(
        r#"
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
IFS= read -r init
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{}},"agentInfo":{{"name":"mock-acp","version":"0.0.1"}}}}}}\n' "$(req_id "$init")"
IFS= read -r new_session
printf 'parked\n' > '{parked}'
while [ ! -f '{release}' ]; do sleep 0.05; done
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"mock-session"}}}}\n' "$(req_id "$new_session")"
IFS= read -r next_frame
printf '%s\n' "$next_frame" > '{observed}'
sleep 2
"#,
        parked = parked.display(),
        release = release.display(),
        observed = observed.display(),
    );
    let script = agent_script("handshake-cancel-no-prompt", &body);
    let acp = adapter(&script);
    let handle = acp.cancel_handle();
    let request = make_request("run-acp-handshake-no-prompt");
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = sender.send(acp.run(&request));
    });
    assert!(
        wait_for_marker(&parked, Duration::from_secs(10)),
        "the fixture never parked inside the handshake"
    );
    handle.store(true, Ordering::SeqCst);
    fs::write(&release, b"go").expect("release the handshake reply");
    let outcome = receiver.recv_timeout(Duration::from_secs(15));
    let _ = worker.join();
    let next_frame = fs::read_to_string(&observed).unwrap_or_default();
    for path in [&parked, &release, &observed, &script] {
        let _ = fs::remove_file(path);
    }
    let events = outcome
        .expect("a cancelled handshake must converge")
        .expect("a cancelled handshake must not report an adapter error");
    assert!(
        !next_frame.contains("session/prompt"),
        "a cancelled run must not start a new user task: {next_frame}"
    );
    assert!(
        next_frame.contains("session/cancel"),
        "the pending cancel must still reach the wire: {next_frame}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::AgentCancelled { .. })),
        "a cancel before any prompt must end cancelled: {events:?}"
    );
}

#[test]
fn handshake_eof_during_initialize_converges_as_a_failure() {
    let script = agent_script("handshake-eof", "IFS= read -r init\nexit 0\n");
    let outcome = run_bounded(
        adapter(&script),
        make_request("run-acp-handshake-eof"),
        Duration::from_secs(10),
    );
    let _ = fs::remove_file(&script);
    let failure = outcome
        .expect("handshake EOF must converge, not spin")
        .expect_err("EOF before initialize must fail the run");
    assert!(failure.message.contains("closed"), "{}", failure.message);
}

#[test]
fn handshake_error_reply_to_session_new_converges_as_a_failure() {
    let body = format!(
        r#"{INITIALIZE_ONLY}
printf '{{"jsonrpc":"2.0","id":%s,"error":{{"code":-32603,"message":"fixture session refused"}}}}\n' "$(req_id "$new_session")"
sleep 30
"#
    );
    let script = agent_script("handshake-error", &body);
    let outcome = run_bounded(
        adapter(&script),
        make_request("run-acp-handshake-error"),
        Duration::from_secs(10),
    );
    let _ = fs::remove_file(&script);
    let failure = outcome
        .expect("a handshake error reply must converge, not wait for idle")
        .expect_err("an error reply to session/new must fail the run");
    assert!(
        failure.message.contains("fixture session refused"),
        "{}",
        failure.message
    );
}

#[test]
fn handshake_silence_is_cancellable_within_the_grace() {
    let body = format!("{INITIALIZE_ONLY}sleep 30\n");
    let script = agent_script("handshake-silent", &body);
    let acp = adapter(&script);
    let handle = acp.cancel_handle();
    let request = make_request("run-acp-handshake-cancel");
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let outcome = acp.run(&request);
        let _ = sender.send(outcome);
    });
    std::thread::sleep(Duration::from_millis(400));
    handle.store(true, Ordering::SeqCst);
    let outcome = receiver.recv_timeout(Duration::from_secs(8));
    let _ = fs::remove_file(&script);
    let failure = outcome
        .expect("cancelling a silent handshake must converge within the grace")
        .expect_err("a cancelled handshake must not report success");
    let _ = worker.join();
    assert!(failure.message.contains("cancel"), "{}", failure.message);
}

#[test]
fn standard_session_streams_text_tools_and_completes() {
    let body = format!(
        "{HANDSHAKE}{}{}{}{}{}printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"stopReason\":\"end_turn\"}}}}\n' \"$prompt_id\"\n",
        update(r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"reasoning"}}"#),
        update(r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello "}}"#),
        update(r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"world"}}"#),
        update(r#"{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Read file","status":"in_progress"}"#),
        update(r#"{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"file body"}}]}"#),
    );
    let script = agent_script("happy", &body);
    let events = adapter(&script)
        .run(&make_request("run-acp-happy"))
        .expect("standard session");
    let _ = fs::remove_file(&script);

    assert_eq!(text_of(&events), "hello world");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StatusChanged { phase, run_id, .. } if phase == "thinking" && run_id == "run-acp-happy"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCall { tool_id: Some(id), name, run_id, .. }
            if id == "tool-1" && name == "Read file" && run_id == "run-acp-happy"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCompleted { tool_id, status, .. } if tool_id == "tool-1" && status == "success"
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { run_id, .. }) if run_id == "run-acp-happy"
    ));
}

#[test]
fn permission_reverse_call_is_rejected_by_default_and_reported() {
    let body = format!(
        r#"{HANDSHAKE}
printf '{{"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{{"sessionId":"mock-session","toolCall":{{"toolCallId":"tool-9"}},"options":[{{"optionId":"allow","name":"Allow","kind":"allow_once"}},{{"optionId":"reject","name":"Reject","kind":"reject_once"}}]}}}}\n'
IFS= read -r reply
case "$reply" in
  *'"optionId":"reject"'*) ;;
  *) echo "fixture: expected reject selection, got $reply" >&2; exit 1 ;;
esac
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$prompt_id"
"#
    );
    let script = agent_script("permission", &body);
    let events = adapter(&script)
        .run(&make_request("run-acp-permission"))
        .expect("permission session");
    let _ = fs::remove_file(&script);

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StatusChanged { phase, message, .. }
            if phase == "permission" && message.contains("tool-9")
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn injected_policy_can_select_an_offered_allow_option() {
    let body = format!(
        r#"{HANDSHAKE}
printf '{{"jsonrpc":"2.0","id":91,"method":"session/request_permission","params":{{"sessionId":"mock-session","toolCall":{{"toolCallId":"tool-10"}},"options":[{{"optionId":"allow","name":"Allow","kind":"allow_once"}}]}}}}\n'
IFS= read -r reply
case "$reply" in
  *'"optionId":"allow"'*) ;;
  *) echo "fixture: expected allow selection, got $reply" >&2; exit 1 ;;
esac
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$prompt_id"
"#
    );
    let script = agent_script("permission-allow", &body);
    let events = adapter(&script)
        .with_permission_policy(Arc::new(|_| {
            AcpPermissionDecision::Select("allow".to_string())
        }))
        .run(&make_request("run-acp-allow"))
        .expect("allow session");
    let _ = fs::remove_file(&script);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn unsupported_reverse_method_is_answered_with_method_not_found() {
    let body = format!(
        r#"{HANDSHAKE}
printf '{{"jsonrpc":"2.0","id":92,"method":"fs/read_text_file","params":{{"sessionId":"mock-session","path":"/etc/hosts"}}}}\n'
IFS= read -r reply
case "$reply" in
  *'"code":-32601'*) ;;
  *) echo "fixture: expected method-not-found, got $reply" >&2; exit 1 ;;
esac
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$prompt_id"
"#
    );
    let script = agent_script("unsupported", &body);
    let events = adapter(&script)
        .run(&make_request("run-acp-unsupported"))
        .expect("unsupported-method session");
    let _ = fs::remove_file(&script);

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StatusChanged { phase, message, .. }
            if phase == "unsupported_method" && message.contains("fs/read_text_file")
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. })));
}

#[test]
fn cancellation_is_acknowledged_with_a_cancelled_stop_reason() {
    let body = format!(
        r#"{HANDSHAKE}
printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"mock-session","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"working"}}}}}}}}\n'
IFS= read -r cancel
case "$cancel" in
  *'"method":"session/cancel"'*) ;;
  *) echo "fixture: expected session/cancel, got $cancel" >&2; exit 1 ;;
esac
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"cancelled"}}}}\n' "$prompt_id"
"#
    );
    let script = agent_script("cancel", &body);
    let acp = adapter(&script);
    let handle = acp.cancel_handle();
    let request = make_request("run-acp-cancel");
    let worker = std::thread::spawn(move || acp.run(&request));
    std::thread::sleep(Duration::from_millis(400));
    handle.store(true, Ordering::SeqCst);
    let events = worker
        .join()
        .expect("cancel worker")
        .expect("cancelled session");
    let _ = fs::remove_file(&script);

    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCancelled { run_id, .. }) if run_id == "run-acp-cancel"
    ));
}

#[test]
fn unsupported_wire_version_fails_the_run() {
    let body = r#"
IFS= read -r init
printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":2}}\n'
sleep 5
"#;
    let script = agent_script("bad-version", body);
    let failure = adapter(&script)
        .run(&make_request("run-acp-version"))
        .expect_err("wire v2 must fail");
    let _ = fs::remove_file(&script);
    assert!(failure.message.contains("unsupported ACP wire version 2"));
}

#[test]
fn oversized_agent_line_fails_the_run() {
    let body = format!(
        r#"{HANDSHAKE}
printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"mock-session","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"%s"}}}}}}}}\n' "$(head -c 1100000 /dev/zero | tr '\0' 'x')"
sleep 5
"#
    );
    let script = agent_script("oversized", &body);
    let failure = adapter(&script)
        .run(&make_request("run-acp-oversized"))
        .expect_err("oversized line must fail");
    let _ = fs::remove_file(&script);
    assert!(
        failure.message.contains("byte limit"),
        "unexpected failure: {}",
        failure.message
    );
}

#[test]
fn foreign_session_update_fails_the_run() {
    let body = format!(
        "{HANDSHAKE}printf '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"sessionId\":\"other-session\",\"update\":{{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{{\"type\":\"text\",\"text\":\"x\"}}}}}}}}\n'\nsleep 5\n"
    );
    let script = agent_script("foreign-session", &body);
    let failure = adapter(&script)
        .run(&make_request("run-acp-foreign"))
        .expect_err("foreign session must fail");
    let _ = fs::remove_file(&script);
    assert!(failure.message.contains("bound to mock-session"));
}

#[test]
fn agent_eof_before_stop_reason_is_reported_as_failure() {
    let body = format!(
        "{HANDSHAKE}{}",
        update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"partial"}}"#
        )
    );
    let script = agent_script("eof", &body);
    let events = adapter(&script)
        .run(&make_request("run-acp-eof"))
        .expect("eof session reports a terminal event");
    let _ = fs::remove_file(&script);

    assert_eq!(text_of(&events), "partial");
    let Some(AgentEvent::AgentFailed { error_code, .. }) = events.last() else {
        panic!("expected AgentFailed, got {:?}", events.last());
    };
    assert_eq!(error_code.as_deref(), Some("acp_transport_closed"));
}

#[test]
fn model_call_admission_is_required_before_launching_an_agent() {
    let script = agent_script("unadmitted", "exit 0\n");
    let failure = AcpAgentAdapter::new(script.to_string_lossy().into_owned())
        .run(&make_request("run-acp-unadmitted"))
        .expect_err("admission must be explicit");
    let _ = fs::remove_file(&script);
    assert!(failure.message.contains("model-call admission"));
}

#[test]
fn relative_cwd_is_rejected_before_launching_an_agent() {
    let script = agent_script("relative-cwd", "exit 0\n");
    let mut request = make_request("run-acp-relative");
    request.command_block.cwd = "relative/path".to_string();
    let failure: AdapterError = adapter(&script)
        .run(&request)
        .expect_err("relative cwd must be rejected");
    let _ = fs::remove_file(&script);
    assert!(failure.message.contains("absolute cwd"));
}

/// L04: detaching drops the kept peer, reports the previous session id, and
/// clears the committed identity so the next run establishes a fresh session.
#[test]
fn fresh_session_detaches_the_kept_peer_and_clears_the_identity() {
    let body = format!(
        "{HANDSHAKE}printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"stopReason\":\"end_turn\"}}}}\n' \"$prompt_id\"\n"
    );
    let script = agent_script("detach-sid", &body);
    let acp = adapter(&script);
    let _events = acp
        .run(&make_request("run-acp-detach"))
        .expect("standard session");
    assert_eq!(acp.committed_session_id().as_deref(), Some("mock-session"));
    assert_eq!(
        acp.start_fresh_session(),
        FreshSessionOutcome::Detached {
            previous_session_id: Some("mock-session".to_string())
        }
    );
    assert!(
        acp.committed_session_id().is_none(),
        "detach must clear the kept peer identity"
    );
    let _ = fs::remove_file(&script);
}

/// P1 (section 83 follow-up): a read-only identity query from a cloned adapter
/// must return while a gated turn still holds the whole-turn peer lock, not
/// wait for the turn to end. The turn is gated on a file the test releases only
/// after the concurrent query returns.
#[test]
fn committed_session_id_read_does_not_wait_on_the_whole_turn_lock() {
    let gate = std::env::temp_dir().join(format!(
        "cosh-acp-gate-{}-{}.flag",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_file(&gate);
    let body = format!(
        "{HANDSHAKE}{}while [ ! -f '{gate}' ]; do sleep 0.05; done\nprintf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"stopReason\":\"end_turn\"}}}}\n' \"$prompt_id\"\n",
        update(r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"working"}}"#),
        gate = gate.display(),
    );
    let script = agent_script("gated-turn", &body);
    let acp = adapter(&script);
    let reader = acp.clone();
    let request = make_request("run-acp-gated");
    let worker = std::thread::spawn(move || acp.run(&request));
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut established = None;
    while std::time::Instant::now() < deadline {
        if let Some(id) = reader.committed_session_id() {
            established = Some(id);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        established.as_deref(),
        Some("mock-session"),
        "the session must establish while the turn is gated"
    );
    let t0 = std::time::Instant::now();
    let id = reader.committed_session_id();
    let elapsed = t0.elapsed();
    assert_eq!(id.as_deref(), Some("mock-session"));
    assert!(
        elapsed < Duration::from_secs(1),
        "the identity read must not block on the whole-turn lock, took {elapsed:?}"
    );
    assert!(
        !gate.exists(),
        "the turn must still be gated when the query returns"
    );
    fs::write(&gate, b"go").unwrap();
    let outcome = worker.join().expect("worker");
    let _ = fs::remove_file(&gate);
    let _ = fs::remove_file(&script);
    let events = outcome.expect("the gated turn completes after release");
    assert!(text_of(&events).contains("working"));
}

/// L04: once a session is established the adapter exposes its committed
/// provider session id, so the Shell can resume via session/load instead of
/// treating the ACP path as having no recoverable identity.
#[test]
fn committed_session_id_reflects_the_established_session() {
    let body = format!(
        "{HANDSHAKE}printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"stopReason\":\"end_turn\"}}}}\n' \"$prompt_id\"\n"
    );
    let script = agent_script("committed-sid", &body);
    let acp = adapter(&script);
    assert!(
        acp.committed_session_id().is_none(),
        "no session before a run"
    );
    let _events = acp
        .run(&make_request("run-acp-committed"))
        .expect("standard session");
    assert_eq!(
        acp.committed_session_id().as_deref(),
        Some("mock-session"),
        "the established session id must be exposed for resume"
    );
    let _ = fs::remove_file(&script);
}

/// Runs against a non-Cosh Agent built on the official TypeScript SDK.
///
/// Ignored by default because it needs an external Node peer; set
/// `COSH_ACP_PEER_PROGRAM` and `COSH_ACP_PEER_SCRIPT` and run with `--ignored`.
#[test]
#[ignore = "requires the official TypeScript SDK peer process"]
fn standard_session_with_the_official_typescript_sdk_peer() {
    let program = std::env::var("COSH_ACP_PEER_PROGRAM").expect("COSH_ACP_PEER_PROGRAM");
    let script = std::env::var("COSH_ACP_PEER_SCRIPT").expect("COSH_ACP_PEER_SCRIPT");
    let workspace = std::env::temp_dir().join(format!(
        "cosh-acp-g2-peer-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    fs::create_dir_all(&workspace).expect("create peer workspace");
    fs::write(
        workspace.join("cosh-acp-peer-marker.txt"),
        "peer-marker-content\n",
    )
    .expect("write peer marker");

    let mut request = make_request("run-acp-g2-peer");
    request.command_block.cwd = workspace.to_string_lossy().into_owned();
    request.command_block.end_cwd = request.command_block.cwd.clone();

    let mut rejected = AcpAgentAdapter::new(program.clone());
    rejected.args = vec![script.clone()];
    rejected.idle_timeout = Duration::from_secs(30);
    let events = rejected
        .with_model_call(true)
        .run(&request)
        .expect("standard session with default reject policy");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCompleted { tool_id, status, .. }
                if tool_id == "peer-tool-1" && status == "error"
        )),
        "reject run events: {events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { run_id, .. }) if run_id == "run-acp-g2-peer"
    ));
    assert!(!text_of(&events).contains("peer-marker-content"));

    let mut allowed = AcpAgentAdapter::new(program.clone());
    allowed.args = vec![script.clone()];
    allowed.idle_timeout = Duration::from_secs(30);
    let events = allowed
        .with_model_call(true)
        .with_permission_policy(Arc::new(|_| {
            AcpPermissionDecision::Select("allow-once".to_string())
        }))
        .run(&request)
        .expect("standard session with an explicit allow decision");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolOutputDelta { tool_id, text, .. }
                if tool_id == "peer-tool-1" && text.contains("peer-marker-content")
        )),
        "allow run events: {events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { run_id, .. }) if run_id == "run-acp-g2-peer"
    ));

    // G4 frame-safety matrix: the same hostile frames must be refused safely
    // by the Shell when they come from an independent SDK peer, not only from
    // a scripted fixture. No extra action may run and the error must be exact.
    // A duplicate inbound tool_call_update must not create an extra action or
    // a second terminal. Projection-level duplication of ToolOutputDelta and
    // ToolCompleted is a registered rendering gap (A40), not asserted away
    // here; the invariants below are the safety contract.
    {
        let mut dup = AcpAgentAdapter::new(program.clone());
        dup.args = vec![script.clone(), "duplicate-tool".to_string()];
        dup.idle_timeout = Duration::from_secs(30);
        let events = dup
            .with_model_call(true)
            .run(&request)
            .expect("a duplicate status update is not a fatal frame");
        let terminals = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::AgentCompleted { .. }))
            .count();
        assert_eq!(
            terminals, 1,
            "duplicate frames must keep one terminal: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. })),
            "a duplicate status must not raise a new approval: {events:?}"
        );
        assert!(
            events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::ToolCompleted { tool_id, .. } => Some(tool_id.clone()),
                    _ => None,
                })
                .all(|tool_id| tool_id == "peer-tool-1"),
            "a duplicate frame must not invent a second tool identity: {events:?}"
        );
        let completions = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCompleted { .. }))
            .count();
        assert_eq!(
            completions, 1,
            "A40: an exact-duplicate update must not render a second completion: {events:?}"
        );
        let outputs = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolOutputDelta { .. }))
            .count();
        assert_eq!(
            outputs, 1,
            "A40: an exact-duplicate update must not replay its content: {events:?}"
        );
    }
    // A legitimate status progression carries distinct fingerprints and must
    // not be swallowed by the exact-duplicate suppression.
    {
        let mut prog = AcpAgentAdapter::new(program.clone());
        prog.args = vec![script.clone(), "progression".to_string()];
        prog.idle_timeout = Duration::from_secs(30);
        let events = prog
            .with_model_call(true)
            .run(&request)
            .expect("a legitimate progression must complete");
        let completions = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCompleted { .. }))
            .count();
        assert_eq!(completions, 1, "progression completion lost: {events:?}");
        let statuses = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentEvent::StatusChanged { phase, message, .. }
                        if phase == "tool_status" && message.contains("peer-tool-1")
                )
            })
            .count();
        assert_eq!(
            statuses, 2,
            "pending and in_progress must both survive suppression: {events:?}"
        );
    }
    for (mode, expected) in [
        ("oversized", "byte limit"),
        ("malformed", "malformed inbound JSON-RPC frame"),
        ("foreign-session", "bound to"),
    ] {
        let mut hostile = AcpAgentAdapter::new(program.clone());
        hostile.args = vec![script.clone(), mode.to_string()];
        hostile.idle_timeout = Duration::from_secs(30);
        let failure = hostile
            .with_model_call(true)
            .run(&request)
            .expect_err(&format!("{mode} frame must fail the run"));
        assert!(
            failure.message.contains(expected),
            "{mode}: unexpected failure: {}",
            failure.message
        );
    }

    let _ = fs::remove_dir_all(&workspace);
}
