//! Process-level tests for one Shell owning one ACP peer and session (M04).
//!
//! The scripted peer is a deterministic fixture that stays alive across prompts
//! and records every frame plus its own pid, so reuse is observed on the wire
//! and in the process table rather than inferred from adapter state.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use cosh_shell::adapter::{AcpAgentAdapter, AgentAdapter};
use cosh_shell::types::{AgentEvent, AgentRequest};

use super::acp_entry::{agent_script, make_request, nonce};

/// A persistent peer: one initialize, one session/new, then a prompt loop.
/// `startup` runs before the handshake answer and `prompt_case` serves one
/// prompt with `$rid`, `$text` and `$history` in scope.
fn peer(name: &str, startup: &str, prompt_case: &str) -> (PathBuf, PathBuf, PathBuf) {
    let capture = std::env::temp_dir().join(format!("cosh-peer-reuse-{name}-{}.jsonl", nonce()));
    let pids = std::env::temp_dir().join(format!("cosh-peer-reuse-{name}-{}.pids", nonce()));
    let gone = capture.with_extension("gone");
    let prompt_case = prompt_case
        .replace("CAPTURE", &capture.display().to_string())
        .replace("GONE", &gone.display().to_string());
    let script_body = r#"
printf '%s\n' "$$" >> 'PIPS'
req_id() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
chunk() { printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"reuse-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"%s"}}}}\n' "$1"; }
history=""
broken=0
STARTUP
IFS= read -r init
printf '%s\n' "$init" >> 'CAPTURE'
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}\n' "$(req_id "$init")"
while IFS= read -r -t 30 line; do
  printf '%s\n' "$line" >> 'CAPTURE'
  case "$line" in
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"reuse-session"}}\n' "$(req_id "$line")"
      ;;
    *'"method":"session/load"'*)
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"reuse-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"restored"}}}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$(req_id "$line")"
      ;;
    *'"method":"session/prompt"'*)
      rid=$(req_id "$line")
      text=$(printf '%s' "$line" | grep -o '"text":"[^"]*"' | head -1 | cut -d'"' -f4)
      history="$history $text"
      PROMPT_CASE
      ;;
  esac
done
"#
    .replace("PIPS", &pids.display().to_string())
    .replace("CAPTURE", &capture.display().to_string())
    .replace("STARTUP", startup)
    .replace("PROMPT_CASE", &prompt_case);
    (agent_script(name, &script_body), capture, pids)
}

const ECHO: &str = r#"
chunk "history:$history"
printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$rid"
"#;

/// For the first prompt: emits one chunk, waits for the cancel notification,
/// answers the pending prompt as cancelled, then deliberately emits a late
/// chunk afterwards. Later prompts echo, so a restarted peer answers normally.
const CANCEL_THEN_LATE: &str = r#"
case "$text" in
  *alpha*)
chunk "partial"
if IFS= read -r -t 10 next; then
  printf '%s\n' "$next" >> 'CAPTURE'
  case "$next" in
    *'"method":"session/cancel"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"cancelled"}}\n' "$rid"
      chunk "late-after-cancel"
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$rid"
      ;;
  esac
else
  printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$rid"
fi
    ;;
  *)
    chunk "history:$history"
    printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$rid"
    ;;
esac
"#;

/// Refuses the prompt that asks for poison and stays broken afterwards, so the
/// refusal is a property of the connection rather than of one request text.
const REFUSE_POISON: &str = r#"
case "$text" in
  *poison*)
    broken=1
    ;;
esac
if [ "$broken" = 1 ]; then
  printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32603,"message":"fixture refusal"}}\n' "$rid"
else
  chunk "history:$history"
  printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$rid"
fi
"#;

/// Answers the first prompt normally and then exits, so the connection is kept
/// after a successful turn but the peer is gone before the next request.
const EXIT_AFTER_FIRST: &str = r#"
chunk "history:$history"
printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$rid"
case "$text" in
  *alpha*)
    printf 'gone\n' >> 'GONE'
    exit 0
    ;;
