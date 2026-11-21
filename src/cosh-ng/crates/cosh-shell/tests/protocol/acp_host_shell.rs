//! Process-level tests for the host-shell v1 extension on the Shell ACP client.
//!
//! The scripted peer is a deterministic fixture, not a real Agent. These tests
//! cover negotiation, field validation, the mandatory-ask invariant and single
//! dispatch; they do not claim foreground PTY fidelity, which needs a real PTY.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cosh_shell::adapter::{
    AcpAgentAdapter, AcpHostDecision, AcpHostExecuteOutcome, AcpHostExecuteRequest,
    AdapterInstance, AgentAdapter, AgentRunPoll, ApprovalDecision, ApprovalResponse,
    HostExecutedShellMetadata, HostExecutedShellResult,
};
use cosh_shell::types::{AgentEvent, CoshApprovalMode};

use super::acp_entry::{agent_script, make_request, nonce};

const EXECUTE: &str = "_cosh/shell/v1/execute";
const OWNED: &str = "_cosh/shell/v1/owned";

/// Peer that answers the handshake, sends the supplied reverse requests, then
/// records every line it reads back before ending the turn.
fn peer_script(
    name: &str,
    requests: &[String],
    expect_lines: usize,
    want_capability: bool,
) -> (PathBuf, PathBuf) {
    peer_script_with_finish(
        name,
        requests,
        expect_lines,
        want_capability,
        &stop_reply("end_turn"),
    )
}

/// The peer's final line: a prompt result carrying the given stop reason.
fn stop_reply(reason: &str) -> String {
    format!(
        "printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"stopReason\":\"{}\"}}}}\\n' \"$prompt_id\"",
        reason
    )
}

/// Same peer, with an explicit finishing shell snippet so a cancelled turn can
/// be answered the way the protocol expects, or left unanswered on purpose.
fn peer_script_with_finish(
    name: &str,
    requests: &[String],
    expect_lines: usize,
    want_capability: bool,
    finish: &str,
) -> (PathBuf, PathBuf) {
    let capture = std::env::temp_dir().join(format!("cosh-host-shell-{name}-{}.jsonl", nonce()));
    let sends = requests
        .iter()
        .map(|request| format!("printf '%s\\n' '{request}'\n"))
        .collect::<String>();
    let body = format!(
        r#"
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
IFS= read -r init
case "$init" in
  *'"_cosh/shell"'*) present=1 ;;
  *) present=0 ;;
esac
if [ "$present" != "{want}" ]; then
  echo "fixture: capability advertisement mismatch (want {want}, got $present)" >&2
  exit 1
fi
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{}}}}}}\n' "$(req_id "$init")"
IFS= read -r new_session
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"host-session"}}}}\n' "$(req_id "$new_session")"
IFS= read -r prompt
prompt_id=$(req_id "$prompt")
{sends}for _ in $(seq 1 {expect_lines}); do
  IFS= read -r -t 10 line || break
  printf '%s\n' "$line" >> '{capture}'
done
{finish}
"#,
        want = if want_capability { 1 } else { 0 },
        sends = sends,
        capture = capture.display(),
        finish = finish,
    );
    (agent_script(name, &body), capture)
}

fn execute_request(id: u32, tool: &str, extra: &str) -> String {
    execute_request_for(id, tool, "pty-1", extra)
}

fn execute_request_for(id: u32, tool: &str, instance: &str, extra: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{EXECUTE}","params":{{"sessionId":"host-session","toolCallId":"{tool}","command":"echo hi","cwd":"/tmp","ptyInstance":"{instance}","hookRequiresApproval":false{extra}}}}}"#
    )
}

fn outcome() -> AcpHostExecuteOutcome {
    AcpHostExecuteOutcome {
        command: "echo hi".to_string(),
        status: "success".to_string(),
        exit_code: Some(0),
        signal: None,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        duration_ms: 7,
        output_ref: None,
        redaction_status: "none".to_string(),
        approval_id: None,
        tool_use_id: None,
        input_wait: None,
        llm_content: "hi\n".to_string(),
        llm_content_truncated: false,
    }
}

fn adapter(program: &Path, decision: AcpHostDecision, calls: Arc<AtomicUsize>) -> AcpAgentAdapter {
    let mut acp = AcpAgentAdapter::new(program.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    acp.with_model_call(true).with_host_policy(
        Arc::new(move |_| decision),
        Arc::new(move |request: &AcpHostExecuteRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            let mut result = outcome();
            result.command = request.command.clone();
            result
        }),
        "pty-1",
    )
}

