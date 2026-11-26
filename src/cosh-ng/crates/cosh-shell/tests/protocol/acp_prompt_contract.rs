//! Process-level tests for the capability and purpose contract the Shell ACP
//! client sends to a peer (M08, H1c prompt parity).
//!
//! The peer is a deterministic fixture that records the prompt it receives, so
//! the contract is asserted on the wire rather than on a builder's return value.
//! The legacy contract is the reference for the shared invariants: the ACP entry
//! must carry the same mode, evidence, language and governance lines, while its
//! execution claims follow only what this connection actually wired.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cosh_shell::adapter::{AcpAgentAdapter, AcpHostExecuteOutcome, AdapterInstance, AgentRunPoll};
use cosh_shell::types::{AgentRequest, CoshApprovalMode};

use super::acp_entry::{agent_script, make_request, nonce};

const CONTINUATION_HINT: &str = "analysis-only continuation after foreground shell handoff";
const WITH_TURN: &str = r#"{"_meta":{"_cosh/turn":{"version":1,"readOnly":true}}}"#;
/// The legacy contract lines both entries must carry, frozen from
/// `provider_prompt_contract_for_language(Auto, "shell", EnUs)`. The two
/// execution bullets that name the `shell` tool and the config-dependent
/// language hint are excluded; a crate-internal guard keeps this file in step
/// with the legacy builder.
const SHARED_CONTRACT: &str = include_str!("acp_contract_reference.txt");

/// Peer that records every frame and answers one prompt with end_turn.
fn recording_peer(name: &str, capabilities: &str) -> (PathBuf, PathBuf) {
    let capture = std::env::temp_dir().join(format!("cosh-acp-contract-{name}-{}.jsonl", nonce()));
    let script_body = format!(
        r#"
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
IFS= read -r init
printf '%s\n' "$init" >> '{capture}'
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{capabilities}}}}}\n' "$(req_id "$init")"
while IFS= read -r -t 20 line; do
  printf '%s\n' "$line" >> '{capture}'
  case "$line" in
    *'"method":"session/new"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"contract-session"}}}}\n' "$(req_id "$line")"
      ;;
    *'"method":"session/prompt"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$(req_id "$line")"
      ;;
  esac
done
"#,
        capture = capture.display(),
        capabilities = capabilities,
    );
    (agent_script(name, &script_body), capture)
}

