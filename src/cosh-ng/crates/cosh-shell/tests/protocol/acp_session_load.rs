//! Process-level tests for ACP session/load resume on reconnect (C1a).
//!
//! A kept peer that died while serving the same workspace leaves a committed
//! session; the fresh connection must resume it via session/load when the peer
//! advertises the capability, refuse before any prompt when it does not, and
//! start a new session after an explicit fresh-session detach.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cosh_shell::adapter::{AcpAgentAdapter, AgentAdapter, FreshSessionOutcome};
use cosh_shell::types::{AgentEvent, AgentRequest};

use super::acp_entry::{agent_script, make_request, nonce};

/// A peer that advertises `loadSession` when `load` is set, answers session/new
/// with a fixed id, replays one restored chunk on session/load, echoes prompts,
/// and exits after the first (alpha) prompt so the next turn reconnects.
fn peer(name: &str, load: bool) -> (PathBuf, PathBuf, PathBuf) {
    let capture = std::env::temp_dir().join(format!("cosh-session-load-{name}-{}.jsonl", nonce()));
    let pids = std::env::temp_dir().join(format!("cosh-session-load-{name}-{}.pids", nonce()));
    let gone = capture.with_extension("gone");
    let capabilities = if load {
        r#"{"loadSession":true}"#
    } else {
        "{}"
    };
    let script_body = r#"
printf '%s
' "$$" >> 'PIPS'
req_id() { printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }
chunk() { printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"reuse-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"%s"}}}}
' "$1"; }
IFS= read -r init
printf '%s
' "$init" >> 'CAPTURE'
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":CAPS}}
' "$(req_id "$init")"
while IFS= read -r -t 30 line; do
  printf '%s
' "$line" >> 'CAPTURE'
  case "$line" in
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"reuse-session"}}
' "$(req_id "$line")"
      ;;
    *'"method":"session/load"'*)
      chunk "restored"
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}
' "$(req_id "$line")"
      ;;
    *'"method":"session/prompt"'*)
      rid=$(req_id "$line")
      text=$(printf '%s' "$line" | grep -o '"text":"[^"]*"' | head -1 | cut -d'"' -f4)
      chunk "echo:$text"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}
' "$rid"
      case "$text" in
        *alpha*)
          printf 'gone
' >> 'GONE'
          exit 0
          ;;
      esac
      ;;
  esac
done
"#
    .replace("PIPS", &pids.display().to_string())
    .replace("CAPTURE", &capture.display().to_string())
    .replace("GONE", &gone.display().to_string())
    .replace("CAPS", capabilities);
    (agent_script(name, &script_body), capture, pids)
}

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