fn replies(capture: &Path) -> Vec<serde_json::Value> {
    let text = fs::read_to_string(capture).unwrap_or_default();
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn run(acp: &AcpAgentAdapter, id: &str) -> Vec<AgentEvent> {
    acp.run(&make_request(id)).expect("host-shell session")
}

#[test]
fn capability_is_advertised_only_when_an_executor_is_wired() {
    let (script, capture) = peer_script("no-capability", &[], 0, false);
    let mut acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    let events = run(&acp.with_model_call(true), "run-host-no-capability");
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));

    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script("capability", &[], 0, true);
    let events = run(
        &adapter(&script, AcpHostDecision::Allow, Arc::clone(&calls)),
        "run-host-capability",
    );
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn execute_dispatches_once_after_owned_and_returns_the_real_result() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script("dispatch", &[execute_request(90, "tool-1", "")], 2, true);
    let events = run(
        &adapter(&script, AcpHostDecision::Allow, Arc::clone(&calls)),
        "run-host-dispatch",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one dispatch");
    assert_eq!(seen.len(), 2, "owned then response: {seen:?}");
    assert_eq!(seen[0]["method"], OWNED);
    assert_eq!(seen[0]["params"]["toolCallId"], "tool-1");
    assert!(seen[0].get("id").is_none(), "owned is a notification");
    assert_eq!(seen[1]["id"], 90);
    assert_eq!(seen[1]["result"]["exit_code"], 0);
    assert_eq!(seen[1]["result"]["end_cwd"], "/tmp");
    assert_eq!(seen[1]["result"]["llmContent"], "hi\n");
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn mandatory_ask_is_not_satisfied_by_policy_allow() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script(
        "must-ask",
        &[execute_request(
            91,
            "tool-1",
            r#","hookRequiresApproval":true"#,
        )],
        2,
        true,
    );
    let events = run(
        &adapter(&script, AcpHostDecision::Allow, Arc::clone(&calls)),
        "run-host-must-ask",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(calls.load(Ordering::SeqCst), 0, "mustAsk must not auto-run");
    assert_eq!(seen.len(), 2, "owned then refusal: {seen:?}");
    assert_eq!(seen[0]["method"], OWNED);
    assert_eq!(seen[1]["error"]["code"], -32010);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn mandatory_ask_with_a_human_decision_dispatches() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script(
        "human",
        &[execute_request(
            92,
            "tool-1",
            r#","hookRequiresApproval":true"#,
        )],
        2,
        true,
    );
    let events = run(
        &adapter(&script, AcpHostDecision::HumanAllow, Arc::clone(&calls)),
        "run-host-human",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0]["method"], OWNED);
    assert_eq!(seen[1]["result"]["status"], "success");
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn a_missing_mandatory_constraint_is_rejected() {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = r#"{"jsonrpc":"2.0","id":93,"method":"_cosh/shell/v1/execute","params":{"sessionId":"host-session","toolCallId":"tool-1","command":"echo hi","cwd":"/tmp","ptyInstance":"pty-1"}}"#.to_string();
    let (script, capture) = peer_script("missing", &[request], 1, true);
    let events = run(
        &adapter(&script, AcpHostDecision::HumanAllow, Arc::clone(&calls)),
        "run-host-missing",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(seen[0]["error"]["code"], -32602, "{seen:?}");
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn unknown_fields_and_relative_paths_are_rejected() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script(
        "unknown",
        &[execute_request(94, "tool-1", r#","allow":true"#)],
        1,
        true,
    );
    run(
        &adapter(&script, AcpHostDecision::HumanAllow, Arc::clone(&calls)),
        "run-host-unknown",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(seen[0]["error"]["code"], -32602, "{seen:?}");

    let (script, capture) = peer_script(
        "relative",
        &[execute_request(
            95,
            "tool-2",
            r#","expectedCwd":"relative/dir""#,
        )],
        1,
        true,
    );
    let events = run(
        &adapter(&script, AcpHostDecision::HumanAllow, Arc::clone(&calls)),
        "run-host-relative",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(seen[0]["error"]["code"], -32602, "{seen:?}");
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn a_duplicate_tool_call_id_is_not_dispatched_twice() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script(
        "duplicate",
        &[
            execute_request(96, "tool-1", ""),
            execute_request(97, "tool-1", ""),
        ],
        3,
        true,
    );
    let events = run(
        &adapter(&script, AcpHostDecision::Allow, Arc::clone(&calls)),
        "run-host-duplicate",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(calls.load(Ordering::SeqCst), 1, "one dispatch only");
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(seen[0]["method"], OWNED);
    assert_eq!(seen[1]["id"], 96);
    assert_eq!(seen[2]["error"]["code"], -32010);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn a_reused_tool_call_id_is_refused_on_the_next_turn_of_a_kept_connection() {
    let calls = Arc::new(AtomicUsize::new(0));
    let capture = std::env::temp_dir().join(format!("cosh-host-cross-turn-{}.jsonl", nonce()));
    // A persistent peer: one handshake, then a prompt loop. Every turn asks to
    // execute the SAME toolCallId, so the second turn probes whether the owned
    // set is held across turns on the kept connection (design 8.x: LivePeer
    // keeps transport/codec/owned across turns).
    let body = r##"
req_id() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
IFS= read -r init
case "$init" in
  *'"_cosh/shell"'*) present=1 ;;
  *) present=0 ;;
esac
if [ "$present" != 1 ]; then echo "fixture: capability advertisement mismatch" >&2; exit 1; fi
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{}}}\n' "$(req_id "$init")"
turn=0
while IFS= read -r -t 30 line; do
  case "$line" in
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"host-session"}}\n' "$(req_id "$line")"
      ;;
    *'"method":"session/prompt"'*)
      rid=$(req_id "$line")
      turn=$((turn+1))
      eid=$((100+turn))
      printf '{"jsonrpc":"2.0","id":%s,"method":"_cosh/shell/v1/execute","params":{"sessionId":"host-session","toolCallId":"tool-reuse","command":"echo hi","cwd":"/tmp","ptyInstance":"pty-1","hookRequiresApproval":false}}\n' "$eid"
      while IFS= read -r -t 10 hostline; do
        printf '%s\n' "$hostline" >> 'CAPTURE'
        case "$hostline" in
          *"\"id\":$eid"*) break ;;
        esac
      done
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$rid"
      ;;
  esac
done
"##
    .replace("CAPTURE", &capture.display().to_string());
    let script = agent_script("cross-turn", &body);
    let acp = adapter(&script, AcpHostDecision::Allow, Arc::clone(&calls));
    let first = run(&acp, "cross-turn-1");
    let second = run(&acp, "cross-turn-2");
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the reused id must not dispatch a second time"
    );
    let owned = seen.iter().filter(|frame| frame["method"] == OWNED).count();
    assert_eq!(owned, 1, "owned only on the first turn: {seen:?}");
    let first_reply = seen
        .iter()
        .find(|frame| frame["id"] == 101)
        .expect("turn 1 execute reply");
    assert_eq!(first_reply["result"]["status"], "success");
    let second_reply = seen
        .iter()
        .find(|frame| frame["id"] == 102)
        .expect("turn 2 execute reply");
    assert_eq!(
        second_reply["error"]["code"], -32010,
        "the kept connection must refuse the reused id: {second_reply:?}"
    );
    assert!(
        second_reply["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already owned"),
        "the refusal must be the owned dedup, not another constraint: {second_reply:?}"
    );
    assert!(matches!(
        first.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
    assert!(matches!(
        second.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn execute_without_a_negotiated_capability_is_refused() {
    let (script, capture) = peer_script(
        "unnegotiated",
        &[execute_request(98, "tool-1", "")],
        1,
        false,
    );
    let mut acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    let events = run(&acp.with_model_call(true), "run-host-unnegotiated");
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(seen[0]["error"]["code"], -32601, "{seen:?}");
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn ownership_transfers_before_the_local_decision_and_never_dispatches_a_refusal() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script("refused", &[execute_request(99, "tool-1", "")], 2, true);
    let events = run(
        &adapter(&script, AcpHostDecision::Refuse, Arc::clone(&calls)),
        "run-host-refused",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a refusal never dispatches"
    );
    assert_eq!(seen.len(), 2, "owned precedes the decision: {seen:?}");
    assert_eq!(seen[0]["method"], OWNED);
    assert_eq!(seen[0]["params"]["toolCallId"], "tool-1");
    assert!(seen[0].get("id").is_none(), "owned is a notification");
    assert_eq!(seen[1]["error"]["code"], -32011);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn a_stale_pty_instance_is_refused_without_dispatch() {
    let stale = r#"{"jsonrpc":"2.0","id":7,"method":"_cosh/shell/v1/execute","params":{"sessionId":"host-session","toolCallId":"call-stale","command":"echo hi","cwd":"/tmp","ptyInstance":"pty-other","hookRequiresApproval":false}}"#;
    let (script, capture) = peer_script("stale-pty", &[stale.to_string()], 2, true);
    let calls = Arc::new(AtomicUsize::new(0));
    run(
        &adapter(&script, AcpHostDecision::HumanAllow, Arc::clone(&calls)),
        "stale-pty",
    );
    let frames = replies(&capture);
    let error = frames
        .iter()
        .find(|frame| frame["id"] == 7)
        .expect("execute reply");
    assert_eq!(error["error"]["code"], -32010);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Drives the production entry: the runtime approval channel is the authority.
fn runtime_replies(
    name: &str,
    decide: impl FnOnce() -> ApprovalDecision + Send + 'static,
) -> (Vec<serde_json::Value>, bool) {
    // The runtime path advertises the Shell session identity from the request.
    let request = execute_request_for(7, "call-runtime", "session-1", "");
    let (script, capture) = peer_script(name, &[request], 2, true);
    let acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned()).with_model_call(true);
    let handle =
        AdapterInstance::Acp(acp).start_cancellable(make_request(name), CoshApprovalMode::Auto);
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let mut saw_ask = false;
    while std::time::Instant::now() < deadline {
        match handle.next_event_timeout(Duration::from_millis(200)) {
            Ok(Some(AgentEvent::ToolPermissionRequest {
                hook_requires_approval,
                ..
            })) => {
                assert!(!hook_requires_approval);
                saw_ask = true;
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => continue,
            Err(failure) => panic!("runtime run failed: {failure:?}"),
        }
    }
    if saw_ask {
        handle
            .respond_approval(ApprovalResponse {
                request_id: "call-runtime".to_string(),
                tool_use_id: Some("call-runtime".to_string()),
                tool_input: None,
                decision: decide(),
            })
            .expect("approval delivery");
    }
    while std::time::Instant::now() < deadline {
        match handle.next_event_timeout(Duration::from_millis(200)) {
            Ok(Some(_)) | Ok(None) => continue,
            Err(_) => break,
        }
    }
    (replies(&capture), saw_ask)
}

#[test]
fn cancelling_the_run_ends_the_pending_execute_as_unknown() {
    let request = execute_request_for(7, "call-cancel", "session-1", "");
    let (script, capture) = peer_script_with_finish(
        "runtime-cancel",
        &[request],
        3,
        true,
        &stop_reply("cancelled"),
    );
    // The run aborts as soon as the rejection is written, so the peer must
    // survive SIGTERM long enough to read the frame it is already waiting for.
    let peer = agent_script(
        "runtime-cancel-peer",
        &format!("trap '' TERM\nexec bash '{}'\n", script.display()),
    );
    let acp = AcpAgentAdapter::new(peer.to_string_lossy().into_owned()).with_model_call(true);
    let handle = AdapterInstance::Acp(acp)
        .start_cancellable(make_request("runtime-cancel"), CoshApprovalMode::Auto);
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let mut saw_ask = false;
    while std::time::Instant::now() < deadline {
        match handle.next_event_timeout(Duration::from_millis(200)) {
            Ok(Some(AgentEvent::ToolPermissionRequest { .. })) => {
                saw_ask = true;
                break;
            }
            Ok(Some(_)) | Ok(None) => continue,
            Err(failure) => panic!("runtime run failed: {failure:?}"),
        }
    }
    assert!(saw_ask, "the foreground ask must reach the runtime");
    handle.cancel();
    // The rejection and the cancel notification are written just before the run
    // ends, so wait for the peer to record them instead of racing the teardown.
    let mut seen = replies(&capture);
    while seen.len() < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        seen = replies(&capture);
    }
    assert_eq!(
        seen.len(),
        3,
        "owned, unknown, then session/cancel: {seen:?}"
    );
    assert_eq!(seen[0]["method"], OWNED);
    assert_eq!(seen[1]["error"]["code"], -32012);
    assert_eq!(seen[2]["method"], "session/cancel");
    assert_eq!(seen[2]["params"]["sessionId"], "host-session");
}

#[test]
fn an_unconfirmed_cancellation_still_ends_within_the_grace() {
    let request = execute_request_for(7, "call-stall", "session-1", "");
    let (script, capture) =
        peer_script_with_finish("runtime-cancel-stall", &[request], 3, true, "sleep 8");
    let peer = agent_script(
        "runtime-cancel-stall-peer",
        &format!("trap '' TERM\nexec bash '{}'\n", script.display()),
    );
    let acp = AcpAgentAdapter::new(peer.to_string_lossy().into_owned()).with_model_call(true);
    let handle = AdapterInstance::Acp(acp)
        .start_cancellable(make_request("runtime-cancel-stall"), CoshApprovalMode::Auto);
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let mut saw_ask = false;
    while std::time::Instant::now() < deadline {
        match handle.next_event_timeout(Duration::from_millis(200)) {
            Ok(Some(AgentEvent::ToolPermissionRequest { .. })) => {
                saw_ask = true;
                break;
            }
            Ok(Some(_)) | Ok(None) => continue,
            Err(failure) => panic!("runtime run failed: {failure:?}"),
        }
    }
    assert!(saw_ask, "the foreground ask must reach the runtime");
    let started = std::time::Instant::now();
    handle.cancel();
    let mut finished = false;
    while std::time::Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Finished) | Err(_) => {
                finished = true;
                break;
            }
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => continue,
        }
    }
    let elapsed = started.elapsed();
    assert!(finished, "the run must end instead of waiting forever");
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed <= Duration::from_secs(12),
        "the grace must bound the wait, took {elapsed:?}"
    );
    let seen = replies(&capture);
    assert_eq!(
        seen.len(),
        3,
        "owned, unknown, then session/cancel: {seen:?}"
    );
    assert_eq!(seen[2]["method"], "session/cancel");
}

fn runtime_result() -> HostExecutedShellResult {
    HostExecutedShellResult {
        llm_content: "hi\n".to_string(),
        return_display: None,
        metadata: HostExecutedShellMetadata {
            command: "echo hi".to_string(),
            status: "completed".to_string(),
            exit_code: 0,
            signal: None,
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            duration_ms: 7,
            output_ref: None,
            redaction_status: "none".to_string(),
            approval_id: Some("approval-1".to_string()),
            tool_use_id: Some("call-runtime".to_string()),
            input_wait: None,
        },
    }
}

#[test]
fn the_runtime_channel_delivers_the_real_foreground_result() {
    let (frames, saw_ask) = runtime_replies("runtime-ok", || ApprovalDecision::HostExecutedShell {
        result: Box::new(runtime_result()),
    });
    assert!(saw_ask, "the runtime must be asked before anything runs");
    let owned = frames
        .iter()
        .position(|frame| frame["method"] == OWNED)
        .expect("owned notification");
    let reply = frames
        .iter()
        .position(|frame| frame["id"] == 7)
        .expect("execute reply");
    assert!(owned < reply, "owned must precede the terminal reply");
    let result = &frames[reply]["result"];
    assert_eq!(result["status"], "completed");
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["llmContent"], "hi\n");
    assert_eq!(result["approval_id"], "approval-1");
}