fn adapter(program: &Path) -> AcpAgentAdapter {
    let mut acp = AcpAgentAdapter::new(program.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    acp.with_model_call(true)
}

/// An adapter whose construction granted the read-only profile locally.
fn trusted_adapter(program: &Path) -> AcpAgentAdapter {
    adapter(program).with_read_only_turn_profile()
}

/// Wires a foreground authority that never actually runs anything.
fn with_authority(acp: AcpAgentAdapter) -> AcpAgentAdapter {
    acp.with_host_authority(
        Arc::new(|request: &cosh_shell::adapter::AcpHostExecuteRequest| {
            Ok(AcpHostExecuteOutcome {
                command: request.command.clone(),
                status: "completed".to_string(),
                exit_code: Some(0),
                signal: None,
                cwd: request.cwd.clone(),
                end_cwd: request.cwd.clone(),
                duration_ms: 1,
                output_ref: None,
                redaction_status: "not_requested".to_string(),
                approval_id: None,
                tool_use_id: None,
                input_wait: None,
                llm_content: String::new(),
                llm_content_truncated: false,
            })
        }),
        "pty-contract",
    )
}

fn continuation(id: &str) -> AgentRequest {
    let mut request = make_request(id);
    request.context_hints = vec![
        CONTINUATION_HINT.to_string(),
        "__cosh_user_approval_mode=auto".to_string(),
    ];
    request
}

fn prompt_texts(capture: &Path) -> Vec<String> {
    fs::read_to_string(capture)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("fixture frame is JSON"))
        .filter(|frame| frame["method"].as_str() == Some("session/prompt"))
        .map(|frame| {
            frame["params"]["prompt"]
                .as_array()
                .expect("prompt blocks")
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

/// Whether this connection advertised the host-shell capability, which is the
/// only condition under which the peer can reach the user's foreground PTY.
fn advertised_host_shell(capture: &Path) -> bool {
    fs::read_to_string(capture)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("fixture frame is JSON"))
        .any(|frame| {
            frame["method"].as_str() == Some("initialize")
                && frame["params"]["clientCapabilities"]["_meta"]["_cosh/shell"].is_object()
        })
}

fn one_prompt(capture: &Path) -> String {
    let texts = prompt_texts(capture);
    assert_eq!(texts.len(), 1, "expected exactly one prompt: {texts:?}");
    texts.into_iter().next().expect("one prompt")
}

/// Runs one turn through the production entry, which is where the invocation
/// mode is supplied, and returns the prompt the peer received.
fn run_through_entry(acp: AcpAgentAdapter, request: AgentRequest, mode: CoshApprovalMode) {
    let handle = AdapterInstance::Acp(acp).start_cancellable(request, mode);
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the acp run did not finish in time"
        );
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Finished) => return,
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => {}
            Err(error) => panic!("acp contract run failed: {error:?}"),
        }
    }
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn an_acp_turn_carries_the_invocation_contract() {
    let (script, capture) = recording_peer("agent", "{}");
    run_through_entry(
        adapter(&script),
        make_request("contract-agent"),
        CoshApprovalMode::Auto,
    );
    let prompt = one_prompt(&capture);
    assert!(
        prompt.contains("cosh-shell Agent contract:"),
        "the peer must receive the invocation contract: {prompt}"
    );
    assert!(prompt.contains("Mode: agent."), "{prompt}");
    assert!(
        prompt.contains("terminal-output:// refs are cosh-shell evidence ids"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("Use `shell` for live shell evidence"),
        "the ACP entry must not name a tool it never negotiated: {prompt}"
    );
    assert!(
        prompt.contains("reply in the language of the user's message"),
        "the language rule must reach the peer: {prompt}"
    );
    assert!(
        !prompt.contains("Bash tool") && !prompt.contains("AskUserQuestion"),
        "the ACP entry must not instruct the peer to call provider-specific tools: {prompt}"
    );
    cleanup(&[&script, &capture]);
}

#[test]
fn recommend_mode_reaches_the_acp_peer_as_no_tool_calls() {
    let (script, capture) = recording_peer("recommend", "{}");
    run_through_entry(
        adapter(&script),
        make_request("contract-recommend"),
        CoshApprovalMode::Recommend,
    );
    let prompt = one_prompt(&capture);
    assert!(prompt.contains("Mode: recommend."), "{prompt}");
    assert!(
        prompt.contains("do not emit tool calls"),
        "recommend mode must reach the peer as a no-tool-call turn: {prompt}"
    );
    assert!(
        prompt.contains("do not request shell output automatically"),
        "recommend mode must not invite automatic output requests: {prompt}"
    );
    assert!(
        !prompt.contains("emit exactly one fenced cosh-request block"),
        "recommend mode must not offer the automatic evidence request path: {prompt}"
    );
    cleanup(&[&script, &capture]);
}

#[test]
fn an_analysis_only_continuation_carries_the_turn_scoped_restriction() {
    let (script, capture) = recording_peer("continuation", WITH_TURN);
    run_through_entry(
        trusted_adapter(&script),
        continuation("contract-continuation"),
        CoshApprovalMode::Auto,
    );
    let prompt = one_prompt(&capture);
    assert!(
        prompt.contains("analysis-only continuation after a foreground shell handoff"),
        "the continuation purpose must reach the peer: {prompt}"
    );
    assert!(
        prompt.contains("This restriction applies only to this turn"),
        "the restriction must stay turn scoped: {prompt}"
    );
    assert!(
        prompt.contains("approval mode is auto and has not changed"),
        "the user's approval mode must be stated unchanged: {prompt}"
    );
    cleanup(&[&script, &capture]);
}

#[test]
fn the_foreground_execution_claim_follows_the_wired_authority() {
    let (script, capture) = recording_peer("foreground", "{}");
    run_through_entry(
        with_authority(adapter(&script)),
        make_request("contract-foreground"),
        CoshApprovalMode::Auto,
    );
    let wired = one_prompt(&capture);
    assert!(
        wired.contains("foreground shell transcript"),
        "a wired authority may promise the foreground transcript: {wired}"
    );
    assert!(
        advertised_host_shell(&capture),
        "the foreground claim must match the advertised capability"
    );
    cleanup(&[&script, &capture]);

    let (script, capture) = recording_peer("no-foreground", "{}");
    // The runtime entry always wires an approval authority, so the boundary
    // that removes the foreground promise is the one that cannot name a PTY
    // instance: without it the client never advertises host-shell.
    let mut unnameable = make_request("contract-no-foreground");
    unnameable.session_id = String::new();
    run_through_entry(adapter(&script), unnameable, CoshApprovalMode::Auto);
    let bare = one_prompt(&capture);
    assert!(
        !advertised_host_shell(&capture),
        "this connection must not advertise host-shell"
    );
    assert!(
        !bare.contains("foreground shell transcript"),
        "without an authority the client must not promise a foreground run: {bare}"
    );
    assert!(
        bare.contains("no foreground shell executor"),
        "the peer must be told where its own execution actually lands: {bare}"
    );
    cleanup(&[&script, &capture]);
}

#[test]
fn the_two_entries_share_the_same_contract_invariants() {
    let (script, capture) = recording_peer("parity", "{}");
    run_through_entry(
        adapter(&script),
        make_request("contract-parity"),
        CoshApprovalMode::Auto,
    );
    let prompt = one_prompt(&capture);
    let mut checked = 0;
    for line in SHARED_CONTRACT.lines() {
        assert!(
            prompt.contains(line),
            "the ACP entry lost a shared contract line: {line:?}\nfull prompt: {prompt}"
        );
        checked += 1;
    }
    assert!(
        checked >= 8,
        "the shared invariant set is too small: {checked}"
    );
    cleanup(&[&script, &capture]);
}

#[test]
fn a_mode_change_between_reused_turns_is_not_cached() {
    let (script, capture) = recording_peer("mode-change", "{}");
    let acp = adapter(&script);
    run_through_entry(
        acp.clone(),
        make_request("contract-mode-1"),
        CoshApprovalMode::Auto,
    );
    run_through_entry(
        acp.clone(),
        make_request("contract-mode-2"),
        CoshApprovalMode::Recommend,
    );
    let texts = prompt_texts(&capture);
    assert_eq!(texts.len(), 2, "both turns must reach the peer: {texts:?}");
    assert!(texts[0].contains("Mode: agent."), "{texts:?}");
    assert!(
        texts[1].contains("Mode: recommend."),
        "a reused connection must not keep the previous turn's mode: {texts:?}"
    );
    cleanup(&[&script, &capture]);
}

#[test]
fn a_tool_result_continuation_never_names_an_unwired_evidence_tool() {
    let (script, capture) = recording_peer("tool-result", "{}");
    let mut request = make_request("contract-tool-result");
    request.user_input = Some(
        "Tool result for request req-1\nTool: tool shell\nCommand: pwd\nStatus: executed\n\
         Exit code: 0\nReason: ok\nStdout:\n/work\nStderr:\n"
            .to_string(),
    );
    run_through_entry(adapter(&script), request, CoshApprovalMode::Auto);
    let prompt = one_prompt(&capture);
    assert!(
        prompt.contains("tool_result:"),
        "the tool-result branch must be the one exercised: {prompt}"
    );
    assert!(
        !prompt.contains("cosh_shell_evidence"),
        "the ACP entry has no control-protocol evidence tool: {prompt}"
    );
    assert!(
        prompt.contains("emit exactly one fenced cosh-request block"),
        "the read path this entry actually wired must stay available: {prompt}"
    );
    cleanup(&[&script, &capture]);
}

#[test]
fn an_evidence_excerpt_continuation_keeps_the_fenced_read_path() {
    let (script, capture) = recording_peer("excerpt", "{}");
    let mut request = make_request("contract-excerpt");
    request.user_input = Some(
        "ShellEvidenceExcerpt\noutput_id: terminal-output://raw-session/cmd-1\n\
         excerpt_status: available\nbounded_output_excerpt:\nhello\n"
            .to_string(),
    );
    run_through_entry(adapter(&script), request, CoshApprovalMode::Auto);
    let prompt = one_prompt(&capture);
    assert!(
        prompt.contains("shell_evidence_excerpt:"),
        "the excerpt branch must be the one exercised: {prompt}"
    );
    assert!(
        !prompt.contains("cosh_shell_evidence"),
        "the ACP entry has no control-protocol evidence tool: {prompt}"
    );
    assert!(
        prompt.contains("use the fenced cosh-request block described below"),
        "the excerpt must point at the read path this entry wired: {prompt}"
    );
    assert!(
        prompt.contains("output <output_id> tail"),
        "the fenced block format must still be spelled out once: {prompt}"
    );
    cleanup(&[&script, &capture]);
}
