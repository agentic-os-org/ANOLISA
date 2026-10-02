use super::*;

#[test]
fn raw_cli_cosh_request_history_auto_sends_history_index() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"echo before-history\n".to_vec(),
                Duration::from_millis(800),
            ),
            (
                b"?? request shell history evidence\n".to_vec(),
                Duration::from_millis(600),
            ),
            (
                b"/details cosh-request-1\n".to_vec(),
                Duration::from_millis(600),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(600)),
        ],
    );

    assert!(output.contains("before-history"), "{output}");
    assert!(!output.contains("Agent Requested Evidence"), "{output}");
    assert!(
        output.contains("Evidence history index received by fake adapter."),
        "{output}"
    );
    assert!(output.contains("cosh-request details"), "{output}");
    assert!(output.contains("request_id: cosh-request-1"), "{output}");
    assert!(output.contains("outcome: parsed"), "{output}");
    assert!(output.contains("reason: parsed"), "{output}");
    assert!(output.contains("raw_block:"), "{output}");
    assert!(
        compact_terminal_words(&output).contains("```cosh-requesthistory```"),
        "{output}"
    );
    assert!(
        !output.contains("bash: request shell history evidence"),
        "{output}"
    );
}

#[test]
fn raw_cli_cosh_request_history_omits_secret_without_confirmation() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"echo token=super-secret\n".to_vec(), Duration::ZERO),
            (
                b"?? request shell history evidence\n".to_vec(),
                Duration::from_millis(800),
            ),
            (b"\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit\n".to_vec(), Duration::from_millis(600)),
        ],
    );

    assert!(!output.contains("Agent Requested Evidence"), "{output}");
    assert!(
        output.contains("Evidence history index received by fake adapter."),
        "{output}"
    );
    assert!(output.contains("token=super-secret"), "{output}");
    assert!(!output.contains("bounded_output_excerpt:"), "{output}");
    assert!(!output.contains("/output-refs/"), "{output}");
}

#[test]
fn raw_cli_cosh_request_output_card_sends_bounded_excerpt() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"printf 'alpha\\nbeta\\ngamma\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? request captured output evidence\n".to_vec(),
                Duration::from_millis(800),
            ),
            (b"\n".to_vec(), Duration::from_millis(1_500)),
            (b"exit\n".to_vec(), Duration::from_millis(600)),
        ],
    );

    assert!(output.contains("alpha"), "{output}");
    assert!(output.contains("beta"), "{output}");
    assert!(output.contains("gamma"), "{output}");
    assert!(output.contains("Agent Requested Evidence"), "{output}");
    assert!(
        output.contains("Agent wants to inspect captured output:"),
        "{output}"
    );
    assert!(
        output.contains("terminal-output://raw-session-"),
        "{output}"
    );
    assert!(output.contains("/cmd-1 tail"), "{output}");
    assert!(
        !output.contains(
            "Agent wants to inspect captured output: terminal-output://raw-session/cmd-1 tail"
        ),
        "{output}"
    );
    assert!(output.contains("Max lines: 2"), "{output}");
    assert!(
        output.contains("Evidence excerpt received by fake adapter: beta gamma"),
        "{output}"
    );
    assert!(!output.contains("```cosh-request"), "{output}");
    assert!(!output.contains("/output-refs/"), "{output}");
}

#[test]
fn raw_cli_cosh_request_card_ignore_keeps_shell_usable() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"printf 'alpha\\nbeta\\ngamma\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? request captured output evidence\n".to_vec(),
                Duration::from_secs(1),
            ),
            (b"i\n".to_vec(), Duration::from_millis(1_500)),
            (
                b"echo after-evidence-ignore\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert!(output.contains("Agent Requested Evidence"), "{output}");
    assert!(
        output.contains("Ignored this evidence request."),
        "{output}"
    );
    assert!(output.contains("after-evidence-ignore"), "{output}");
    assert!(
        !output.contains("Evidence excerpt received by fake adapter:"),
        "{output}"
    );
    assert!(!output.contains("bash: i"), "{output}");
    assert!(!output.contains("cosh-osc$ cosh-osc$"), "{output}");
}

#[test]
fn raw_cli_cosh_request_card_ctrl_c_cancels_only_evidence_request() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"printf 'alpha\\nbeta\\ngamma\\n'\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"?? request captured output evidence\n".to_vec(),
                Duration::from_secs(1),
            ),
            (vec![0x03], Duration::from_millis(2_000)),
            (
                b"echo after-evidence-cancel\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert!(output.contains("Agent Requested Evidence"), "{output}");
    assert!(output.contains("after-evidence-cancel"), "{output}");
    assert!(
        !output.contains("Evidence excerpt received by fake adapter:"),
        "{output}"
    );
    assert!(!output.contains("Agent cancellation requested"), "{output}");
    assert!(
        !output.contains("Reason: user requested cancellation"),
        "{output}"
    );
    assert!(!output.contains("cosh-osc$ cosh-osc$"), "{output}");
}

#[test]
fn raw_cli_terminal_output_read_misroute_records_details_audit() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"printf 'misroute-output\\n'\n".to_vec(), Duration::ZERO),
            (
                b"?? misroute terminal output read\n".to_vec(),
                Duration::from_secs(1),
            ),
            (b"/details tool-1\n".to_vec(), Duration::from_millis(1_200)),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert!(output.contains("misroute-output"), "{output}");
    assert!(output.contains("Activity details tool-1"), "{output}");
    assert!(
        output.contains("virtual_evidence_read_misroute: true"),
        "{output}"
    );
    assert!(
        output.contains("misrouted_output_id: terminal-output://raw-session-"),
        "{output}"
    );
    assert!(
        output.contains("recommended_action: fenced_cosh_request_output"),
        "{output}"
    );
    assert!(!output.contains("raw-session/cmd-1"), "{output}");
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_invalid_cosh_request_is_audited_without_evidence_card() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"?? request invalid shell evidence\n".to_vec(),
                Duration::from_millis(300),
            ),
            (
                b"/details cosh-request-1\n".to_vec(),
                Duration::from_millis(1_500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert!(output.contains("cosh-request details"), "{output}");
    assert!(output.contains("request_id: cosh-request-1"), "{output}");
    assert!(output.contains("outcome: invalid"), "{output}");
    assert!(output.contains("reason: parse_error"), "{output}");
    assert!(
        compact_terminal_words(&output).contains("```cosh-requestread terminal-output://"),
        "{output}"
    );
    assert!(!output.contains("Agent Requested Evidence"), "{output}");
    assert!(
        !output.contains("Evidence excerpt received by fake adapter"),
        "{output}"
    );
    assert!(!output.contains("bash: /details"), "{output}");
}

#[test]
fn raw_cli_fallback_debug_session_reports_fenced_request_access() {
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_DEBUG", "1")],
        vec![
            (b"printf 'fallback-debug\\n'\n".to_vec(), Duration::ZERO),
            (b"/debug session\n".to_vec(), Duration::from_millis(500)),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );

    assert!(output.contains("fallback-debug"), "{output}");
    assert!(
        output.contains("evidence access: fenced_request_fallback"),
        "{output}"
    );
    assert!(
        !output.contains("evidence access: control_protocol_tool"),
        "{output}"
    );
    assert!(!output.contains("bash: /debug"), "{output}");
}
