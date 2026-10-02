use super::*;

#[test]
fn raw_cli_streamed_tool_fallback_with_host_capability_is_not_delivered() {
    let home = temp_shell_home("qwen-streamed-tool-fallback-no-delivery");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
case "$*" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-streamed-fallback","model":"qwen-test"}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-streamed-fallback","message":{"content":[{"type":"text","text":"STREAMED FALLBACK RECOVERY ONLY"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-streamed-fallback","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-streamed-fallback","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *streamed-tool-fallback-no-delivery*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-streamed-fallback","message":{"id":"m1","type":"message","role":"assistant","model":"qwen","content":[{"type":"tool_use","id":"call_fallback","name":"run_shell_command","input":{"command":"echo STREAMED_FALLBACK"}}]}}'
    sleep 30
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-streamed-fallback","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? streamed-tool-fallback-no-delivery\n".to_vec(),
                Duration::from_millis(500),
            ),
            (
                b"/details handoff-1\n".to_vec(),
                Duration::from_millis(5_000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("$ echo STREAMED_FALLBACK"), "{output}");
    assert!(output.contains("STREAMED_FALLBACK"), "{output}");
    assert!(
        output.contains("STREAMED FALLBACK RECOVERY ONLY"),
        "{output}"
    );
    assert!(
        output.contains("selected_shell_execution_path: foreground_shell_handoff_recovery"),
        "{output}"
    );
    assert!(
        output.contains("provider_result_delivery_status: not_provider_tool_request"),
        "{output}"
    );
    assert!(!output.contains("host_executed_shell"), "{output}");
    assert!(
        !output.contains("control_protocol_host_executed_shell_result"),
        "{output}"
    );
}

#[test]
fn raw_cli_streamed_tool_fallback_recovery_blocks_new_shell_tool() {
    let home = temp_shell_home("qwen-streamed-tool-fallback-recovery-blocks-shell");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
case "$*" in
  *ShellCommandCompleted*)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-streamed-fallback-block","model":"qwen-test"}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-streamed-fallback-block","message":{"id":"m2","type":"message","role":"assistant","model":"qwen","content":[{"type":"text","text":"RECOVERY TRIED SECOND SHELL TOOL"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-streamed-fallback-block","message":{"id":"m3","type":"message","role":"assistant","model":"qwen","content":[{"type":"tool_use","id":"call_recovery","name":"run_shell_command","input":{"command":"echo SHOULD_NOT_RUN_IN_RECOVERY"}}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-streamed-fallback-block","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-streamed-fallback-block","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *streamed-tool-fallback-blocks-recovery-tool*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-streamed-fallback-block","message":{"id":"m1","type":"message","role":"assistant","model":"qwen","content":[{"type":"tool_use","id":"call_fallback","name":"run_shell_command","input":{"command":"echo STREAMED_ONCE"}}]}}'
    sleep 30
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-streamed-fallback-block","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("COSH_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS", "1"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? streamed-tool-fallback-blocks-recovery-tool\n".to_vec(),
                Duration::from_millis(500),
            ),
            (
                b"/details handoff-1\n".to_vec(),
                Duration::from_millis(5_000),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("$ echo STREAMED_ONCE"), "{output}");
    assert!(output.contains("STREAMED_ONCE"), "{output}");
    assert!(
        output.contains("RECOVERY TRIED SECOND SHELL TOOL"),
        "{output}"
    );
    assert_eq!(
        count_occurrences(&output, "Bash tool sent to shell"),
        1,
        "{output}"
    );
    assert!(
        !output.contains("cosh-osc$ echo SHOULD_NOT_RUN_IN_RECOVERY"),
        "{output}"
    );
    assert!(!output.contains("Auto-approved req-2"), "{output}");
    assert!(
        output.contains("provider_result_delivery_status: not_provider_tool_request"),
        "{output}"
    );
}

#[test]
fn raw_cli_qwen_streamed_non_shell_tool_renders_result_without_call_activity() {
    let home = temp_shell_home("qwen-streamed-non-shell-tool-activity");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-tool-activity","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *streamed-non-shell-tool-activity*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-activity","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_read","name":"Read","input":{}}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-activity","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"Cargo.toml\"}"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-activity","event":{"type":"content_block_stop","index":0}}'
    printf '%s\n' '{"type":"user","session_id":"sess-tool-activity","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_read","is_error":false,"content":"read output"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-tool-activity","message":{"role":"assistant","content":[{"type":"text","text":"READ VISIBILITY FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-tool-activity","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-tool-activity","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? streamed-non-shell-tool-activity\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Read completed"), "{output}");
    assert!(output.contains("Cargo.toml"), "{output}");
    assert!(output.contains("1 lines returned"), "{output}");
    assert!(output.contains("stdout: 1 lines"), "{output}");
    let main_output = output.split("/details tool-1").next().unwrap_or(&output);
    assert!(!main_output.contains("Read called"), "{output}");
    assert!(output.contains("Activity details tool-1"), "{output}");
    assert!(output.contains("Tool:"), "{output}");
    assert!(output.contains("Classification: file-read"), "{output}");
    assert!(output.contains("Result:"), "{output}");
    assert!(output.contains("Raw input:"), "{output}");
    assert!(
        output.contains("provider: provider_native_stream"),
        "{output}"
    );
    assert!(output.contains("Original: Read"), "{output}");
    assert!(output.contains("Preview: Cargo.toml"), "{output}");
    assert!(output.contains("READ VISIBILITY FINAL"), "{output}");
}

#[test]
fn raw_cli_qwen_tool_result_splits_agent_cards_around_structured_card() {
    let home = temp_shell_home("qwen-tool-result-splits-agent-card");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-tool-splits-agent-card","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *tool-result-splits-agent-card*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-splits-agent-card","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"PRE TOOL TEXT"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-splits-agent-card","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_read_split","name":"Read","input":{}}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-splits-agent-card","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"Cargo.toml\"}"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-splits-agent-card","event":{"type":"content_block_stop","index":1}}'
    printf '%s\n' '{"type":"user","session_id":"sess-tool-splits-agent-card","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_read_split","is_error":false,"content":"read output"}]}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-tool-splits-agent-card","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"POST TOOL TEXT"}}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-tool-splits-agent-card","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-tool-splits-agent-card","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ANIMATION", "always"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? tool-result-splits-agent-card\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    let pre = output.find("PRE TOOL TEXT").expect(&output);
    let first_bottom = output[pre..]
        .find('╰')
        .map(|offset| pre + offset)
        .expect(&output);
    let pending = output.find("Thinking... reading file: 1").expect(&output);
    let card = output.find("Read completed").expect(&output);
    let post = output.find("POST TOOL TEXT").expect(&output);
    let second_agent = output[card..post]
        .find("╭ Agent")
        .map(|offset| card + offset)
        .expect(&output);

    assert!(
        pre < first_bottom
            && first_bottom < pending
            && pending < card
            && card < second_agent
            && second_agent < post,
        "{output}"
    );
}

/// A model generating large arguments streams deltas for a long time before the
/// tool call exists. Without a status for that window the Agent card sits blank
/// and the user cannot tell generation from a hang.
///
/// Text is streamed first on purpose: that is the harder case, because a live
/// markdown surface otherwise suppresses the status entirely.
#[test]
fn raw_cli_qwen_slow_tool_arguments_show_a_status_without_the_payload() {
    let home = temp_shell_home("qwen-slow-tool-arguments-status");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-slow-arguments","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *slow-tool-arguments-status*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-slow-arguments","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me write that file.\n\n"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-slow-arguments","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_write_slow","name":"write_file","input":{}}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-slow-arguments","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"/tmp/SECRET_ARGUMENT_PATH\",\"content\":\"SECRET_ARGUMENT_BODY"}}}'
    sleep 8
    printf '%s\n' '{"type":"stream_event","session_id":"sess-slow-arguments","event":{"type":"content_block_stop","index":1}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-slow-arguments","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-slow-arguments","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ANIMATION", "always"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? slow-tool-arguments-status\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(12_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Let me write that file."), "{output}");
    assert!(
        output.contains("generating write_file arguments"),
        "the argument-generation window must be visible even after streamed text: {output}"
    );
    // The deltas carry the destination path and the file body; neither may reach
    // the terminal, and a byte count would only invent progress.
    assert!(!output.contains("SECRET_ARGUMENT_PATH"), "{output}");
    assert!(!output.contains("SECRET_ARGUMENT_BODY"), "{output}");
}

