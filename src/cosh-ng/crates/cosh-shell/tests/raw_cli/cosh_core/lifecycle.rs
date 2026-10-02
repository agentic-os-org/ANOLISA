use super::*;

#[test]
fn raw_cli_cosh_core_malformed_provider_event_failure_is_contained() {
    let home = temp_shell_home("cosh-core-malformed-provider");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-malformed","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-malformed-provider-event*)
    printf '%s\n' '{"type":"assistant","session_id":'
    printf '%s\n' 'cosh-core malformed provider fixture stderr' >&2
    exit 17
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-malformed","is_error":false,"result":"ignored"}'
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
                b"?? cosh-core-malformed-provider-event\n".as_slice(),
            ),
            (
                "cosh-core malformed provider fixture stderr",
                b"echo after-malformed-provider\n".as_slice(),
            ),
            ("after-malformed-provider", b"exit\n".as_slice()),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("cosh-core malformed provider fixture stderr"),
        "{output}"
    );
    assert!(!output.contains("Agent question unavailable"), "{output}");
    assert!(!output.contains(r#"{"type":"assistant""#), "{output}");
    assert!(output.contains("after-malformed-provider"), "{output}");
    assert!(
        !output.contains("bash: cosh-core-malformed-provider-event: command not found"),
        "{output}"
    );
    assert!(!output.contains("Agent timed out:"), "{output}");
}

#[test]
fn raw_cli_cosh_core_dumb_terminal_uses_plain_blocks() {
    let home = temp_shell_home("cosh-core-dumb-terminal");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-dumb","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-dumb-terminal*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-dumb","message":{"content":[{"type":"text","text":"Cosh-core dumb terminal response."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-dumb","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-dumb","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            ("NO_COLOR", "1"),
            ("TERM", "dumb"),
        ],
        vec![
            (b"?? cosh-core-dumb-terminal\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Cosh-core dumb terminal response."),
        "{output}"
    );
    assert!(output.contains("Agent:"), "{output}");
    assert!(!output.contains("Agent status:"), "{output}");
    assert!(!output.contains('╭'), "{output}");
    assert!(!output.contains('│'), "{output}");
    assert!(!output.contains('╰'), "{output}");
}

#[test]
fn raw_cli_cosh_core_cancel_then_exit_cleans_up_active_provider_process() {
    let home = temp_shell_home("cosh-core-process-cleanup");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    let pid_file = home.join("cosh-core.pid");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
printf '%s\n' "$$" > "$COSH_CORE_PID_FILE"
trap 'exit 0' TERM INT HUP
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-process-cleanup","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-process-cleanup*)
    sleep 60
    ;;
