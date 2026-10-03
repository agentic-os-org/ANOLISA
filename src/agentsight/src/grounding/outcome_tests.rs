use super::*;
use agentsight_atif::EXTRA_IS_ERROR;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn bash_call(command: &str) -> ToolCall {
    ToolCall {
        tool_call_id: "tc-1".into(),
        function_name: "Bash".into(),
        arguments: serde_json::json!({ "command": command }),
        extra: None,
    }
}

fn plain_call() -> ToolCall {
    ToolCall {
        tool_call_id: "tc-1".into(),
        function_name: "Read".into(),
        arguments: serde_json::json!({}),
        extra: None,
    }
}

fn result(content: &str) -> ObservationResult {
    ObservationResult {
        source_call_id: Some("tc-1".into()),
        content: Some(serde_json::Value::String(content.into())),
        subagent_trajectory_ref: None,
        extra: None,
    }
}

fn flagged_result(content: &str) -> ObservationResult {
    let mut extra = HashMap::new();
    extra.insert(EXTRA_IS_ERROR.to_string(), serde_json::Value::Bool(true));
    ObservationResult {
        extra: Some(extra),
        ..result(content)
    }
}

fn classify(call: &ToolCall, content: &str) -> CallVerdict {
    classify_call(call, Some(&result(content)))
}

// ---------------------------------------------------------------------------
// attribution_decision_spec.md §8 — T1..T7 (call-level fixtures)
// ---------------------------------------------------------------------------

#[test]
fn t1_provider_flag_outranks_exit_code() {
    let v = classify_call(
        &bash_call("ssh host uptime"),
        Some(&flagged_result(
            "Exit code 255\nssh: connect failed: Bad file descriptor",
        )),
    );
    assert_eq!(v.status, CallStatus::Failed);
    assert_eq!(v.matched_rule, "R1");
    assert_eq!(v.confidence, Confidence::High);
}

#[test]
fn t2_exit_zero_ignores_error_words_in_body() {
    let v = classify(
        &bash_call("grep -r Error ."),
        "=== A. new default exporter ===\nError during tracing: F\nExit code 0",
    );
    assert_eq!(v.status, CallStatus::Ok, "rule={}", v.matched_rule);
    assert_eq!(v.matched_rule, "R3(exit0)");
}

#[test]
fn t3_probe_not_found_is_expected() {
    let v = classify(
        &bash_call("ls /root/.qoder-server/*.log"),
        "ls: cannot access '/root/.qoder-server/*.log': No such file or directory",
    );
    assert_eq!(v.status, CallStatus::OkProbe);
    assert_eq!(v.matched_rule, "B3");
}

#[test]
fn t4_command_not_found_is_a_failure() {
    let v = classify(
        &bash_call("python script.py"),
        "(eval):1: command not found: python\nExit code 127",
    );
    assert_eq!(v.status, CallStatus::Failed);
    assert_eq!(v.matched_rule, "R3");
}

#[test]
fn t5_user_interception_is_blocked_not_failed() {
    let v = classify(
        &bash_call("rm -rf /tmp/x"),
        "The user doesn't want to proceed with this tool use",
    );
    assert_eq!(v.status, CallStatus::Blocked);
    assert_eq!(v.matched_rule, "R2");
}

#[test]
fn t6_nested_exception_annotates_without_failing() {
    let v = classify(
        &plain_call(),
        "result ok; tool method invocation failed, java.lang.NullPointerException at Foo.bar",
    );
    assert_eq!(v.status, CallStatus::Ok);
    assert!(
        v.nested_error
            .as_deref()
            .is_some_and(|n| n.contains("NullPointerException")),
        "nested_error={:?}",
        v.nested_error
    );
}

#[test]
fn t7_placeholder_payload_is_unknown_not_ok() {
    let v = classify(&bash_call("make build"), "[dws-bash:pending-post-tool-use]");
    assert_eq!(v.status, CallStatus::Unknown);
    assert_eq!(v.matched_rule, "B8");
}

// ---------------------------------------------------------------------------
// R0 / R4 / R5 and their guards
// ---------------------------------------------------------------------------

#[test]
fn unlinked_call_is_unknown_never_guessed() {
    let v = classify_call(&plain_call(), None);
    assert_eq!(v.status, CallStatus::Unknown);
    assert_eq!(v.matched_rule, "R0");
}

#[test]
fn anchored_signature_fails_without_exit_code() {
    let v = classify(&plain_call(), "File has not been read yet. Read it first.");
    assert_eq!(v.status, CallStatus::Failed);
    assert_eq!(v.matched_rule, "R4");
    assert!(
        v.evidence_quote.is_some(),
        "a verdict needs a quotable basis"
    );
}

#[test]
fn two_part_contract_signature_needs_both_halves() {
    let both = classify(
        &plain_call(),
        "Invalid arguments for Edit: missing required parameter 'old_string'",
    );
    assert_eq!(both.status, CallStatus::Failed);

    let half = classify(&plain_call(), "Invalid arguments for Edit were corrected");
    assert_eq!(
        half.status,
        CallStatus::Ok,
        "half a signature must not indict a call"
    );
}

#[test]
fn weak_prefix_only_counts_inside_the_opening_window() {
    let near = classify(&plain_call(), "Error: disk full");
    assert_eq!(near.status, CallStatus::Failed);
    assert_eq!(
        near.confidence,
        Confidence::Medium,
        "text-only, needs review"
    );

    let far = format!("{}\nError: disk full", "x".repeat(WEAK_SIGNAL_WINDOW + 10));
    let v = classify(&plain_call(), &far);
    assert_eq!(
        v.status,
        CallStatus::Ok,
        "an error word deep in the body is not this call's failure"
    );
}

