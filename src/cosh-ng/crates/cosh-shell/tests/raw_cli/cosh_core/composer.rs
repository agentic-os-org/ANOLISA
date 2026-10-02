use super::*;

fn run_agent_composer_steps(test_name: &str, steps: &[(&str, &[u8])]) -> (String, String) {
    let home = temp_shell_home(test_name);
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    let request_log = home.join("request.log");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
if [ "$1" = "--registry" ]; then
    read -r registry_request
    printf '%s\n' '{"success":true,"data":[{"name":"repo-review","level":"project","disabled":false}]}'
    exit 0
fi
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
read -r user_message
printf '%s\n' "$user_message" > "$COSH_CORE_REQUEST_LOG"
printf '%s\n' '{"type":"system","subtype":"init","session_id":"77777777-7777-4777-8777-777777777777","session_resumable":true,"model":"cosh-core-test","tools":[]}'
printf '%s\n' '{"type":"assistant","session_id":"77777777-7777-4777-8777-777777777777","message":{"content":[{"type":"text","text":"COSH CORE COMPOSER FINAL"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"77777777-7777-4777-8777-777777777777","is_error":false,"result":"done"}'
"#,
    );

    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let request_log_str = request_log.to_string_lossy().to_string();
    let mut input_steps = vec![("cosh-osc$", b"/agent\n".as_slice())];
    input_steps.extend_from_slice(steps);
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "cosh-core",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &core_str),
            ("COSH_CORE_REQUEST_LOG", &request_log_str),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_SHELL_LANG", "en-US"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &input_steps,
    );
    let request = fs::read_to_string(&request_log).expect("cosh-core request log");
    let _ = fs::remove_dir_all(&home);

    (output, request)
}

fn run_agent_composer(test_name: &str, input: &[u8]) -> (String, String) {
    run_agent_composer_steps(
        test_name,
        &[
            ("Agent Composer", input),
            ("COSH CORE COMPOSER FINAL", b"exit\n"),
        ],
    )
}

#[test]
fn raw_cli_agent_composer_sends_validated_references_and_skill_to_cosh_core() {
    let (output, request) = run_agent_composer(
        "cosh-core-agent-composer",
        b"/skill:repo-review inspect @Cargo.toml @src @../Cargo.toml\r",
    );

    assert!(output.contains("Runtime: cosh-core"), "{output}");
    assert!(
        output.contains("◆ "),
        "Agent must own composer input: {output}"
    );
    assert!(output.contains("COSH CORE COMPOSER FINAL"), "{output}");
    assert!(output.contains("References skipped"), "{output}");
    assert!(output.contains("\"../Cargo.toml\""), "{output}");
    assert!(
        output.contains("invalid workspace-relative path"),
        "{output}"
    );
    assert!(request.contains("agent_composer:"), "{request}");
    assert!(
        request.contains("selected_skill: \\\"repo-review\\\""),
        "{request}"
    );
    assert!(request.contains("- file: \\\"Cargo.toml\\\""), "{request}");
    assert!(request.contains("- directory: \\\"src\\\""), "{request}");
    assert!(request.contains("rejected_reference_count: 1"), "{request}");
    assert!(!request.contains("__cosh_agent_composer="), "{request}");
}

#[test]
fn raw_cli_plain_agent_composer_request_omits_empty_metadata() {
    let (output, request) = run_agent_composer(
        "cosh-core-agent-composer-plain",
        b"analyze this code without references\r",
    );

    assert!(output.contains("COSH CORE COMPOSER FINAL"), "{output}");
    assert!(request.contains("analyze this code without references"));
    assert!(!request.contains("agent_composer:"), "{request}");
    assert!(!request.contains("References below were explicitly selected"));
}

#[test]
fn raw_cli_agent_composer_completes_a_registry_skill_before_submit() {
    let (output, request) = run_agent_composer_steps(
        "cosh-core-agent-composer-skill-completion",
        &[
            ("Agent Composer", b"/skill"),
            ("› /skill:repo-review", b"\tinspect this workspace\r"),
            ("COSH CORE COMPOSER FINAL", b"exit\n"),
        ],
    );

    assert!(output.contains("› /skill:repo-review"), "{output}");
    assert!(
        output.contains("/skill:repo-review inspect this workspace"),
        "{output}"
    );
    assert!(
        request.contains("selected_skill: \\\"repo-review\\\""),
        "{request}"
    );
}
