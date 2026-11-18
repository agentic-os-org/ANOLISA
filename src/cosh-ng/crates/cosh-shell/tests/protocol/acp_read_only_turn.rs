//! Process-level tests for the per-turn read-only restriction on the Shell ACP
//! client (M02).
//!
//! The scripted peer is a deterministic fixture, not a real Agent. These tests
//! cover the client half of the guarantee: admission needs a locally granted
//! profile *and* the peer's advertisement, a restricted continuation must not
//! reach a peer that fails either, and a capable peer must receive the
//! restriction on the prompt itself. The last test records the boundary that
//! admission is a local trust decision, not remote enforcement.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cosh_shell::adapter::{AcpAgentAdapter, AgentAdapter};
use cosh_shell::types::AgentRequest;

use super::acp_entry::{agent_script, make_request, nonce};

const TURN_KEY: &str = "_cosh/turn";
const CONTINUATION_HINT: &str = "analysis-only continuation after foreground shell handoff";

/// Peer that records every frame it reads and answers the handshake with the
/// supplied `agentCapabilities` JSON. `side_effect` runs when a prompt arrives.
fn recording_peer(name: &str, capabilities: &str, side_effect: &str) -> (PathBuf, PathBuf) {
    let capture =
        std::env::temp_dir().join(format!("cosh-read-only-turn-{name}-{}.jsonl", nonce()));
    let body = format!(
        r#"
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
IFS= read -r init
printf '%s\n' "$init" >> '{capture}'
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{capabilities}}}}}\n' "$(req_id "$init")"
for _ in 1 2 3; do
  IFS= read -r -t 10 line || break
  printf '%s\n' "$line" >> '{capture}'
  case "$line" in
    *'"method":"session/new"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"ro-session"}}}}\n' "$(req_id "$line")"
      ;;
    *'"method":"session/prompt"'*)
      {side_effect}
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$(req_id "$line")"
      break
      ;;
  esac
done
"#,
        capture = capture.display(),
        capabilities = capabilities,
        side_effect = side_effect,
    );
    (agent_script(name, &body), capture)
}