#[test]
fn a_runtime_denial_refuses_the_action() {
    let (frames, saw_ask) = runtime_replies("runtime-deny", || ApprovalDecision::Deny {
        message: "the user refused".to_string(),
    });
    assert!(saw_ask);
    let reply = frames
        .iter()
        .find(|frame| frame["id"] == 7)
        .expect("execute reply");
    assert_eq!(reply["error"]["code"], -32011);
}

#[test]
fn a_runtime_native_allow_is_a_contract_violation() {
    let (frames, saw_ask) = runtime_replies("runtime-allow", || ApprovalDecision::Allow);
    assert!(saw_ask);
    let reply = frames
        .iter()
        .find(|frame| frame["id"] == 7)
        .expect("execute reply");
    assert_eq!(reply["error"]["code"], -32010);
}

#[test]
fn approval_decision_reaches_the_peer_reply_within_the_budget() {
    let request = execute_request_for(7, "call-latency", "session-1", "");
    let (script, capture) = peer_script("approval-latency", &[request], 2, true);
    let acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned()).with_model_call(true);
    let handle = AdapterInstance::Acp(acp)
        .start_cancellable(make_request("approval-latency"), CoshApprovalMode::Auto);
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let mut saw_ask = false;
    while std::time::Instant::now() < deadline {
        match handle.next_event_timeout(Duration::from_millis(200)) {
            Ok(Some(AgentEvent::ToolPermissionRequest { .. })) => {
                saw_ask = true;
                break;
            }
            Ok(Some(_)) | Ok(None) => continue,
            Err(failure) => panic!("runtime run failed: {failure:?}"),
        }
    }
    assert!(saw_ask, "the foreground ask must reach the runtime");
    let mut result = runtime_result();
    result.metadata.tool_use_id = Some("call-latency".to_string());
    let started = std::time::Instant::now();
    handle
        .respond_approval(ApprovalResponse {
            request_id: "call-latency".to_string(),
            tool_use_id: Some("call-latency".to_string()),
            tool_input: None,
            decision: ApprovalDecision::HostExecutedShell {
                result: Box::new(result),
            },
        })
        .expect("approval delivery");
    // The decision-to-reply boundary is internal to the process, so watch the
    // peer's capture for the execute reply instead of the event stream.
    let mut reply_ms = None;
    while std::time::Instant::now() < deadline {
        if replies(&capture).iter().any(|frame| frame["id"] == 7) {
            reply_ms = Some(started.elapsed().as_millis());
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let reply_ms = reply_ms.expect("the execute reply must reach the peer");
    eprintln!("APPROVAL-LATENCY approval_to_reply_ms={reply_ms}");
    let mut finished = false;
    while std::time::Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Finished) | Err(_) => {
                finished = true;
                break;
            }
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => continue,
        }
    }
    assert!(finished, "the run must end after the terminal reply");
    let seen = replies(&capture);
    let owned = seen
        .iter()
        .position(|frame| frame["method"] == OWNED)
        .expect("owned notification");
    let reply = seen
        .iter()
        .position(|frame| frame["id"] == 7)
        .expect("execute reply");
    assert!(owned < reply, "owned must precede the terminal reply");
    assert_eq!(seen[reply]["result"]["status"], "completed");
    assert!(
        reply_ms < 15_000,
        "the decision must reach the peer without a stall, took {reply_ms}ms"
    );
}