#[test]
fn raw_cli_qwen_long_running_tool_shows_pending_status_then_result_card() {
    let home = temp_shell_home("qwen-long-running-tool-pending-status");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-long-running-tool","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *long-running-tool-pending-status*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-long-running-tool","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_read_pending","name":"Read","input":{}}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-long-running-tool","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"Cargo.toml\"}"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-long-running-tool","event":{"type":"content_block_stop","index":0}}'
    sleep 2
    printf '%s\n' '{"type":"user","session_id":"sess-long-running-tool","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_read_pending","is_error":false,"content":"read output"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-long-running-tool","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-long-running-tool","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ANIMATION", "always"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? long-running-tool-pending-status\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Thinking... reading file: 1"), "{output}");
    assert!(output.contains("Read completed"), "{output}");
    assert!(output.contains("Cargo.toml"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
}

#[test]
fn raw_cli_qwen_streamed_write_file_renders_concise_result_card() {
    let home = temp_shell_home("qwen-streamed-write-file-tool-activity");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-write-tool-activity","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *streamed-write-file-tool-activity*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-write-tool-activity","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"WRITE PREAMBLE"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-write-tool-activity","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_write","name":"write_file","input":{}}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-write-tool-activity","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"/tmp/cosh-write-card.txt\",\"content\":\"SHOULD_NOT_LEAK_STREAMED_WRITE_CONTENT\"}"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-write-tool-activity","event":{"type":"content_block_stop","index":1}}'
    printf '%s\n' '{"type":"user","session_id":"sess-write-tool-activity","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_write","is_error":false,"content":"write ok"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-write-tool-activity","message":{"role":"assistant","content":[{"type":"text","text":"WRITE VISIBILITY FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-write-tool-activity","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-write-tool-activity","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_ANIMATION", "always"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? streamed-write-file-tool-activity\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    let pre = output.find("WRITE PREAMBLE").expect(&output);
    let first_bottom = output[pre..]
        .find('╰')
        .map(|offset| pre + offset)
        .expect(&output);
    let pending = output.find("Thinking... writing file: 1").expect(&output);
    let card = output.find("Write completed").expect(&output);
    assert!(pre < first_bottom && first_bottom < pending && pending < card);
    assert!(output.contains("Write completed"), "{output}");
    assert!(output.contains("/tmp/cosh-write-card.txt"), "{output}");
    assert!(output.contains("write completed: new file"), "{output}");
    assert!(output.contains("stdout: 1 lines"), "{output}");
    assert!(output.contains("Classification: file-write"), "{output}");
    assert!(output.contains("Impact: write"), "{output}");
    assert!(
        output.contains("Preview: /tmp/cosh-write-card.txt"),
        "{output}"
    );
    assert!(
        !output.contains("SHOULD_NOT_LEAK_STREAMED_WRITE_CONTENT"),
        "{output}"
    );
    assert!(!output.contains("\"content\""), "{output}");
}

#[test]
fn raw_cli_qwen_streamed_skill_tool_renders_skill_name_and_loaded_result() {
    let home = temp_shell_home("qwen-streamed-skill-tool-activity");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-skill-tool-activity","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *streamed-skill-tool-activity*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-skill-tool-activity","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_skill","name":"Skill","input":{}}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-skill-tool-activity","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"skill\":\"memory-analysis\"}"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-skill-tool-activity","event":{"type":"content_block_stop","index":0}}'
    printf '%s\n' '{"type":"user","session_id":"sess-skill-tool-activity","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_skill","is_error":false,"content":"skill loaded"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-skill-tool-activity","message":{"role":"assistant","content":[{"type":"text","text":"SKILL VISIBILITY FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-skill-tool-activity","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-skill-tool-activity","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? streamed-skill-tool-activity\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Skill completed"), "{output}");
    assert!(output.contains("memory-analysis"), "{output}");
    assert!(output.contains("instructions available"), "{output}");
    assert!(output.contains("stdout: 1 lines"), "{output}");
    assert!(output.contains("Classification: skill"), "{output}");
    assert!(output.contains("Impact: context-mutation"), "{output}");
    assert!(output.contains("Original: Skill"), "{output}");
    assert!(output.contains("Preview: memory-analysis"), "{output}");
    assert!(output.contains("SKILL VISIBILITY FINAL"), "{output}");
}

#[test]
fn raw_cli_qwen_streamed_skill_tool_renders_failed_result_phase() {
    let home = temp_shell_home("qwen-streamed-skill-tool-failure");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-skill-tool-failure","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *streamed-skill-tool-failure*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-skill-tool-failure","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_skill_fail","name":"Skill","input":{}}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-skill-tool-failure","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"skill\":\"missing-skill\"}"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-skill-tool-failure","event":{"type":"content_block_stop","index":0}}'
    printf '%s\n' '{"type":"user","session_id":"sess-skill-tool-failure","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_skill_fail","is_error":true,"content":"skill not found"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-skill-tool-failure","message":{"role":"assistant","content":[{"type":"text","text":"SKILL FAILURE FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-skill-tool-failure","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-skill-tool-failure","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? streamed-skill-tool-failure\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Skill failed"), "{output}");
    assert!(output.contains("missing-skill"), "{output}");
    assert!(output.contains("skill not found"), "{output}");
    assert!(output.contains("stderr: 1 lines"), "{output}");
    assert!(output.contains("Classification: skill"), "{output}");
    assert!(output.contains("Status: error"), "{output}");
    assert!(output.contains("SKILL FAILURE FINAL"), "{output}");
}

#[test]
fn raw_cli_qwen_unknown_mcp_tool_renders_custom_fallback_without_raw_json() {
    let home = temp_shell_home("qwen-streamed-unknown-mcp-tool");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-unknown-mcp-tool","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *unknown-mcp-tool-activity*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-unknown-mcp-tool","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_mcp","name":"mcp__github__create_issue","input":{"title":"Bug report","body":"SHOULD_NOT_RENDER_RAW_MCP_BODY","labels":["bug"]}}]}}'
    printf '%s\n' '{"type":"user","session_id":"sess-unknown-mcp-tool","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_mcp","is_error":false,"content":"created issue"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-unknown-mcp-tool","message":{"role":"assistant","content":[{"type":"text","text":"UNKNOWN MCP FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-unknown-mcp-tool","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-unknown-mcp-tool","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? unknown-mcp-tool-activity\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("mcp__github__create_issue completed"),
        "{output}"
    );
    assert!(
        output.contains("server: github; tool: create_issue"),
        "{output}"
    );
    assert!(output.contains("created issue"), "{output}");
    assert!(output.contains("UNKNOWN MCP FINAL"), "{output}");
    assert!(
        !output.contains("SHOULD_NOT_RENDER_RAW_MCP_BODY"),
        "{output}"
    );
    assert!(!output.contains("\"labels\""), "{output}");
}

#[test]
fn raw_cli_qwen_unknown_custom_tool_failure_renders_diagnostic_without_raw_json() {
    let home = temp_shell_home("qwen-unknown-custom-tool-failure");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-unknown-custom-failure","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *unknown-custom-tool-failure*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-unknown-custom-failure","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_custom_fail","name":"mcp__jira__create_ticket","input":{"title":"Failure report","body":"SHOULD_NOT_RENDER_FAILED_CUSTOM_BODY","labels":["internal"]}}]}}'
    printf '%s\n' '{"type":"user","session_id":"sess-unknown-custom-failure","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_custom_fail","is_error":true,"content":"custom tool failed"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-unknown-custom-failure","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-unknown-custom-failure","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? unknown-custom-tool-failure\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("mcp__jira__create_ticket failed"),
        "{output}"
    );
    assert!(
        output.contains("server: jira; tool: create_ticket"),
        "{output}"
    );
    assert!(output.contains("custom tool failed"), "{output}");
    assert!(
        !output.contains("SHOULD_NOT_RENDER_FAILED_CUSTOM_BODY"),
        "{output}"
    );
    assert!(!output.contains("\"labels\""), "{output}");
}