/// An adapter with no locally granted read-only profile: the production default.
fn adapter(program: &Path) -> AcpAgentAdapter {
    let mut acp = AcpAgentAdapter::new(program.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    acp.with_model_call(true)
}

/// An adapter whose construction has granted the profile locally.
fn trusted_adapter(program: &Path) -> AcpAgentAdapter {
    adapter(program).with_read_only_turn_profile()
}

fn continuation(id: &str) -> AgentRequest {
    let mut request = make_request(id);
    request.context_hints = vec![CONTINUATION_HINT.to_string()];
    request
}

fn frames(capture: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(capture)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture frame is JSON"))
        .collect()
}

fn method(frame: &serde_json::Value) -> &str {
    frame["method"].as_str().unwrap_or_default()
}

fn prompt_frame(capture: &Path) -> Option<serde_json::Value> {
    frames(capture)
        .into_iter()
        .find(|frame| method(frame) == "session/prompt")
}

const WITHOUT_TURN: &str = "{}";
const WITH_TURN: &str = r#"{"_meta":{"_cosh/turn":{"version":1,"readOnly":true}}}"#;

#[test]
fn a_restricted_continuation_never_reaches_a_peer_without_the_capability() {
    let (script, capture) = recording_peer("denied", WITHOUT_TURN, "");
    let outcome = trusted_adapter(&script).run(&continuation("ro-denied"));
    assert!(
        outcome.is_err(),
        "a peer that never promised the restriction must be refused: {outcome:?}"
    );
    let seen = frames(&capture);
    assert!(
        seen.iter().any(|frame| method(frame) == "initialize"),
        "the handshake is still how the client learns the capability: {seen:?}"
    );
    assert!(
        !seen.iter().any(|frame| method(frame) == "session/prompt"),
        "no prompt may reach an unconstrained peer: {seen:?}"
    );
    assert!(
        !seen.iter().any(|frame| method(frame) == "session/new"),
        "the refusal must precede session creation, which can start children: {seen:?}"
    );
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
}

#[test]
fn a_restricted_continuation_carries_the_restriction_to_a_capable_peer() {
    let (script, capture) = recording_peer("allowed", WITH_TURN, "");
    let outcome = trusted_adapter(&script).run(&continuation("ro-allowed"));
    assert!(
        outcome.is_ok(),
        "a capable peer must still serve the turn: {outcome:?}"
    );
    let prompt = prompt_frame(&capture).expect("a capable peer must receive the prompt");
    assert_eq!(
        prompt["params"]["_meta"][TURN_KEY],
        serde_json::json!({"version": 1, "readOnly": true}),
        "the restriction must travel on the prompt, not only in the handshake: {prompt}"
    );
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
}

#[test]
fn an_ordinary_turn_still_reaches_a_peer_without_the_capability() {
    let (script, capture) = recording_peer("ordinary", WITHOUT_TURN, "");
    let outcome = adapter(&script).run(&make_request("ro-ordinary"));
    assert!(
        outcome.is_ok(),
        "the restriction must not gate ordinary turns: {outcome:?}"
    );
    let prompt = prompt_frame(&capture).expect("an ordinary turn must reach the peer");
    assert!(
        prompt["params"]["_meta"][TURN_KEY].is_null(),
        "an ordinary turn must not claim a restriction it does not need: {prompt}"
    );
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
}

#[test]
fn recommend_only_without_the_continuation_hint_is_not_restricted() {
    let (script, capture) = recording_peer("recommend", WITHOUT_TURN, "");
    let request = make_request("ro-recommend");
    assert_eq!(request.mode, cosh_shell::types::AgentMode::RecommendOnly);
    let outcome = adapter(&script).run(&request);
    assert!(
        outcome.is_ok(),
        "recommend-only alone is not the restricted continuation: {outcome:?}"
    );
    assert!(
        prompt_frame(&capture).is_some(),
        "the prompt must still be delivered"
    );
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
}

#[test]
fn a_capability_the_client_cannot_verify_is_treated_as_absent() {
    for (name, capabilities) in [
        (
            "version",
            r#"{"_meta":{"_cosh/turn":{"version":2,"readOnly":true}}}"#,
        ),
        (
            "flag",
            r#"{"_meta":{"_cosh/turn":{"version":1,"readOnly":false}}}"#,
        ),
        ("shape", r#"{"_meta":{"_cosh/turn":"yes"}}"#),
    ] {
        let (script, capture) = recording_peer(name, capabilities, "");
        let outcome = trusted_adapter(&script).run(&continuation(&format!("ro-{name}")));
        assert!(
            outcome.is_err(),
            "{name}: an unverifiable advertisement must not authorise the turn: {outcome:?}"
        );
        assert!(
            prompt_frame(&capture).is_none(),
            "{name}: no prompt may reach the peer"
        );
        let _ = fs::remove_file(&script);
        let _ = fs::remove_file(&capture);
    }
}

/// A peer that copies the advertisement and then runs its own executor. This is
/// the counterexample the advertisement alone cannot stop.
fn lying_peer(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let marker =
        std::env::temp_dir().join(format!("cosh-read-only-turn-{name}-{}.marker", nonce()));
    let side_effect = format!("printf 'ran\\n' > '{}'", marker.display());
    let (script, capture) = recording_peer(name, WITH_TURN, &side_effect);
    (script, capture, marker)
}

#[test]
fn a_self_reporting_peer_without_a_local_profile_is_never_started() {
    let (script, capture, marker) = lying_peer("selfreport");
    let outcome = adapter(&script).run(&continuation("ro-selfreport"));
    assert!(
        !marker.exists(),
        "the peer must never get the chance to run its own executor"
    );
    assert!(
        !capture.exists(),
        "the refusal must precede starting the peer at all"
    );
    assert!(
        outcome.is_err(),
        "a copied advertisement must not authorise the turn: {outcome:?}"
    );
    let _ = fs::remove_file(&script);
}

/// Known limitation, not a guarantee: once the profile is granted locally, the
/// wire cannot constrain a peer that lies about honouring it. Admission is a
/// local trust decision, which is why the profile is off by default and must
/// only be granted where the restriction has been established locally.
#[test]
fn a_local_profile_is_a_trust_decision_not_remote_enforcement() {
    let (script, capture, marker) = lying_peer("trusted-liar");
    let outcome = trusted_adapter(&script).run(&continuation("ro-trusted-liar"));
    assert!(
        outcome.is_ok(),
        "a granted profile admits the turn: {outcome:?}"
    );
    assert!(
        prompt_frame(&capture).is_some(),
        "the granted profile delivers the prompt"
    );
    assert!(
        marker.exists(),
        "recorded boundary: a lying peer still runs its own executor"
    );
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&capture);
    let _ = fs::remove_file(&marker);
}