#[test]
fn eof_after_the_terminal_reply_is_not_reported_as_success() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script_with_finish(
        "eof-after-reply",
        &[execute_request(91, "tool-eof", "")],
        2,
        true,
        "",
    );
    let events = run(
        &adapter(&script, AcpHostDecision::Allow, Arc::clone(&calls)),
        "run-host-eof-after-reply",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one dispatch");
    assert_eq!(seen.len(), 2, "owned then response: {seen:?}");
    assert_eq!(seen[0]["method"], OWNED);
    assert_eq!(seen[1]["id"], 91);
    match events.last() {
        Some(AgentEvent::AgentFailed { error_code, .. }) => {
            assert_eq!(error_code.as_deref(), Some("acp_transport_closed"));
        }
        other => panic!("eof must not be reported as success: {other:?}"),
    }
}

#[test]
fn a_peer_vanishing_before_owned_fails_closed_without_replay() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (script, capture) = peer_script_with_finish(
        "eof-before-owned",
        &[execute_request(92, "tool-eof", "")],
        0,
        true,
        "",
    );
    let events = run(
        &adapter(&script, AcpHostDecision::Allow, Arc::clone(&calls)),
        "run-host-eof-before-owned",
    );
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    // Whether the owned write still reaches the pipe before the peer exits is
    // a race the Shell cannot win, so the guarantee is at most one dispatch and
    // a closed failure, never a success claim and never a replay.
    assert!(
        calls.load(Ordering::SeqCst) <= 1,
        "a vanished peer must not cause a replay: {seen:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::AgentFailed { .. })),
        "the run must fail closed: {:?}",
        events.last()
    );
}

