use super::*;

// Raising the shell-evidence idle timeout far above the test's own schedule
// makes the stalled-provider fallback unreachable, so a recovery that still
// happens can only have come from the evidence-delivery boundary.
const NO_IDLE_FALLBACK: (&str, &str) = ("COSH_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS", "600");

// The provider answered in the same turn as the Bash tool request and ended the
// turn before the foreground shell result existed. That text is based on prompt
// history, not on this command, so it must never reach the terminal; the answer
// must come from the shell-evidence continuation instead, and not from the
// stalled-provider idle fallback.
#[test]
fn raw_cli_preemptive_provider_text_is_dropped_and_recovered_from_shell_evidence() {
    let home = temp_shell_home("cosh-core-preemptive-stale-text");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-preempt","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-preempt","message":{"content":[{"type":"text","text":"Token total reported by this run: FRESH_SENTINEL."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-preempt","is_error":false,"result":"done"}'
    exit 0
    ;;
  *cosh-core-provider-preempts-shell-result*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-preempt","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"sleep 1; echo FRESH_SENTINEL"},"tool_use_id":"toolu-preempt"}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-preempt","message":{"content":[{"type":"text","text":"Token total reported by this run: STALE_SENTINEL."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-preempt","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-preempt","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            NO_IDLE_FALLBACK,
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-provider-preempts-shell-result\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("$ sleep 1; echo FRESH_SENTINEL"),
        "{output}"
    );
    // The pre-result answer is never rendered, in any form.
    assert!(!output.contains("STALE_SENTINEL"), "{output}");
    // The answer the user sees was generated after the evidence landed, and the
    // recovery ran exactly once.
    assert_eq!(
        count_occurrences(&output, "Token total reported by this run: FRESH_SENTINEL."),
        1,
        "{output}"
    );
    assert_ordered(
        &output,
        &[
            "Bash tool sent to shell",
            "Token total reported by this run: FRESH_SENTINEL.",
        ],
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}

// Same preemptive answer, but emitted while the foreground command is still
// running, so the text is held by the still-active run rather than by the
// shell-wide hold. It must be dropped there too, and the answer must still come
// from the shell-evidence continuation.
#[test]
fn raw_cli_preemptive_provider_text_during_command_is_dropped_and_recovered() {
    let home = temp_shell_home("cosh-core-preemptive-late-finish");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-late-finish","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-late-finish","message":{"content":[{"type":"text","text":"Token total reported by this run: FRESH_SENTINEL."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-late-finish","is_error":false,"result":"done"}'
    exit 0
    ;;
  *cosh-core-provider-preempts-during-command*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-late","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"sleep 5; echo FRESH_SENTINEL"},"tool_use_id":"toolu-late"}}'
    # Answer from prompt history early in the foreground command, then end the
    # turn without ever consuming the host-executed result. The command outlasts
    # this by ~4s, so the stale text always precedes the shell result.
    sleep 1
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-late-finish","message":{"content":[{"type":"text","text":"Token total reported by this run: STALE_SENTINEL."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-late-finish","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-late-finish","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            NO_IDLE_FALLBACK,
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-provider-preempts-during-command\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(1_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(10_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(!output.contains("STALE_SENTINEL"), "{output}");
    assert_eq!(
        count_occurrences(&output, "Token total reported by this run: FRESH_SENTINEL."),
        1,
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}

// The normal same-turn flow is untouched: preamble text emitted before the Bash
// tool request and the final text emitted after the host-executed result both
// survive, and no recovery turn is started.
#[test]
fn raw_cli_same_turn_host_executed_result_keeps_preamble_and_final_text() {
    let home = temp_shell_home("cosh-core-same-turn-preamble");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-same-turn","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-provider-same-turn-preamble*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-same-turn","message":{"content":[{"type":"text","text":"PREAMBLE_SENTINEL let me run the command."}]}}'
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-same-turn","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"sleep 1; echo SAME_TURN_OUTPUT"},"tool_use_id":"toolu-same-turn"}}'
    if IFS= read -r response; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*SAME_TURN_OUTPUT*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-same-turn","message":{"content":[{"type":"text","text":"FINAL_SENTINEL the command printed SAME_TURN_OUTPUT."}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-same-turn","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-same-turn","is_error":true,"result":"missing same-turn host_executed_shell result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-same-turn","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-provider-same-turn-preamble\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(6_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        output.contains("PREAMBLE_SENTINEL let me run the command."),
        "{output}"
    );
    assert!(
        output.contains("FINAL_SENTINEL the command printed SAME_TURN_OUTPUT."),
        "{output}"
    );
    assert!(
        output.contains("provider_result_delivery_status: delivered")
            || !output.contains("provider_result_delivery_status:"),
        "{output}"
    );
    assert!(
        !output.contains("missing same-turn host_executed_shell result"),
        "{output}"
    );
    assert!(
        !output.contains("Using a fresh provider turn for shell evidence recovery."),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}
