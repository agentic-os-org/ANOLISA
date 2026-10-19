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
fn a_payload_that_opens_with_its_exception_is_a_failure() {
    // `nested_error` reports only an exception *quoted inside* a payload and
    // documents the leading one as this call's own failure, "handled by
    // find_signature" — which had a rule for a leading Python traceback but
    // none for the JVM/SQL classes its own marker list names. A tool that
    // crashed with its exception as the payload's first line therefore graded
    // as Ok at High confidence, and its error text stayed usable as evidence
    // for the very values it mentions.
    let v = classify(
        &plain_call(),
        "java.lang.NullPointerException: Cannot invoke \"java.util.List.size()\" because \"rows\" is null",
    );
    assert_eq!(
        v.status,
        CallStatus::Failed,
        "the exception opens the payload, so it is this call's failure: rule={}",
        v.matched_rule
    );
    assert_eq!(v.matched_rule, "R4");
    assert!(
        v.evidence_quote.is_some(),
        "a verdict needs a quotable basis"
    );
}

#[test]
fn t7_placeholder_payload_is_unknown_not_ok() {
    let v = classify(&bash_call("make build"), "[dws-bash:pending-post-tool-use]");
    assert_eq!(v.status, CallStatus::Unknown);
    assert_eq!(v.matched_rule, "B8");
}