#[test]
fn eof_during_a_pending_review_still_runs_locally_and_fails_the_reply() {
    let (script, capture) = peer_script_with_finish(
        "eof-during-review",
        &[execute_request(93, "tool-eof", "")],
        1,
        true,
        "",
    );
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned())
        .with_model_call(true)
        .with_host_authority(
            Arc::new(move |request: &AcpHostExecuteRequest| {
                counted.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).expect("test listener");
                release_rx
                    .lock()
                    .expect("release lock")
                    .recv()
                    .expect("test release");
                let mut result = outcome();
                result.command = request.command.clone();
                Ok(result)
            }),
            "pty-1",
        );
    let request = make_request("run-host-eof-during-review");
    let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = Arc::clone(&collected);
    let worker = std::thread::spawn(move || {
        let outcome = acp.run_stream(&request, &mut |event| {
            store.lock().expect("event store").push(event);
            Ok(())
        });
        (outcome, collected)
    });
    entered_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the authority must be reached");
    // The peer has already exited after reading owned, so the reply can no
    // longer be delivered. The local decision is still honoured.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut seen = replies(&capture);
    while seen.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        seen = replies(&capture);
    }
    assert_eq!(seen.len(), 1, "owned only: {seen:?}");
    assert_eq!(seen[0]["method"], OWNED);
    release_tx.send(()).expect("release the review");
    let (outcome, collected) = worker.join().expect("worker");
    if outcome.is_ok() {
        eprintln!(
            "EOF-REVIEW-OK events={:?}",
            collected.lock().expect("event store")
        );
    }
    let failure = outcome.expect_err("the reply cannot reach a vanished peer");
    let events = collected.lock().expect("event store").clone();
    let _ = fs::remove_file(&script);
    let seen = replies(&capture);
    let _ = fs::remove_file(&capture);

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "exactly one local dispatch"
    );
    assert_eq!(seen.len(), 1, "still owned only: {seen:?}");
    assert!(
        !failure.message.is_empty(),
        "the failure must explain itself: {failure:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::StatusChanged { phase, .. } if phase == "host_execute"
        )),
        "no delivered-reply status may be claimed: {events:?}"
    );
}

