use super::*;

#[test]
fn raw_cli_approval_text_input_does_not_confirm_or_leak_to_bash() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"exit\n".to_vec(), Duration::from_millis(5_000)),
        (b"\x1b".to_vec(), Duration::from_millis(200)),
        (b"exit\n".to_vec(), Duration::from_millis(200)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("tool request") && output.contains("medium risk"));
    assert!(output.contains("Cancelled"));
    assert!(output.contains("$ git status"));
    assert!(!output.contains("No command ran."));
    assert!(!output.contains("tool request - cancelled by user"));
    assert!(!output.contains("Approved"));
    assert!(!output.contains("Decision: approved"));
    assert_eq!(count_occurrences(&output, "cosh-osc$ exit"), 1, "{output}");
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_approval_split_arrow_sequence_does_not_cancel() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"\x1b[".to_vec(), Duration::from_millis(5_000)),
        (b"C".to_vec(), Duration::from_millis(50)),
        (b"\x1b[C".to_vec(), Duration::from_millis(50)),
        (b"\n".to_vec(), Duration::from_millis(100)),
        (b"exit\n".to_vec(), Duration::from_millis(1_000)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("tool request") && output.contains("medium risk"));
    assert!(
        output.contains("> [ Deny ]") || output.contains("[Deny]"),
        "{output}"
    );
    assert!(output.contains("Denied"));
    assert!(output.contains("$ git status --short"));
    assert!(!output.contains("No command ran."));
    assert!(!output.contains("Bash tool - denied"));
    assert!(!output.contains("Cancelled"));
    assert!(!output.contains("Approved"));
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_approval_application_cursor_arrow_updates_focus() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"\x1bOC".to_vec(), Duration::from_millis(5_000)),
        (b"\x1bOC".to_vec(), Duration::from_millis(100)),
        (b"\n".to_vec(), Duration::from_millis(100)),
        (b"exit\n".to_vec(), Duration::from_millis(1_000)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("tool request") && output.contains("medium risk"));
    assert!(
        output.contains("> [ Deny ]") || output.contains("[Deny]"),
        "{output}"
    );
    assert!(output.contains("Denied"));
    assert!(output.contains("$ git status --short"));
    assert!(!output.contains("No command ran."));
    assert!(!output.contains("Bash tool - denied"));
    assert!(!output.contains("Cancelled"));
    assert!(!output.contains("Approved"));
    assert!(!output.contains("bash:"));
}

/// Turn-scope batch consent (issue #1773): a multi-request turn offers
/// "Allow all this turn" and selecting it sweeps the queued requests
/// through the same resolution pipeline without further cards.
#[test]
fn raw_cli_turn_batch_consent_sweeps_queued_requests() {
    assert_turn_batch_consent_serializes_handoffs(&[]);
}

#[test]
fn raw_cli_zsh_turn_batch_consent_serializes_handoffs() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    assert_turn_batch_consent_serializes_handoffs(&["--shell", "zsh"]);
}

fn assert_turn_batch_consent_serializes_handoffs(args: &[&str]) {
    let output = run_raw_cli_ask_with_args_and_marker_input(
        args,
        &[
            ("cosh-osc$", b"?? stream batch tool approval\n"),
            // Move to "Allow all this turn" (index 1) once the card is up.
            ("Approval req-1", b"\x1b[C\n"),
            (
                "Command result analysis for req-3",
                b"?? batch-handoff-follow-up\n",
            ),
            (
                "Received shell prompt request: ?? batch-handoff-follow-up",
                b"exit\n",
            ),
        ],
    );
    let normalized = strip_ansi_escape(&output).replace('\r', "");

    assert!(output.contains("Allow all this turn"), "{output}");
    assert!(output.contains("Approved for this turn req-1"), "{output}");
    assert!(output.contains("Approved for this turn req-2"), "{output}");
    assert!(output.contains("Approved for this turn req-3"), "{output}");
    // The swept requests never present their own cards.
    assert!(!output.contains("Approval req-2"), "{output}");
    assert!(!output.contains("Approval req-3"), "{output}");
    assert!(!output.contains("Blocked req-3"), "{output}");
    assert!(normalized.contains("\nalpha\nbeta\n"), "{output}");
    assert!(
        output.contains("Received shell prompt request: ?? batch-handoff-follow-up"),
        "a follow-up Agent run must start after every batched handoff closes: {output}"
    );
    assert!(!output.contains("bash:"));
}

/// Single-request turns keep the standard card contract: no turn-scope
/// batch action is offered (issue #1773 zero-noise guarantee).
#[test]
fn raw_cli_single_request_turn_keeps_standard_actions() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"\n".to_vec(), Duration::from_millis(5_000)),
        (b"exit\n".to_vec(), Duration::from_millis(1_000)),
    ]);

    assert_approval_prompt_visible(&output);
    assert!(output.contains("Always trust"), "{output}");
    assert!(!output.contains("Allow all this turn"), "{output}");
    assert!(!output.contains("Approved for this turn"), "{output}");
}
