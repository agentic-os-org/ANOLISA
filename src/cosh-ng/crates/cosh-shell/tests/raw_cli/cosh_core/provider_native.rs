use super::*;

#[test]
fn raw_cli_cosh_core_suppresses_provider_native_echo_after_manual_host_executed_shell() {
    let home = temp_shell_home("cosh-core-manual-host-executed-echo");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-manual-host-executed-echo","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-manual-host-executed-echo*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-cosh-core-manual-echo","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"sudo -V"},"tool_use_id":"toolu-cosh-core-manual-echo"}}'
    # #1940: skip approval receipts that now precede the decision line.
    while IFS= read -r response; do
      case "$response" in
        *'"type":"approval_receipt"'*) continue ;;
        *) break ;;
      esac
    done
    if [ -n "$response" ]; then
      case "$response" in
        *'"behavior":"host_executed_shell"'*bounded_output_summary*'sudo -V'*)
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-manual-host-executed-echo","message":{"content":[{"type":"text","text":"Manual host-executed result accepted."},{"type":"tool_use","id":"toolu-cosh-core-manual-echo-provider","name":"shell","input":{"command":"sudo -V"}}]}}'
          printf '%s\n' '{"type":"user","session_id":"sess-cosh-core-manual-host-executed-echo","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu-cosh-core-manual-echo-provider","is_error":false,"content":"PROVIDER ECHO SHOULD BE SUPPRESSED\n"}]}}'
          printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-manual-host-executed-echo","message":{"content":[{"type":"text","text":"COSH CORE HOST EXECUTED ECHO FINAL"}]}}'
          printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-manual-host-executed-echo","is_error":false,"result":"done"}'
          exit 0
          ;;
        *'"behavior":"allow"'*)
          printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-manual-host-executed-echo","is_error":true,"result":"unexpected provider-native allow"}'
          exit 1
          ;;
      esac
    fi
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-manual-host-executed-echo","is_error":true,"result":"missing manual host_executed_shell result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-manual-host-executed-echo","is_error":false,"result":"ignored"}'
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
            ("COSH_SHELL_DEBUG", "1"),
        ],
        vec![
            (
                b"?? cosh-core-manual-host-executed-echo\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (b"/debug session\n".to_vec(), Duration::from_millis(6_000)),
            (b"exit 0\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);
    let normalized = strip_ansi_escape(&output).replace('\r', "");

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(!output.contains("Auto-approved req-1"), "{output}");
    assert!(!output.contains("Auto-approved req-2"), "{output}");
    assert!(!output.contains("auto-approved by provider"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    // The provider-native replay stays suppressed, while the approved command
    // is presented exactly once as the shell-owned command line.
    assert_eq!(count_occurrences(&normalized, "\nsudo -V\n"), 1, "{output}");
    assert!(
        output.contains("COSH CORE HOST EXECUTED ECHO FINAL"),
        "{output}"
    );
    assert!(
        !output.contains("PROVIDER ECHO SHOULD BE SUPPRESSED"),
        "{output}"
    );
    assert!(
        !output.contains("unexpected provider-native allow"),
        "{output}"
    );
    assert!(
        !output.contains("missing manual host_executed_shell result"),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}

#[test]
fn raw_cli_cosh_core_provider_native_tool_results_are_visible() {
    let home = temp_shell_home("cosh-core-provider-native-tool-visible");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-native-visible","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-provider-native-visible*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-native-visible","message":{"content":[{"type":"tool_use","id":"call_cosh_core_shell","name":"shell","input":{"command":"echo COSH_CORE_NATIVE_SHELL"}}]}}'
    printf '%s\n' '{"type":"user","session_id":"sess-cosh-core-native-visible","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_cosh_core_shell","is_error":false,"content":"COSH_CORE_NATIVE_SHELL\n"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-native-visible","message":{"content":[{"type":"text","text":"COSH CORE PROVIDER NATIVE VISIBLE FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-native-visible","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-native-visible","is_error":false,"result":"ignored"}'
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
            ("COSH_SHELL_DEBUG", "1"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-provider-native-visible\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("$ echo COSH_CORE_NATIVE_SHELL"), "{output}");
    assert!(output.contains("COSH_CORE_NATIVE_SHELL"), "{output}");
    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(
        output.contains("Provider-native shell tool allowed"),
        "{output}"
    );
    assert!(
        output.contains("COSH CORE PROVIDER NATIVE VISIBLE FINAL"),
        "{output}"
    );
    assert!(!output.contains("auto-approved by provider"), "{output}");
    assert!(
        !output.contains("provider_native_shell_bypassed_control_protocol"),
        "{output}"
    );
    assert!(!output.contains("host_executed_shell"), "{output}");
    assert!(!output.contains("missing host_executed_shell"), "{output}");
    assert!(!output.contains("foreground_shell_pty"), "{output}");
}

#[test]
fn raw_cli_cosh_core_provider_native_shell_omits_duplicate_activity_in_normal_ui() {
    let home = temp_shell_home("cosh-core-provider-native-shell-no-activity");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-native-no-activity","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-provider-native-no-activity*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-native-no-activity","message":{"content":[{"type":"tool_use","id":"call_shell_no_activity","name":"shell","input":{"command":"echo COSH_CORE_NATIVE_NO_ACTIVITY"}}]}}'
    printf '%s\n' '{"type":"user","session_id":"sess-cosh-core-native-no-activity","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_shell_no_activity","is_error":false,"content":"COSH_CORE_NATIVE_NO_ACTIVITY\n"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-native-no-activity","message":{"content":[{"type":"text","text":"COSH CORE PROVIDER NATIVE NO ACTIVITY FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-native-no-activity","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-native-no-activity","is_error":false,"result":"ignored"}'
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
                b"?? cosh-core-provider-native-no-activity\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(4_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("$ echo COSH_CORE_NATIVE_NO_ACTIVITY"),
        "{output}"
    );
    assert!(output.contains("COSH_CORE_NATIVE_NO_ACTIVITY"), "{output}");
    assert!(
        output.contains("COSH CORE PROVIDER NATIVE NO ACTIVITY FINAL"),
        "{output}"
    );
    assert!(!output.contains("auto-approved by provider"), "{output}");
    assert!(
        !output.contains("provider_native_shell_bypassed_control_protocol"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_core_control_request_suppresses_matching_shell_snapshot() {
    let home = temp_shell_home("cosh-core-control-suppresses-shell-snapshot");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-control-snapshot","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-control-snapshot-dup*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-control-snapshot","message":{"content":[{"type":"tool_use","id":"toolu-cosh-core-dup","name":"shell","input":{"command":"df -h"}}]}}'
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-cosh-core-dup","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"df -h"},"tool_use_id":"toolu-cosh-core-dup"}}'
    # #1940: skip approval receipts that now precede the decision line.
    while IFS= read -r response; do
      case "$response" in
        *'"type":"approval_receipt"'*) continue ;;
        *) break ;;
      esac
    done
    case "$response" in
      *'"behavior":"host_executed_shell"'*'df -h'*)
        printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-control-snapshot","message":{"content":[{"type":"text","text":"COSH CORE CONTROL SNAPSHOT FINAL"}]}}'
        printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-control-snapshot","is_error":false,"result":"done"}'
        exit 0
        ;;
    esac
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-control-snapshot","is_error":true,"result":"missing host executed result"}'
    exit 1
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-control-snapshot","is_error":false,"result":"ignored"}'
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
                b"?? cosh-core-control-snapshot-dup\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Auto-approved req-1"), "{output}");
    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Filesystem"), "{output}");
    assert!(
        output.contains("COSH CORE CONTROL SNAPSHOT FINAL"),
        "{output}"
    );
    assert!(!output.contains("auto-approved by provider"), "{output}");
    assert!(
        !output.contains("provider_native_shell_bypassed_control_protocol"),
        "{output}"
    );
    assert!(!output.contains("missing host executed result"), "{output}");
}