/// Whether a peer is still running. A zombie has already exited and closed its
/// pipes, so `kill -0` alone cannot mark the phase; the state must be read.
fn peer_still_alive(pid: &str) -> bool {
    let output = std::process::Command::new("ps")
        .arg("-o")
        .arg("state=")
        .arg("-p")
        .arg(pid)
        .output();
    match output {
        Ok(output) => {
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            !state.is_empty() && !state.starts_with('Z')
        }
        Err(_) => false,
    }
}

/// The capture file only proves the peer read `owned`, not that it exited. This
/// control waits for the peer process to be gone before releasing the review, so
/// the reply provably cannot be delivered and the write must fail.
#[test]
fn a_reply_after_the_peer_process_exited_fails_the_write() {
    let pidfile = std::env::temp_dir().join(format!("cosh-host-shell-eof-exit-{}.pid", nonce()));
    let finish = format!("printf '%s\\n' \"$$\" > '{}'", pidfile.display());
    let (script, capture) = peer_script_with_finish(
        "eof-after-exit",
        &[execute_request(94, "tool-eof-exit", "")],
        1,
        true,
        &finish,
    );
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned())
        .with_model_call(true)
        .with_host_authority(
            Arc::new(move |request: &AcpHostExecuteRequest| {
                counted.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).expect("test listener");
                release_rx
                    .lock()
                    .expect("release lock")
                    .recv()
                    .expect("test release");
                let mut result = outcome();
                result.command = request.command.clone();
                Ok(result)
            }),
            "pty-1",
        );
    let request = make_request("run-host-eof-after-exit");
    let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = Arc::clone(&collected);
    let worker = std::thread::spawn(move || {
        let outcome = acp.run_stream(&request, &mut |event| {
            store.lock().expect("event store").push(event);
            Ok(())
        });
        (outcome, collected)
    });
    entered_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the authority must be reached");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut seen = replies(&capture);
    while seen.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        seen = replies(&capture);
    }
    assert_eq!(seen.len(), 1, "owned only: {seen:?}");
    while !pidfile.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = fs::read_to_string(&pidfile).expect("peer pid");
    let pid = pid.trim().to_string();
    while peer_still_alive(&pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !peer_still_alive(&pid),
        "the peer must be gone before the review is released"
    );
    release_tx.send(()).expect("release the review");
    let (outcome, collected) = worker.join().expect("worker");
    let events = collected.lock().expect("event store").clone();
    eprintln!("EOF-AFTER-EXIT outcome={outcome:?} events={events:?}");
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
    let _ = fs::remove_file(&pidfile);

    let failure = outcome.expect_err("a reply to an exited peer must fail the write");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "exactly one local dispatch"
    );
    assert!(
        !failure.message.is_empty(),
        "the failure must explain itself"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::StatusChanged { phase, .. } if phase == "host_execute"
        )),
        "no delivered-reply status may be claimed: {events:?}"
    );
}
