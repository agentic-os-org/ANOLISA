use super::*;

#[test]
fn raw_cli_capped_run_approval_continues_same_core_process() {
    let home = temp_shell_home("cosh-core-turn-extension");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    let process_log = home.join("process.log");
    let request_log = home.join("requests.log");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
printf '%s\n' "$$" >> "$COSH_CORE_PROCESS_LOG"
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
read -r first_request
printf '%s\n' "$first_request" >> "$COSH_CORE_REQUEST_LOG"
printf '%s\n' '{"type":"system","subtype":"init","session_id":"55555555-5555-4555-8555-555555555555","session_resumable":true,"model":"cosh-core-test","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error","session_id":"55555555-5555-4555-8555-555555555555","is_error":true,"result":"Agent exceeded max turns (5)","error_code":"max_turns","max_turns":5}'
read -r second_request
printf '%s\n' "$second_request" >> "$COSH_CORE_REQUEST_LOG"
case "$second_request" in
  *"Continue the current task"*)
    printf '%s\n' '{"type":"assistant","session_id":"55555555-5555-4555-8555-555555555555","message":{"content":[{"type":"text","text":"COSH CORE TURN EXTENSION FINAL"}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"55555555-5555-4555-8555-555555555555","is_error":false,"result":"done"}'
    ;;
  *)
    printf '%s\n' '{"type":"result","subtype":"error","session_id":"55555555-5555-4555-8555-555555555555","is_error":true,"result":"missing turn continuation request"}'
    ;;
esac
"#,
    );

    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let process_log_str = process_log.to_string_lossy().to_string();
    let request_log_str = request_log.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &core_str),
            ("COSH_CORE_PROCESS_LOG", &process_log_str),
            ("COSH_CORE_REQUEST_LOG", &request_log_str),
            // Keep the process count scoped to agent-run lifecycles: the
            // startup banner's credential probe would add a one-shot
            // `cosh-core --registry` spawn unrelated to this contract.
            ("COSH_SHELL_STARTUP_BANNER", "0"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$", b"?? cosh-core-turn-extension\n".as_slice()),
            ("The Agent used all 5 configured turns", b"\n".as_slice()),
            ("COSH CORE TURN EXTENSION FINAL", b"exit\n".as_slice()),
        ],
    );
    let process_count = fs::read_to_string(&process_log)
        .expect("process log")
        .lines()
        .count();
    let requests = fs::read_to_string(&request_log).expect("request log");
    let request_count = requests.lines().count();
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent exceeded max turns (5)"), "{output}");
    assert!(output.contains("Agent turn budget"), "{output}");
    assert!(
        output.contains("Continue") && output.contains("Stop"),
        "{output}"
    );
    assert!(output.contains("Continuing"), "{output}");
    assert!(
        output.contains("COSH CORE TURN EXTENSION FINAL"),
        "{output}"
    );
    assert_eq!(process_count, 1, "{output}");
    assert_eq!(request_count, 2, "{requests}");
    assert!(requests.contains("Continue the current task"), "{requests}");
}

#[test]
fn raw_cli_non_resumable_cap_has_no_extension_card() {
    let home = temp_shell_home("cosh-core-non-resumable-cap");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
read -r user_message
printf '%s\n' '{"type":"system","subtype":"init","session_id":"66666666-6666-4666-8666-666666666666","session_resumable":false,"model":"cosh-core-test","tools":[]}'
printf '%s\n' '{"type":"result","subtype":"error","session_id":"66666666-6666-4666-8666-666666666666","is_error":true,"result":"Agent exceeded max turns (5)","error_code":"max_turns","max_turns":5}'
"#,
    );

    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &core_str)],
        vec![
            (b"?? cosh-core-non-resumable-cap\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(output.contains("Agent exceeded max turns (5)"), "{output}");
    assert!(!output.contains("Agent turn budget"), "{output}");
    assert!(!output.contains("Continue"), "{output}");
}
