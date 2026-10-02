use super::*;

#[test]
fn raw_cli_cosh_core_question_card_answer_continues_same_provider_turn() {
    let home = temp_shell_home("cosh-core-question-answer");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-question","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-provider-question-card*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-question","message":{"content":[{"type":"tool_use","id":"toolu-cosh-core-ask","name":"ask_user_question","input":{"question":"Choose a color for cosh-core provider follow-up","options":[{"label":"Green"},{"label":"Blue"}],"allow_free_text":true}}]}}'
    printf '%s\n' '{"type":"control_request","request_id":"ask-cosh-core-1","request":{"subtype":"ask_user","question":"Choose a color for cosh-core provider follow-up","options":[{"label":"Green"},{"label":"Blue"}],"allow_free_text":true,"multi_select":false}}'
    if IFS= read -r response; then
      case "$response" in
        *'"request_id":"ask-cosh-core-1"'*'"answer":"Green"'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-question","message":{"content":[{"type":"text","text":"Cosh-core question answer received in same provider turn."}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-question","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-question","is_error":true,"result":"missing cosh-core question answer"}'
    exit 1
    ;;
  *"Answer to pending Agent question"*)
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-question","is_error":true,"result":"question answer restarted provider instead of answering same turn"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-question","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            (
                "cosh-osc$",
                b"?? cosh-core-provider-question-card\n".as_slice(),
            ),
            ("Left/Right move | Enter send", b"\n"),
            (
                "Cosh-core question answer received in same provider turn.",
                b"exit\n",
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent question"), "{output}");
    assert!(!output.contains("ask_user_question called"), "{output}");
    assert!(
        !output.contains("Tool called: ask_user_question"),
        "{output}"
    );
    assert!(
        output.contains("Choose a color for cosh-core provider follow-up"),
        "{output}"
    );
    assert!(output.contains("[1] Green"), "{output}");
    assert!(output.contains("[2] Blue"), "{output}");
    assert!(output.contains("Answer: Green"), "{output}");
    assert!(
        output.contains("Cosh-core question answer received in same provider turn."),
        "{output}"
    );
    assert!(
        !output.contains("missing cosh-core question answer"),
        "{output}"
    );
    assert!(
        !output.contains("question answer restarted provider instead of answering same turn"),
        "{output}"
    );
    assert!(!output.contains("Got your answer:"), "{output}");
    assert!(!output.contains("/answer"), "{output}");
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(
        !output.contains("bash: cosh-core-provider-question-card: command not found"),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}

#[test]
fn raw_cli_cosh_core_narrow_question_and_debug_remain_readable() {
    let home = temp_shell_home("cosh-core-narrow-question-debug");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-narrow","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-narrow-question-debug*)
    printf '%s\n' '{"type":"control_request","request_id":"ask-cosh-core-narrow-1","request":{"subtype":"ask_user","question":"Choose the narrow terminal follow-up action for cosh-core provider output","options":[{"label":"Keep investigating"},{"label":"Open debug session"}],"allow_free_text":true,"multi_select":false}}'
    if IFS= read -r response; then
      case "$response" in
        *'"request_id":"ask-cosh-core-narrow-1"'*'"answer":"Open debug session"'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-narrow","message":{"content":[{"type":"text","text":"Cosh-core narrow terminal answer received before debug."}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-narrow","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-narrow","is_error":true,"result":"missing narrow question answer"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-narrow","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_WIDTH", "40"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            (
                "cosh-osc$",
                b"?? cosh-core-narrow-question-debug\n".as_slice(),
            ),
            (
                "Left/Right move | Enter send",
                b"Open debug session\n".as_slice(),
            ),
            ("received before debug.", b"/debug session\n".as_slice()),
            ("provider invocation:", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    let compact = compact_terminal_words(&output);
    assert!(output.contains("Agent question"), "{output}");
    assert!(output.contains("Choose the narrow terminal"), "{output}");
    assert!(output.contains("cosh-core provider output"), "{output}");
    assert!(output.contains("[1] Keep investigating"), "{output}");
    assert!(output.contains("[2] Open debug session"), "{output}");
    assert!(compact.contains("Answer: Open debug session"), "{output}");
    assert!(
        output.contains("Cosh-core narrow terminal answer"),
        "{output}"
    );
    assert!(output.contains("received before debug."), "{output}");
    assert!(output.contains("provider invocation:"), "{output}");
    assert!(
        !output.contains("missing narrow question answer"),
        "{output}"
    );
    assert!(!output.contains("bash: /debug"), "{output}");
    assert!(
        !output.contains("bash: cosh-core-narrow-question-debug: command not found"),
        "{output}"
    );
    assert_agent_block_width(&output, 40);
}

#[test]
fn raw_cli_cosh_core_malformed_question_fails_fast_and_restores_shell() {
    let home = temp_shell_home("cosh-core-malformed-question");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-malformed","model":"test"}'
read -r user_message
printf '%s\n' '{"type":"control_request","request_id":"ask-bad","request":{"subtype":"ask_user","questions":[{"question":"nested should not be reconstructed"}]}}'
exit 0
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? malformed-question\n"),
            (
                "Agent question unavailable",
                b"echo shell-recovered-after-malformed\n",
            ),
            ("shell-recovered-after-malformed", b"exit\n"),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        !output.contains("nested should not be reconstructed"),
        "{output}"
    );
    assert!(!output.contains("Agent question\n"), "{output}");
    assert!(output.contains("Agent question unavailable"), "{output}");
    assert!(
        output.contains("The Agent returned an incomplete question. Please retry."),
        "{output}"
    );
    assert!(
        output.contains("shell-recovered-after-malformed"),
        "{output}"
    );
    assert!(!output.contains("missing-question"), "{output}");
    assert!(!output.contains("Agent timed out:"), "{output}");
}

#[test]
fn raw_cli_cosh_core_answer_write_failure_retries_through_fallback() {
    let home = temp_shell_home("cosh-core-answer-write-failure");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-write-failure","model":"test"}'
read -r user_message
case "$user_message" in
  *"Answer to pending Agent question"*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-write-failure","message":{"content":[{"type":"text","text":"Answer was retried through a fallback provider turn."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-write-failure","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"control_request","request_id":"ask-write-failure","request":{"subtype":"ask_user","question":"Choose before the connection closes","options":[{"label":"Green"}],"allow_free_text":false,"multi_select":false}}'
exec 0<&-
sleep 2
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? answer-write-failure\n"),
            ("Left/Right move | Enter send", b"\n"),
            (
                "Answer was retried through a fallback provider turn.",
                b"exit\n",
            ),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Answer: Green"), "{output}");
    assert!(
        output.contains("Answer was retried through a fallback provider turn."),
        "{output}"
    );
    assert!(
        !output.contains("Agent answer delivery uncertain"),
        "{output}"
    );
    assert!(output.contains("cosh-osc$ exit"), "{output}");
    assert!(!output.contains("answer-write-failed"), "{output}");
    assert!(!output.contains("Agent timed out:"), "{output}");
}