#[test]
fn raw_cli_cosh_core_streamed_provider_native_shell_result_renders_before_final_text() {
    let home = temp_shell_home("cosh-core-streamed-provider-native-shell-visible");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-streamed-native-visible","model":"cosh-core-test"}'
read -r user_message
printf '%s\n' '{"type":"stream_event","session_id":"sess-cosh-core-streamed-native-visible","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","session_id":"sess-cosh-core-streamed-native-visible","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"COSH CORE PRE TOOL TEXT STREAMS"}}}'
printf '%s\n' '{"type":"stream_event","session_id":"sess-cosh-core-streamed-native-visible","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_cosh_core_stream_shell","name":"shell","input":{}}}}'
printf '%s\n' '{"type":"stream_event","session_id":"sess-cosh-core-streamed-native-visible","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"echo COSH_CORE_STREAM_NATIVE_SHELL\"}"}}}'
printf '%s\n' '{"type":"stream_event","session_id":"sess-cosh-core-streamed-native-visible","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","session_id":"sess-cosh-core-streamed-native-visible","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"COSH CORE POST TOOL TEXT SHOULD WAIT"}}}'
printf '%s\n' '{"type":"stream_event","session_id":"sess-cosh-core-streamed-native-visible","event":{"type":"message_stop"}}'
sleep 1
printf '%s\n' '{"type":"user","session_id":"sess-cosh-core-streamed-native-visible","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_cosh_core_stream_shell","is_error":false,"content":"COSH_CORE_STREAM_NATIVE_SHELL\n"}]}}'
printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-streamed-native-visible","message":{"content":[{"type":"text","text":"COSH CORE STREAM PROVIDER NATIVE FINAL"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-streamed-native-visible","is_error":false,"result":"done"}'
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
            ("COSH_SHELL_DEBUG", "1"),
        ],
        vec![
            (b"/mode approval auto\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-streamed-provider-native-visible\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(3_000)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    let command_pos = output
        .find("$ echo COSH_CORE_STREAM_NATIVE_SHELL")
        .unwrap_or_else(|| panic!("{output}"));
    let stdout_pos = output
        .find("COSH_CORE_STREAM_NATIVE_SHELL")
        .unwrap_or_else(|| panic!("{output}"));
    let pre_tool_text_pos = output
        .find("COSH CORE PRE TOOL TEXT STREAMS")
        .unwrap_or_else(|| panic!("{output}"));
    let post_tool_text_pos = output
        .find("COSH CORE POST TOOL TEXT SHOULD WAIT")
        .unwrap_or_else(|| panic!("{output}"));
    let final_pos = output
        .find("COSH CORE STREAM PROVIDER NATIVE FINAL")
        .unwrap_or_else(|| panic!("{output}"));
    assert!(pre_tool_text_pos < command_pos, "{output}");
    assert!(command_pos < post_tool_text_pos, "{output}");
    assert!(command_pos < final_pos, "{output}");
    assert!(stdout_pos < post_tool_text_pos, "{output}");
    assert!(stdout_pos < final_pos, "{output}");
    assert!(
        output.contains("auto-approved by provider: $ echo COSH_CORE_STREAM_NATIVE_SHELL"),
        "{output}"
    );
    assert!(!output.contains("[Details] tool-"), "{output}");
    assert!(
        output.contains("provider_native_shell_bypassed_control_protocol"),
        "{output}"
    );
    assert!(!output.contains("host_executed_shell"), "{output}");
    assert!(!output.contains("foreground_shell_pty"), "{output}");
}

#[test]
fn raw_cli_cosh_core_provider_native_non_shell_result_is_visible() {
    let home = temp_shell_home("cosh-core-provider-native-non-shell-visible");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-native-non-shell-visible","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-provider-native-non-shell-visible*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-native-non-shell-visible","message":{"content":[{"type":"tool_use","id":"call_cosh_core_read","name":"Read","input":{"file_path":"Cargo.toml"}}]}}'
    printf '%s\n' '{"type":"user","session_id":"sess-cosh-core-native-non-shell-visible","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_cosh_core_read","is_error":false,"content":"read output visible"}]}}'
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-native-non-shell-visible","message":{"content":[{"type":"text","text":"COSH CORE PROVIDER NATIVE NON SHELL FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-native-non-shell-visible","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-native-non-shell-visible","is_error":false,"result":"ignored"}'
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
                b"?? cosh-core-provider-native-non-shell-visible\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(3_000)),
            (b"/details out-1\n".to_vec(), Duration::from_millis(500)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Read completed"), "{output}");
    assert!(output.contains("Cargo.toml"), "{output}");
    assert!(output.contains("1 lines returned"), "{output}");
    assert!(output.contains("stdout: 1 lines"), "{output}");
    assert!(output.contains("Classification: file-read"), "{output}");
    assert!(output.contains("Preview: Cargo.toml"), "{output}");
    assert!(output.contains("Tool output - stdout captured"), "{output}");
    assert!(output.contains("read output visible"), "{output}");
    assert!(
        output.contains("COSH CORE PROVIDER NATIVE NON SHELL FINAL"),
        "{output}"
    );
}

#[test]
fn raw_cli_path_prompt_remains_visible_before_first_tool_call() {
    let home = temp_shell_home("cosh-core-path-prompt-visible");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-path-prompt-visible","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *README.md*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-path-prompt-visible","message":{"content":[{"type":"tool_use","id":"call_cosh_core_read","name":"Read","input":{"file_path":"README.md"}}]}}'
    printf '%s\n' '{"type":"user","session_id":"sess-cosh-core-path-prompt-visible","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_cosh_core_read","is_error":false,"content":"path prompt read output"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-path-prompt-visible","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"error","session_id":"sess-cosh-core-path-prompt-visible","is_error":true,"result":"missing path prompt"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let prompt = "帮我读一下这个文件./README.md";
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
        ],
        vec![
            (prompt.as_bytes().to_vec(), Duration::from_millis(500)),
            (b"\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2_500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    let request_notice = format!("Agent input: {prompt}");
    let prompt_pos = output
        .find(&request_notice)
        .unwrap_or_else(|| panic!("path prompt notice was not rendered: {output}"));
    let tool_pos = output
        .find("Read completed")
        .unwrap_or_else(|| panic!("first tool result was not rendered: {output}"));
    assert!(prompt_pos < tool_pos, "{output}");
    assert_eq!(output.matches(&request_notice).count(), 1, "{output}");
    assert!(
        !output.contains("Received shell prompt request"),
        "{output}"
    );
    assert!(!output.contains("command not found"), "{output}");
    assert!(!output.contains("missing path prompt"), "{output}");
}