esac
sleep 60
"#,
    );

    let binary = env!("CARGO_BIN_EXE_cosh-shell");
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let pid_file_str = pid_file.to_string_lossy().to_string();
    let mut child = Command::new(binary)
        .args(["raw", "cosh-core"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", &home_str)
        .env("COSH_CORE_PATH", &cosh_core_path_str)
        .env("COSH_CORE_PID_FILE", &pid_file_str)
        .env("COSH_SHELL_ISOLATED", "1")
        .env("COSH_SHELL_INTEGRATION", "enhanced")
        .env("COSH_SHELL_RAW_SHELL", "bash")
        .env("COSH_SHELL_DEFAULT_SHELL", "bash")
        .env("COSH_SHELL_LANG", "en-US")
        .env("COSH_SHELL_BOOTSTRAP_PATH", "0")
        .process_group(0)
        .spawn()
        .expect("spawn cosh-shell raw");
    let raw_pid = child.id();
    let mut stdin = child.stdin.take().expect("child stdin");
    let writer_pid_file = pid_file.clone();
    let writer = thread::spawn(move || {
        stdin
            .write_all(b"?? cosh-core-process-cleanup\n")
            .expect("write prompt");
        stdin.flush().expect("flush prompt");
        for _ in 0..100 {
            if writer_pid_file.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        stdin.write_all(b"/cancel\n").expect("write cancel");
        stdin.flush().expect("flush cancel");
        thread::sleep(Duration::from_millis(1_000));
        stdin.write_all(b"exit\n").expect("write exit");
        stdin.flush().expect("flush exit");
    });

    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("wait raw process cleanup")
        .unwrap_or_else(|| {
            if let Ok(pid_text) = fs::read_to_string(&pid_file) {
                if let Ok(provider_pid) = pid_text.trim().parse::<u32>() {
                    signal_pid(provider_pid, "TERM");
                    thread::sleep(Duration::from_millis(100));
                    signal_pid(provider_pid, "KILL");
                }
            }
            signal_pid(raw_pid, "TERM");
            signal_process_group(raw_pid, "TERM");
            thread::sleep(Duration::from_millis(100));
            signal_pid(raw_pid, "KILL");
            signal_process_group(raw_pid, "KILL");
            panic!("cosh-shell raw did not exit after cancelling active provider")
        });
    writer.join().expect("join writer");
    let output = child.wait_with_output().expect("collect output");
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let provider_pid = read_pid_file_with_retry(&pid_file);

    for _ in 0..20 {
        if !process_is_alive(provider_pid) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let alive = process_is_alive(provider_pid);
    if alive {
        signal_pid(provider_pid, "TERM");
        thread::sleep(Duration::from_millis(100));
        signal_pid(provider_pid, "KILL");
    }
    let _ = fs::remove_dir_all(&home);

    assert!(status.success(), "status={status:?}\n{text}");
    assert!(text.contains("cosh-core-process-cleanup"), "{text}");
    assert!(!alive, "provider pid {provider_pid} survived\n{text}");
}

#[test]
fn raw_cli_cosh_core_completed_run_exit_leaves_no_provider_process() {
    let home = temp_shell_home("cosh-core-process-cleanup-completed");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    let pid_file = home.join("cosh-core.pid");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
printf '%s\n' "$$" > "$COSH_CORE_PID_FILE"
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-process-cleanup-completed","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-process-cleanup-completed*)
    printf '%s\n' '{"type":"assistant","session_id":"sess-cosh-core-process-cleanup-completed","message":{"content":[{"type":"text","text":"Cosh-core cleanup completed run."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-process-cleanup-completed","is_error":false,"result":"done"}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-process-cleanup-completed","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let pid_file_str = pid_file.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &cosh_core_path_str),
            ("COSH_CORE_PID_FILE", &pid_file_str),
        ],
        vec![
            (
                b"?? cosh-core-process-cleanup-completed\n".to_vec(),
                Duration::ZERO,
            ),
            (b"exit\n".to_vec(), Duration::from_millis(2_500)),
        ],
    );
    let provider_pid = read_pid_file_with_retry(&pid_file);
    let alive = process_is_alive(provider_pid);
    if alive {
        signal_pid(provider_pid, "TERM");
        thread::sleep(Duration::from_millis(100));
        signal_pid(provider_pid, "KILL");
    }
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Cosh-core cleanup completed run."),
        "{output}"
    );
    assert!(!alive, "provider pid {provider_pid} survived\n{output}");
}

#[test]
fn raw_cli_cosh_core_host_executed_provider_disconnect_marks_recovery_reason() {
    let home = temp_shell_home("cosh-core-host-executed-disconnect");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-cosh-core-host-executed-disconnect","model":"cosh-core-test"}'
read -r user_message
case "$user_message" in
  *cosh-core-provider-host-executed-disconnect*)
    printf '%s\n' '{"type":"control_request","request_id":"ctrl-cosh-core-disconnect","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"sleep 1"},"tool_use_id":"toolu-cosh-core-disconnect"}}'
    exit 0
    ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-cosh-core-host-executed-disconnect","is_error":false,"result":"ignored"}'
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let cosh_core_path_str = cosh_core_path.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "cosh-core",
        &[],
        &[("HOME", &home_str), ("COSH_CORE_PATH", &cosh_core_path_str)],
        vec![
            (b"/mode approval trust confirm\n".to_vec(), Duration::ZERO),
            (
                b"?? cosh-core-provider-host-executed-disconnect\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"\n".to_vec(), Duration::from_millis(2_000)),
            (
                b"/details handoff-1\n".to_vec(),
                Duration::from_millis(2_500),
            ),
            (b"/debug session\n".to_vec(), Duration::from_millis(1_000)),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.contains("Approved req-1") || output.contains("Auto-approved req-1"),
        "{output}"
    );
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("$ sleep 1"), "{output}");
    assert!(
        output.contains("selected_shell_execution_path: foreground_shell_handoff_recovery"),
        "{output}"
    );
    assert!(
        output.contains("provider_result_delivery_status: provider_run_not_active")
            || output.contains("provider_result_delivery_status: provider_channel_closed"),
        "{output}"
    );
    assert!(
        output.contains("recovery_reason: provider run was not active")
            || output.contains("recovery_reason: provider approval channel closed"),
        "{output}"
    );
    assert!(
        output.contains("latest recovery status: provider_run_not_active")
            || output.contains("latest recovery status: provider_channel_closed"),
        "{output}"
    );
    assert!(
        output.contains("latest recovery reason: resume_failed"),
        "{output}"
    );
    assert!(
        output.contains("latest provider request: ctrl-cosh-core-disconnect"),
        "{output}"
    );
    assert!(
        output.contains("latest tool use id: toolu-cosh-core-disconnect"),
        "{output}"
    );
    assert!(
        !output.contains("control_protocol_host_executed_shell_result"),
        "{output}"
    );
    assert!(
        !output.contains("bash: cosh-core-provider-host-executed-disconnect: command not found"),
        "{output}"
    );
}