fn wait_gone(gone: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !gone.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(gone.exists(), "the peer must reach its exit path");
    std::thread::sleep(Duration::from_millis(100));
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn a_reconnect_loads_the_committed_session_and_projects_the_replay() {
    let (script, capture, pids_file) = peer("load", true);
    let acp = adapter(&script);
    let first = acp.run(&request("load-1", "alpha")).expect("first turn");
    assert!(
        first
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "the first turn must end normally: {first:?}"
    );
    wait_gone(&capture.with_extension("gone"));
    let second = acp.run(&request("load-2", "beta")).expect("reconnect turn");
    let text = text_of(&second);
    assert!(
        text.contains("restored"),
        "the load replay must reach the sink: {text:?}"
    );
    assert!(text.contains("beta"), "{text:?}");
    assert_eq!(
        count(&capture, "session/new"),
        1,
        "only the first turn starts a session"
    );
    assert_eq!(
        count(&capture, "session/load"),
        1,
        "the reconnect must load"
    );
    let load = frames(&capture)
        .into_iter()
        .find(|frame| frame["method"] == "session/load")
        .expect("a session/load frame");
    assert_eq!(
        load["params"]["sessionId"], "reuse-session",
        "the committed id is loaded"
    );
    assert_eq!(pids(&pids_file).len(), 2, "a dead peer needs a new child");
    cleanup(&[
        &script,
        &capture,
        &pids_file,
        &capture.with_extension("gone"),
    ]);
}

#[test]
fn a_peer_without_load_session_refuses_the_resume_before_any_prompt() {
    let (script, capture, pids_file) = peer("noload", false);
    let acp = adapter(&script);
    acp.run(&request("noload-1", "alpha")).expect("first turn");
    wait_gone(&capture.with_extension("gone"));
    let error = acp
        .run(&request("noload-2", "beta"))
        .expect_err("a peer without session/load must refuse the resume");
    assert!(
        error.message.contains("session/load") && error.message.contains("refused"),
        "the refusal must name session/load: {}",
        error.message
    );
    assert_eq!(
        count(&capture, "session/load"),
        0,
        "no load frame to an incapable peer"
    );
    assert_eq!(
        count(&capture, "session/prompt"),
        1,
        "the second prompt must not be sent"
    );
    cleanup(&[
        &script,
        &capture,
        &pids_file,
        &capture.with_extension("gone"),
    ]);
}

#[test]
fn an_explicit_fresh_session_starts_new_instead_of_loading() {
    let (script, capture, pids_file) = peer("fresh", true);
    let acp = adapter(&script);
    acp.run(&request("fresh-1", "alpha")).expect("first turn");
    // alpha exits the peer; the detach still clears the committed identity.
    wait_gone(&capture.with_extension("gone"));
    let outcome = acp.start_fresh_session();
    assert!(
        matches!(outcome, FreshSessionOutcome::Detached { previous_session_id: Some(ref id) } if id == "reuse-session"),
        "the detach must report the committed id: {outcome:?}"
    );
    let second = acp.run(&request("fresh-2", "beta")).expect("fresh turn");
    assert!(text_of(&second).contains("beta"), "{second:?}");
    assert_eq!(
        count(&capture, "session/new"),
        2,
        "a fresh session starts new"
    );
    assert_eq!(
        count(&capture, "session/load"),
        0,
        "a fresh session must not load"
    );
    cleanup(&[
        &script,
        &capture,
        &pids_file,
        &capture.with_extension("gone"),
    ]);
}

/// A peer that stays alive across prompts (no exit), advertises loadSession,
/// and records every frame, so explicit selection is observed forcing a fresh
/// load even though the kept peer could still serve the turn.
fn live_peer(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let capture = std::env::temp_dir().join(format!("cosh-session-load-{name}-{}.jsonl", nonce()));
    let pids = std::env::temp_dir().join(format!("cosh-session-load-{name}-{}.pids", nonce()));
    let body = format!(
        r#"printf '%s\n' "$$" >> '{pids}'
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
sid="reuse-session"
chunk() {{ printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"%s","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"%s"}}}}}}}}\n' "$sid" "$1"; }}
IFS= read -r init
printf '%s\n' "$init" >> '{capture}'
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{"loadSession":true}}}}}}\n' "$(req_id "$init")"
while IFS= read -r -t 30 line; do
  printf '%s\n' "$line" >> '{capture}'
  case "$line" in
    *'"method":"session/new"'*)
      sid="reuse-session"
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"%s"}}}}\n' "$(req_id "$line")" "$sid"
      ;;
    *'"method":"session/load"'*)
      sid=$(printf '%s' "$line" | grep -o '"sessionId":"[^"]*"' | head -1 | cut -d'"' -f4)
      chunk "restored"
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$(req_id "$line")"
      ;;
    *'"method":"session/prompt"'*)
      rid=$(req_id "$line")
      text=$(printf '%s' "$line" | grep -o '"text":"[^"]*"' | head -1 | cut -d'"' -f4)
      chunk "echo:$text"
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$rid"
      ;;
  esac
done
"#,
        pids = pids.display(),
        capture = capture.display(),
    );
    (agent_script(name, &body), capture, pids)
}

#[test]
fn an_explicit_resume_loads_the_named_session_even_with_a_live_peer() {
    let (script, capture, pids_file) = live_peer("explicit");
    let acp = adapter(&script);
    let first = acp
        .run(&request("explicit-1", "alpha"))
        .expect("first turn");
    assert!(
        first
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentCompleted { .. })),
        "the first turn must end normally so the peer is kept: {first:?}"
    );
    assert_eq!(count(&capture, "session/new"), 1);
    assert_eq!(pids(&pids_file).len(), 1, "the kept peer is still alive");
    // The kept peer could serve the turn, but explicit selection must force a
    // fresh connection that loads the named session, not the kept peer's id.
    acp.resume_session("explicit-session");
    let second = acp
        .run(&request("explicit-2", "beta"))
        .expect("explicit resume turn");
    assert!(text_of(&second).contains("beta"), "{second:?}");
    assert_eq!(
        count(&capture, "session/load"),
        1,
        "explicit selection must load"
    );
    let load = frames(&capture)
        .into_iter()
        .find(|frame| frame["method"] == "session/load")
        .expect("a session/load frame");
    assert_eq!(
        load["params"]["sessionId"], "explicit-session",
        "the explicitly named session is loaded, not the kept peer's id"
    );
    assert_eq!(
        pids(&pids_file).len(),
        2,
        "explicit resume opens a fresh child"
    );
    cleanup(&[&script, &capture, &pids_file]);
}
