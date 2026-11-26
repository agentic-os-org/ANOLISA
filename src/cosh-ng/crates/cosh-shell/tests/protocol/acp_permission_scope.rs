//! Wire-level scope of the standard ACP permission surface in the Shell client.
//!
//! The peers here are deterministic fixtures, not independent ACP agents: they
//! pin what the product answers on the wire and what it must never answer with.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cosh_shell::adapter::{AcpAgentAdapter, AcpHostRefusal, AcpPermissionDecision, AgentAdapter};
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
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{},"agentInfo":{"name":"mock-acp","version":"0.0.1"}}}\n' "$(req_id "$init")"
IFS= read -r new_session
printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"mock-session"}}\n' "$(req_id "$new_session")"
IFS= read -r prompt
prompt_id=$(req_id "$prompt")
"#;

const ALLOW_ONCE: &str = r#"{"optionId":"allow","name":"Allow","kind":"allow_once"}"#;
const ALLOW_ALWAYS: &str =
    r#"{"optionId":"allow-always","name":"Allow always","kind":"allow_always"}"#;
const REJECT_ONCE: &str = r#"{"optionId":"reject","name":"Reject","kind":"reject_once"}"#;
const REJECT_ALWAYS: &str =
    r#"{"optionId":"reject-always","name":"Reject always","kind":"reject_always"}"#;

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos()
}

/// One peer that offers `options` for `tool_call`, records the reply verbatim
/// and then ends the turn. The reply goes through /bin/cat so the shell's own
/// stdout buffer cannot leak into the capture.
fn peer(name: &str, tool_call: &str, options: &[&str], capture: &Path) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cosh-acp-permission-{name}-{}-{}.sh",
        std::process::id(),
        nonce()
    ));
    let body = format!(
        r#"{HANDSHAKE}
printf '{{"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{{"sessionId":"mock-session","toolCall":{tool_call},"options":[{options}]}}}}\n'
IFS= read -r reply
printf '%s\n' "$reply" | /bin/cat >> '{capture}'
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$prompt_id"
"#,
        tool_call = tool_call,
        options = options.join(","),
        capture = capture.display(),
    );
    fs::write(&path, format!("#!/bin/bash\n{body}\n")).expect("write permission fixture");
    let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod permission fixture");
    path
}

fn capture_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "cosh-acp-permission-{name}-{}-{}.jsonl",
        std::process::id(),
        nonce()
    ))
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

/// Runs one turn on a worker thread so a peer that never converges is reported
/// as a bounded assertion failure instead of hanging the suite.
fn run_bounded(
    acp: AcpAgentAdapter,
    request: AgentRequest,
    limit: Duration,
) -> Result<Vec<AgentEvent>, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = sender.send(acp.run(&request));
    });
    match receiver.recv_timeout(limit) {
        Ok(Ok(events)) => {
            let _ = worker.join();
            Ok(events)
        }
        Ok(Err(error)) => {
            let _ = worker.join();
            Err(format!("adapter failed: {error:?}"))
        }
        Err(_) => Err(format!(
            "adapter did not converge within {}s",
            limit.as_secs()
        )),
    }
}

fn reply_json(capture: &Path) -> serde_json::Value {
    let text = fs::read_to_string(capture).expect("permission reply capture");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one permission reply, got {text}"
    );
    serde_json::from_str(lines[0]).expect("permission reply is json")
}

fn outcome_of(reply: &serde_json::Value) -> &serde_json::Value {
    &reply["result"]["outcome"]
}

fn assert_cancelled(reply: &serde_json::Value) {
    assert_eq!(
        outcome_of(reply)["outcome"],
        "cancelled",
        "a fail-closed answer must be cancelled, got {reply}"
    );
    assert!(
        outcome_of(reply).get("optionId").is_none(),
        "a cancelled outcome must not carry an option, got {reply}"
    );
}

fn assert_selected(reply: &serde_json::Value, option_id: &str) {
    assert_eq!(outcome_of(reply)["outcome"], "selected", "got {reply}");
    assert_eq!(outcome_of(reply)["optionId"], option_id, "got {reply}");
}