#[test]
fn http_error_counts_only_at_line_start() {
    let at_start = classify(&plain_call(), "HTTP 503 Service Unavailable");
    assert_eq!(at_start.status, CallStatus::Failed);

    let inline = classify(&plain_call(), "the guide says HTTP 404 means not found");
    assert_eq!(inline.status, CallStatus::Ok);
}

#[test]
fn mixed_command_chain_is_not_a_probe() {
    let v = classify(
        &bash_call("ls /tmp && rm -rf /tmp/cache"),
        "rm: cannot remove '/tmp/cache': No such file or directory\nExit code 1",
    );
    assert_eq!(
        v.status,
        CallStatus::Failed,
        "a real command in the chain means the non-zero exit may be real"
    );
}

#[test]
fn probe_chain_of_only_probes_is_expected() {
    let v = classify(
        &bash_call("which python3 && test -f /etc/hosts"),
        "no python3 found\nExit code 1",
    );
    assert_eq!(v.status, CallStatus::OkProbe);
}

#[test]
fn absolute_probe_path_is_recognised() {
    let v = classify(&bash_call("/bin/ls /nope"), "No such file or directory");
    assert_eq!(v.status, CallStatus::OkProbe);
}

#[test]
fn last_exit_code_wins_over_earlier_subcommands() {
    let text = "Exit code 1\nretrying\nExit code 0";
    let (code, idx) = last_exit_code(text).expect("a code is present");
    assert_eq!(code, 0, "the final sub-command decides the call");
    assert!(
        text[idx..].starts_with("Exit code 0"),
        "offset must point at the last marker, got {:?}",
        &text[idx..]
    );

    assert_eq!(last_exit_code("no code here"), None);
}

#[test]
fn alternate_exit_code_wordings_are_structured_evidence() {
    // Verbatim shape from a live capture: the only structured failure signal is
    // the trailing parenthetical, which the single "Exit code " marker missed,
    // leaving a certain failure to be guessed at by the weak text heuristic.
    let v = classify(
        &bash_call("sqlite3 db.sqlite 'SELECT 1'"),
        "Error: in prepare, no such column: complete\n  SELECT COUNT(*) FROM t WHERE status = \"complete\";\n\n(Command exited with code 1)",
    );
    assert_eq!(v.status, CallStatus::Failed);
    assert_eq!(
        v.confidence,
        Confidence::High,
        "a reported exit code is structured evidence, not a guess"
    );

    let v = classify(&bash_call("run"), "boom\nexit status 2");
    assert_eq!(v.status, CallStatus::Failed);
    assert_eq!(v.confidence, Confidence::High);

    // The same generalisation must not invent failures: a zero exit in the
    // alternate wording still clears error words in the body.
    let v = classify(
        &bash_call("grep -r error ."),
        "src/a.rs: error handling\n(Command exited with code 0)",
    );
    assert_eq!(v.status, CallStatus::Ok);
}

#[test]
fn non_ascii_payload_does_not_panic() {
    let v = classify(
        &plain_call(),
        "中文输出，文件未找到：no such file or directory",
    );
    assert_eq!(v.status, CallStatus::Failed);
    assert!(v.evidence_quote.is_some());
}

#[test]
fn empty_payload_is_unknown() {
    let v = classify(&plain_call(), "   ");
    assert_eq!(v.status, CallStatus::Unknown);
    assert_eq!(v.matched_rule, "B8");
}

#[test]
fn clean_payload_is_ok_with_no_quote() {
    let v = classify(&plain_call(), "wrote 3 files");
    assert_eq!(v.status, CallStatus::Ok);
    assert_eq!(v.matched_rule, "default");
    assert!(v.evidence_quote.is_none());
    assert!(v.nested_error.is_none());
}

// ---------------------------------------------------------------------------
// Shapes taken from a live capture
// ---------------------------------------------------------------------------

#[test]
fn pending_permission_is_blocked_even_when_flagged_as_error() {
    // The runtime reports an ungranted permission as a tool error. Trusting the
    // flag here would charge the user's own refusal to the agent.
    let v = classify_call(
        &plain_call(),
        Some(&flagged_result(
            "Claude requested permissions to write to /tmp/poem.txt, but you haven't granted it yet.",
        )),
    );
    assert_eq!(v.status, CallStatus::Blocked);
    assert_eq!(v.matched_rule, "R2");
}

#[test]
fn command_not_found_is_a_failure_without_flag_or_exit_code() {
    let v = classify(
        &bash_call("jq .name package.json"),
        "/bin/bash: line 1: jq: command not found\n\nCommand not found",
    );
    assert_eq!(v.status, CallStatus::Failed);
    assert_eq!(v.matched_rule, "R4");
}

#[test]
fn genuine_missing_file_error_still_fails() {
    let v = classify_call(
        &plain_call(),
        Some(&flagged_result(
            "File does not exist. Note: your current working directory is /tmp.",
        )),
    );
    assert_eq!(v.status, CallStatus::Failed, "a real error keeps failing");
    assert_eq!(v.matched_rule, "R1");
}

#[test]
fn grep_reporting_binary_matches_is_success() {
    let v = classify(
        &bash_call("grep -r agentsight /var/log"),
        "grep: /var/log/x.db: binary file matches\ngrep: /var/log/y.db: binary file matches",
    );
    assert_eq!(v.status, CallStatus::Ok, "rule={}", v.matched_rule);
}