#[test]
fn a_read_that_quotes_the_placeholder_marker_is_not_unknown() {
    // A successful Read of this very source quotes the marker inside a larger
    // payload. The call produced output, so B8 must not swallow it into
    // Unknown and drop the observation from the evidence pool.
    let payload = "src/agentsight/src/grounding/outcome.rs\n\
                   const PLACEHOLDER_MARKERS: &[&str] = &[\"pending-post-tool-use\"];\n\
                   fn is_placeholder_or_empty(text: &str) -> bool {";
    let v = classify(&plain_call(), payload);
    assert_eq!(v.status, CallStatus::Ok, "rule={}", v.matched_rule);
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
fn a_lowercase_error_line_opens_a_flagless_failure() {
    // The same flagless payload, spelled with a lowercase token, was graded
    // `Ok`: the anchored signatures are matched case-insensitively because
    // "the same condition is reported with different capitalisation by
    // different tools", and the sibling scans of the same payloads read
    // `error:` the same way (the grader's error signal, and the opt ledger's
    // error markers). The weak prefix alone let one letter decide the verdict.
    let lower = classify(&plain_call(), "error: cannot find value `rows`");
    assert_eq!(
        lower.status,
        CallStatus::Failed,
        "a flagless failure spelled in lowercase is still this call's: rule={}",
        lower.matched_rule
    );
    assert_eq!(lower.confidence, Confidence::Medium);
    assert_eq!(lower.matched_rule, "R5");

    // The opening-window guard is unchanged: an error word deep in the body
    // still describes something the tool merely reported.
    let far = format!("{}\nerror: disk full", "x".repeat(WEAK_SIGNAL_WINDOW + 10));
    assert_eq!(classify(&plain_call(), &far).status, CallStatus::Ok);
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
fn bracket_probe_is_expected() {
    // `[` and `[[` are the POSIX and bash spellings of `test`: a single
    // bracket probe that exits non-zero is the answer, not a fault (spec B3).
    let v = classify(&bash_call("[ -f /var/log/app.log ]"), "Exit code 1");
    assert_eq!(v.status, CallStatus::OkProbe, "rule={}", v.matched_rule);
    assert_eq!(v.matched_rule, "B3");

    let v = classify(&bash_call("[[ -f /var/log/app.log ]]"), "Exit code 1");
    assert_eq!(v.status, CallStatus::OkProbe, "rule={}", v.matched_rule);
    assert_eq!(v.matched_rule, "B3");
}

#[test]
fn redirected_probe_is_still_expected() {
    // `2>&1`, `>&2` and `&>` are redirections, not chains. Splitting on the raw
    // `&` left a stray segment ("1") whose head is not a probe command, so a
    // probe whose non-zero exit is the answer was graded as a real failure.
    for command in [
        "ls /nope 2>&1",
        "ls /nope >/dev/null 2>&1",
        "pgrep -f agent &>/dev/null",
        "ls /nope 1>&2",
    ] {
        let v = classify(
            &bash_call(command),
            "ls: cannot access '/nope': No such file or directory\nExit code 2",
        );
        assert_eq!(
            v.status,
            CallStatus::OkProbe,
            "{command} graded as {} ({:?})",
            v.matched_rule,
            v.status
        );
        assert_eq!(v.matched_rule, "B3", "{command}");
    }
}

/// A shell call whose command is an argv array is still a shell call.
///
/// `agentsight-trajectory-collector` maps a Codex rollout's
/// `local_shell_call` to an ATIF `ToolCall` whose `arguments.command` is the
/// executable plus its argv — `{"type":"exec","command":["ls","-l"]}` — and
/// its own regression asserts that shape (`test_tool_call_variants`). The
/// grounding reader took the key as a string only, so such a call looked like
/// one that runs no command at all: `is_probe` answered `false` for what is
/// plainly a probe, and a non-zero exit from `["ls", "/nope"]` was graded a
/// certain failure instead of the answer the probe produced.
#[test]
fn an_argv_command_is_read_like_a_shell_command() {
    for argv in [
        serde_json::json!(["ls", "/root/.qoder-server"]),
        serde_json::json!(["ls", "-l", "/root/.qoder-server"]),
        serde_json::json!(["/usr/bin/ls", "/root/.qoder-server"]),
    ] {
        let call = ToolCall {
            tool_call_id: "tc-1".into(),
            function_name: "shell".into(),
            arguments: serde_json::json!({ "type": "exec", "command": argv }),
            extra: None,
        };
        let v = classify(
            &call,
            "ls: cannot open directory '/root/.qoder-server': Permission denied\n(exit code 2)",
        );
        assert_eq!(
            v.status,
            CallStatus::OkProbe,
            "{argv} rule={}",
            v.matched_rule
        );
        assert_eq!(v.matched_rule, "B3", "{argv}");
    }
}

/// Reading the argv form must not make every argv call a probe, and a
/// non-probe command keeps the failure its exit code reports.
#[test]
fn an_argv_command_that_is_not_a_probe_is_not_demoted() {
    let call = ToolCall {
        tool_call_id: "tc-1".into(),
        function_name: "shell".into(),
        arguments: serde_json::json!({
            "type": "exec",
            "command": ["cp", "a", "/etc/app.conf"],
        }),
        extra: None,
    };
    let v = classify(
        &call,
        "cp: cannot create regular file '/etc/app.conf': Permission denied\n(exit code 1)",
    );
    assert_eq!(v.status, CallStatus::Failed, "rule={}", v.matched_rule);
    assert_eq!(v.matched_rule, "R3");
}

#[test]
fn backgrounded_probe_chain_with_a_real_command_is_not_a_probe() {
    // A bare `&` still separates: the second command is real work, so a
    // non-zero exit may be its failure.
    let v = classify(
        &bash_call("ls /nope & cat /etc/missing"),
        "cat: /etc/missing: No such file or directory\nExit code 1",
    );
    assert_eq!(v.status, CallStatus::Failed);
}

#[test]
fn redirection_before_a_real_command_still_splits() {
    // Tolerating the redirection must not swallow the separator that follows it.
    let v = classify(
        &bash_call("ls /nope 2>&1 && rm -rf /tmp/cache"),
        "rm: cannot remove '/tmp/cache': No such file or directory\nExit code 1",
    );
    assert_eq!(v.status, CallStatus::Failed);
}

#[test]
fn multiline_command_with_a_real_segment_is_not_a_probe() {
    // Newlines chain commands exactly like `;` and `&&` do. Splitting only on
    // the ASCII separators judged the whole script by its first line's head,
    // so a failing `cat` on line two was read as the expected answer of an
    // `ls` probe.
    let v = classify(
        &bash_call("ls /tmp\ncat /etc/nonexistent"),
        "cat: /etc/nonexistent: No such file or directory\nExit code 1",
    );
    assert_eq!(
        v.status,
        CallStatus::Failed,
        "a real command on a later line means the non-zero exit may be real"
    );
}

#[test]
fn multiline_probe_script_is_still_expected() {
    let v = classify(
        &bash_call("ls /tmp\ntest -f /etc/hosts"),
        "No such file or directory\nExit code 1",
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
fn colon_and_equals_separated_exit_codes_are_structured_evidence() {
    // The detector's and the grader's scans read the same tool results and both
    // accept a status separated from the words by spaces, a colon or an equals
    // sign (`mentions_failing_exit_code`); merged fixes treat "exit code: 0" as
    // a spelling that occurs. This classifier only accepted the space form, so
    // a failure whose only structured signal was "Exit code: 1" fell through to
    // the default Ok verdict and stayed in the evidence pool.
    for payload in [
        "Exit code: 1\nbuild stopped",
        "exit code=2\nno report generated",
        "(Command exited with code: 1)",
    ] {
        let v = classify(&bash_call("make"), payload);
        assert_eq!(v.status, CallStatus::Failed, "{payload}");
        assert_eq!(v.matched_rule, "R3", "{payload}");
        assert_eq!(v.confidence, Confidence::High, "{payload}");
    }

    // The success short-circuit must read the same spellings: error words in
    // the body of a successful command are not this call's failure.
    let v = classify(&bash_call("grep -r Error ."), "Error: none\nExit code: 0");
    assert_eq!(v.status, CallStatus::Ok);
    assert_eq!(v.matched_rule, "R3(exit0)");
}

/// A successful call whose *output* quotes the exit-code wording is not that
/// call's failure. A `Read` of a file that merely mentions `Exit code 1` — this
/// repository's own `outcome_tests.rs`, `grader/evidence.rs` and
/// `reuse/summarize.rs` all do — was classified `Failed` from the quoted text,
/// which dropped the whole observation from the evidence pool and turned the
/// paths the file names into apparently fabricated ones.
#[test]
fn a_quoted_exit_code_is_not_this_calls_failure() {
    let v = classify(
        &plain_call(),
        "// Exit code 1 is returned when the check fails.\nfn main() {}",
    );
    assert_eq!(v.status, CallStatus::Ok, "rule={}", v.matched_rule);
    assert_eq!(v.matched_rule, "default");

    // A reader that prefixes every line (Claude Code's `Read` numbers them)
    // embeds the quoted marker mid-line, where it is not this call's report.
    let v = classify(
        &plain_call(),
        "    58→            \"Exit code 255\\nssh: connect failed: Bad file descriptor\",",
    );
    assert_eq!(v.status, CallStatus::Ok, "rule={}", v.matched_rule);
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

#[test]
fn explicit_success_outranks_error_text_and_quoted_exit_codes() {
    for text in [
        "Traceback (most recent call last):\nRuntimeError: archived failure",
        "Error: archived failure",
        "No such file or directory",
        "Archived shell output\nExit code 1",
    ] {
        let mut successful = flagged_result(text);
        successful
            .extra
            .as_mut()
            .unwrap()
            .insert(EXTRA_IS_ERROR.into(), serde_json::json!(false));
        let v = classify_call(&plain_call(), Some(&successful));
        assert_eq!(v.status, CallStatus::Ok, "{text}");
        assert_eq!(v.confidence, Confidence::High);
        assert_eq!(v.matched_rule, "R1(success)");
        assert_eq!(
            classify_call(&plain_call(), Some(&flagged_result(text))).status,
            CallStatus::Failed
        );
        assert_eq!(classify(&plain_call(), text).status, CallStatus::Failed);
    }
}

#[test]
fn explicit_success_preserves_blocked_unknown_and_nested_evidence() {
    for (text, status) in [
        ("User denied permission", CallStatus::Blocked),
        ("", CallStatus::Unknown),
        ("  ", CallStatus::Unknown),
        ("[dws-bash:pending-post-tool-use]", CallStatus::Unknown),
    ] {
        let mut successful = flagged_result(text);
        successful
            .extra
            .as_mut()
            .unwrap()
            .insert(EXTRA_IS_ERROR.into(), serde_json::json!(false));
        assert_eq!(
            classify_call(&plain_call(), Some(&successful)).status,
            status
        );
    }
    let mut successful =
        flagged_result("tool method invocation failed, java.lang.NullPointerException at Foo.bar");
    successful
        .extra
        .as_mut()
        .unwrap()
        .insert(EXTRA_IS_ERROR.into(), serde_json::json!(false));
    let v = classify_call(&plain_call(), Some(&successful));
    assert_eq!(v.status, CallStatus::Ok);
    assert!(
        v.nested_error
            .as_deref()
            .is_some_and(|text| text.contains("NullPointerException"))
    );
}

/// A payload whose only failure signal is an operating-system refusal is a
/// certain failure for the other two readers of the same tool result.
///
/// `interruption::detector::text_has_tool_error_signal` and
/// `grader::evidence::text_has_error_signal` both read "permission denied"
/// and the `EACCES` / `EPERM` errno spellings. They were kept when the
/// generic words were dropped, and the errno pair was added to align the two
/// scans, so that a stored result is not a `ToolFailure` interruption for one
/// reader and a clean success for the other. The grounding classifier had
/// neither, so a failed permission check fell through to the default `Ok`
/// verdict: its result stayed in the evidence pool and could ground the very
/// paths it failed to access, and the directed `FailureThenFabrication` edge
/// could never name the call that caused the failure.
#[test]
fn denied_permission_is_a_failure_signature() {
    for payload in [
        "cp: cannot create regular file '/etc/app.conf': Permission denied",
        "open('/root/.ssh/id_ed25519') failed: EACCES",
        "write /proc/sys/net/ipv4/ip_forward: EPERM (Operation not permitted)",
    ] {
        let v = classify(&plain_call(), payload);
        assert_eq!(v.status, CallStatus::Failed, "{payload}");
        assert_eq!(v.matched_rule, "R4", "{payload}");
        assert_eq!(v.confidence, Confidence::High, "{payload}");
        assert!(v.evidence_quote.is_some(), "{payload}");
    }
}

/// The signals this rule adds sit below the rules that already decide these
/// payloads, so no established verdict changes.
#[test]
fn denied_permission_respects_the_rules_above_it() {
    // A declared success is the provider's word for this invocation, and the
    // grader reads the same payload the same way (`a search that skips
    // unreadable directories still exits 0`).
    let mut successful = flagged_result("find: '/root': Permission denied");
    successful
        .extra
        .as_mut()
        .unwrap()
        .insert(EXTRA_IS_ERROR.into(), serde_json::json!(false));
    assert_eq!(
        classify_call(&plain_call(), Some(&successful)).status,
        CallStatus::Ok
    );

    // A successful command may print a permission error in its body.
    let v = classify(
        &bash_call("grep -r denied /var/log"),
        "find: '/root': Permission denied\nExit code 0",
    );
    assert_eq!(v.status, CallStatus::Ok, "rule={}", v.matched_rule);
    assert_eq!(v.matched_rule, "R3(exit0)");

    // The user's own refusal is still an interception, not an agent failure.
    for payload in [
        "User denied permission",
        "Permission denied: User denied",
        "Claude requested permissions to write to /tmp/poem.txt, but you haven't granted it yet.",
    ] {
        assert_eq!(
            classify(&plain_call(), payload).status,
            CallStatus::Blocked,
            "{payload}"
        );
    }

    // A probe asking whether something is reachable answers with its refusal.
    let v = classify(
        &bash_call("ls /root/.qoder-server"),
        "ls: cannot open directory '/root/.qoder-server': Permission denied",
    );
    assert_eq!(v.status, CallStatus::OkProbe);
    assert_eq!(v.matched_rule, "B3");
}