#[test]
fn raw_cli_qwen_unknown_plain_tool_input_renders_opaque_payload_summary() {
    let home = temp_shell_home("qwen-unknown-plain-tool-input");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-unknown-plain-tool","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *unknown-plain-tool-input*)
    printf '%s\n' '{"type":"stream_event","session_id":"sess-unknown-plain-tool","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_plain","name":"CustomPlain","input":"SHOULD_NOT_RENDER_RAW_PLAIN_INPUT"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-unknown-plain-tool","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"broken{"}}}'
    printf '%s\n' '{"type":"stream_event","session_id":"sess-unknown-plain-tool","event":{"type":"content_block_stop","index":0}}'
    printf '%s\n' '{"type":"user","session_id":"sess-unknown-plain-tool","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_plain","is_error":false,"content":"plain tool completed"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-unknown-plain-tool","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-unknown-plain-tool","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? unknown-plain-tool-input\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("CustomPlain completed"), "{output}");
    assert!(output.contains("input: opaque payload"), "{output}");
    assert!(output.contains("plain tool completed"), "{output}");
    assert!(
        !output.contains("SHOULD_NOT_RENDER_RAW_PLAIN_INPUT"),
        "{output}"
    );
    assert!(!output.contains("broken{"), "{output}");
}

#[test]
fn raw_cli_non_shell_permission_passes_through_allow_only() {
    let home = temp_shell_home("qwen-non-shell-pass-through");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let co_path = bin_dir.join("co");
    write_executable(
        &co_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-non-shell-pass-through","model":"qwen-test"}'
read -r user_message
case "$user_message" in
  *provider-read-pass-through*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-read","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"file_path":"README.md"},"tool_use_id":"toolu-read"}}'
    if IFS= read -r response; then
      case "$response" in
        *'"request_id":"ctrl-read"'*'"behavior":"allow"'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-non-shell-pass-through","message":{"content":[{"type":"text","text":"Read permission allowed through provider control protocol."}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-non-shell-pass-through","is_error":false,"result":"done"}'
          exit 0
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-non-shell-pass-through","is_error":true,"result":"missing non-shell allow response"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-non-shell-pass-through","is_error":false,"result":"ignored"}'
"#,
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "qwen",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? provider-read-pass-through\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(3_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(
        output.contains("Read permission allowed through provider control protocol."),
        "{output}"
    );
    assert!(!output.contains("Bash tool sent to shell"), "{output}");
    assert!(!output.contains("host_executed_shell"), "{output}");
    assert!(!output.contains("foreground_shell_pty"), "{output}");
    assert!(
        !output.contains("missing non-shell allow response"),
        "{output}"
    );
}