esac
"#;

fn adapter(program: &Path) -> AcpAgentAdapter {
    let mut acp = AcpAgentAdapter::new(program.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    acp.with_model_call(true)
}

fn frames(capture: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(capture)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture frame is JSON"))
        .collect()
}

fn count(capture: &Path, method_name: &str) -> usize {
    frames(capture)
        .iter()
        .filter(|frame| frame["method"].as_str() == Some(method_name))
        .count()
}

fn pids(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn text_of(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn request(id: &str, prompt: &str) -> AgentRequest {
    let mut request = make_request(id);
    request.user_input = Some(prompt.to_string());
    request
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn a_second_user_request_reuses_the_child_and_session_without_a_second_initialize() {
    let (script, capture, pids_file) = peer("reuse", "", ECHO);
    let acp = adapter(&script);
    let first = acp.run(&request("reuse-1", "alpha")).expect("first turn");
    let second = acp.run(&request("reuse-2", "beta")).expect("second turn");
    assert!(text_of(&first).contains("alpha"), "{first:?}");
    assert!(text_of(&second).contains("beta"), "{second:?}");
    assert_eq!(count(&capture, "initialize"), 1, "one handshake per Shell");
    assert_eq!(count(&capture, "session/new"), 1, "one session per Shell");
    assert_eq!(count(&capture, "session/prompt"), 2, "both turns ran");
    let seen = pids(&pids_file);
    assert_eq!(
        seen.len(),
        1,
        "the second turn must reuse the child: {seen:?}"
    );
    cleanup(&[&script, &capture, &pids_file]);
}

#[test]
fn the_reused_session_keeps_the_conversation_context() {
    let (script, capture, pids_file) = peer("context", "", ECHO);
    let acp = adapter(&script);
    acp.run(&request("ctx-1", "alpha")).expect("first turn");
    let second = acp.run(&request("ctx-2", "beta")).expect("second turn");
    let text = text_of(&second);
    assert!(
        text.contains("alpha") && text.contains("beta"),
        "the peer must still see the first turn: {text:?}"
    );
    cleanup(&[&script, &capture, &pids_file]);
}

#[test]
fn a_changed_workspace_does_not_reuse_the_peer() {
    let (script, capture, pids_file) = peer("cwd", "", ECHO);
    let acp = adapter(&script);
    acp.run(&request("cwd-1", "alpha")).expect("first turn");
    let other = tempfile::tempdir().expect("other workspace");
    let mut second = request("cwd-2", "beta");
    let cwd = fs::canonicalize(other.path()).expect("canonical other workspace");
    second.command_block.cwd = cwd.to_string_lossy().into_owned();
    second.command_block.end_cwd = second.command_block.cwd.clone();
    acp.run(&second).expect("second turn");
    assert_eq!(
        count(&capture, "initialize"),
        2,
        "a new workspace is a new session"
    );
    assert_eq!(count(&capture, "session/new"), 2);
    assert_eq!(
        pids(&pids_file).len(),
        2,
        "a new workspace needs a new child"
    );
    cleanup(&[&script, &capture, &pids_file]);
}

#[test]
fn a_cancelled_turn_does_not_pollute_the_next_turn() {
    let (script, capture, pids_file) = peer("cancel", "", CANCEL_THEN_LATE);
    let acp = adapter(&script);
    let flag = acp.cancel_handle();
    let watched = capture.clone();
    let canceller = std::thread::spawn(move || {
        // A fixed sleep can land during the handshake, which cancels a turn
        // that never started; wait for the prompt to reach the peer instead.
        let deadline = Instant::now() + Duration::from_secs(20);
        while count(&watched, "session/prompt") == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        flag.store(true, Ordering::SeqCst);
    });
    let first = acp
        .run(&request("cancel-1", "alpha"))
        .expect("cancelled turn");
    canceller.join().expect("canceller thread");
    assert!(
        first
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCancelled { .. })),
        "the first turn must end cancelled: {first:?}"
    );
    // The runtime clears the shared flag when it admits the next run; a direct
    // `run` call has to model that admission or the next turn inherits it.
    acp.cancel_handle().store(false, Ordering::SeqCst);
    let second = acp.run(&request("cancel-2", "beta")).expect("next turn");
    let text = text_of(&second);
    assert!(
        !text.contains("late-after-cancel"),
        "a frame emitted after the cancelled terminal must not reach the next turn: {text:?}"
    );
    assert!(text.contains("beta"), "{second:?}");
    cleanup(&[&script, &capture, &pids_file]);
}

#[test]
fn a_failed_turn_does_not_leave_a_poisoned_peer_for_the_next_request() {
    let (script, capture, pids_file) = peer("failed", "", REFUSE_POISON);
    let acp = adapter(&script);
    let first = acp
        .run(&request("fail-1", "poison"))
        .expect("failed turn is reported");
    assert!(
        first
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentFailed { .. })),
        "the refusal must surface as a failure: {first:?}"
    );
    let second = acp.run(&request("fail-2", "beta")).expect("next turn");
    assert!(
        second
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "the next request must not inherit the failed turn: {second:?}"
    );
    cleanup(&[&script, &capture, &pids_file]);
}