fn tool_call(id: &str) -> String {
    format!(r#"{{"toolCallId":"{id}"}}"#)
}

/// One offered-options case end to end: run the turn, read the wire reply.
fn answer(
    name: &str,
    options: &[&str],
    policy: Option<AcpPermissionDecision>,
) -> (serde_json::Value, Vec<AgentEvent>) {
    let capture = capture_path(name);
    let script = peer(name, &tool_call("tool-scope"), options, &capture);
    let mut acp = adapter(&script);
    if let Some(decision) = policy {
        acp = acp.with_permission_policy(Arc::new(move |_| decision.clone()));
    }
    let events = run_bounded(
        acp,
        make_request(&format!("run-permission-{name}")),
        Duration::from_secs(30),
    )
    .expect("permission turn");
    let reply = reply_json(&capture);
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
    (reply, events)
}

#[test]
fn the_default_refusal_prefers_the_once_option_over_a_remembered_refusal() {
    let (reply, events) = answer(
        "prefer-once",
        &[REJECT_ALWAYS, REJECT_ONCE, ALLOW_ONCE],
        None,
    );
    assert_selected(&reply, "reject");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StatusChanged { phase, .. } if phase == "permission"
    )));
}

#[test]
fn a_remembered_refusal_is_never_selected_on_the_users_behalf() {
    let (reply, _) = answer("remembered-refusal", &[REJECT_ALWAYS, ALLOW_ONCE], None);
    assert_cancelled(&reply);
}

#[test]
fn an_allow_only_request_is_cancelled_rather_than_granted() {
    let (reply, _) = answer("allow-only", &[ALLOW_ONCE, ALLOW_ALWAYS], None);
    assert_cancelled(&reply);
}

#[test]
fn a_request_without_options_is_cancelled() {
    let (reply, _) = answer("no-options", &[], None);
    assert_cancelled(&reply);
}

#[test]
fn an_unoffered_option_from_an_injected_policy_is_cancelled_on_the_wire() {
    let (reply, _) = answer(
        "unoffered",
        &[ALLOW_ONCE, REJECT_ONCE],
        Some(AcpPermissionDecision::Select("not-offered".to_string())),
    );
    assert_cancelled(&reply);
}

#[test]
fn a_cancelled_decision_is_reported_as_cancelled_on_the_wire() {
    let (reply, _) = answer(
        "decision-cancel",
        &[ALLOW_ONCE, REJECT_ONCE],
        Some(AcpPermissionDecision::Cancel),
    );
    assert_cancelled(&reply);
}

#[test]
fn the_permission_reply_carries_only_the_standard_outcome_fields() {
    let (reply, _) = answer("reply-shape", &[ALLOW_ONCE, REJECT_ONCE], None);
    let object = reply.as_object().expect("reply is an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["id", "jsonrpc", "result"], "got {reply}");
    let result = reply["result"].as_object().expect("result is an object");
    let mut result_keys: Vec<&str> = result.keys().map(String::as_str).collect();
    result_keys.sort_unstable();
    assert_eq!(result_keys, ["outcome"], "got {reply}");
    let outcome = outcome_of(&reply)
        .as_object()
        .expect("outcome is an object");
    let mut outcome_keys: Vec<&str> = outcome.keys().map(String::as_str).collect();
    outcome_keys.sort_unstable();
    assert_eq!(outcome_keys, ["optionId", "outcome"], "got {reply}");
    assert_eq!(reply["id"], 90);
}

#[test]
fn a_shell_looking_permission_never_reaches_the_foreground_authority() {
    let capture = capture_path("shell-looking");
    let shell_tool_call = r#"{"toolCallId":"tool-shell","kind":"execute","title":"rm -rf /tmp/fixture","rawInput":{"command":"rm -rf /tmp/fixture"}}"#;
    let script = peer(
        "shell-looking",
        shell_tool_call,
        &[ALLOW_ONCE, REJECT_ONCE],
        &capture,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let acp = adapter(&script).with_host_authority(
        Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Err(AcpHostRefusal::new(
                -32011,
                "fixture: a standard permission must not reach the foreground authority",
            ))
        }),
        "pty-fixture",
    );
    let events = run_bounded(
        acp,
        make_request("run-permission-shell-looking"),
        Duration::from_secs(30),
    )
    .expect("shell-looking permission turn");
    let reply = reply_json(&capture);
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);

    assert_selected(&reply, "reject");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the foreground authority must not be invoked for a standard permission"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolPermissionRequest { .. })),
        "a standard permission must not become a Shell command approval card"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StatusChanged { phase, message, .. }
            if phase == "permission" && message.contains("tool-shell")
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}