#[test]
fn a_peer_that_exited_while_idle_is_not_handed_to_the_next_request() {
    let (script, capture, pids_file) = peer("idle-exit", "", EXIT_AFTER_FIRST);
    let acp = adapter(&script);
    let first = acp.run(&request("idle-1", "alpha")).expect("first turn");
    assert!(
        first
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "the first turn must end normally so the peer is kept: {first:?}"
    );
    let gone = capture.with_extension("gone");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !gone.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        gone.exists(),
        "the peer must have reached its own exit path"
    );
    // Only the adapter reaps this child, so it can still be a zombie here; the
    // settle lets the exit finish before the next request is admitted.
    std::thread::sleep(Duration::from_millis(100));
    let second = acp.run(&request("idle-2", "beta")).expect("next turn");
    assert!(text_of(&second).contains("beta"), "{second:?}");
    assert_eq!(
        count(&capture, "initialize"),
        2,
        "a dead peer must not be reused"
    );
    assert_eq!(
        count(&capture, "session/new"),
        1,
        "only the first turn starts a session"
    );
    assert_eq!(
        count(&capture, "session/load"),
        1,
        "the reconnect resumes the committed session"
    );
    assert_eq!(pids(&pids_file).len(), 2, "a dead peer needs a new child");
    cleanup(&[&script, &capture, &pids_file, &gone]);
}

#[test]
fn cold_start_and_reused_steady_state_are_distinguishable() {
    let (script, capture, pids_file) = peer("timing", "sleep 0.5", ECHO);
    let acp = adapter(&script);
    let started = Instant::now();
    acp.run(&request("time-1", "alpha")).expect("cold turn");
    let cold = started.elapsed();
    let started = Instant::now();
    acp.run(&request("time-2", "beta")).expect("warm turn");
    let warm = started.elapsed();
    println!(
        "REUSE-TIMING cold_ms={} warm_ms={}",
        cold.as_millis(),
        warm.as_millis()
    );
    assert_eq!(count(&capture, "initialize"), 1);
    assert!(
        warm < cold,
        "the reused turn must not pay the handshake again: cold={cold:?} warm={warm:?}"
    );
    assert!(
        warm.as_millis() < 500,
        "the reused turn must not pay the fixture startup cost again: {warm:?}"
    );
    assert!(
        cold.as_millis() >= 500,
        "the fixture injects a 500ms startup cost that the cold turn must pay: {cold:?}"
    );
    cleanup(&[&script, &capture, &pids_file]);
}
