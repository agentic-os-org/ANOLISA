use super::*;

#[test]
fn recent_shell_evidence_summary_suppresses_same_tail_read() {
    let mut state = ShellEvidenceState::default();
    state.record_host_executed_shell_output(
        "terminal-output://session-1/cmd-1".to_string(),
        Some("run-1".to_string()),
        true,
    );

    assert!(state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));
}

#[test]
fn recent_shell_evidence_allows_expansion_and_run_miss() {
    let mut state = ShellEvidenceState::default();
    state.record_shell_evidence_read_output(
        "terminal-output://session-1/cmd-1".to_string(),
        Some("run-1".to_string()),
        "tail".to_string(),
        80,
    );

    assert!(!state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));
    assert!(!state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-2"),
        "tail",
        120,
    ));
}

#[test]
fn recent_shell_evidence_incomplete_summary_does_not_suppress_default_read() {
    let mut state = ShellEvidenceState::default();
    state.record_host_executed_shell_output(
        "terminal-output://session-1/cmd-1".to_string(),
        Some("run-1".to_string()),
        false,
    );

    assert!(!state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));
}

#[test]
fn recent_shell_evidence_excerpt_coverage_controls_suppression() {
    let mut state = ShellEvidenceState::default();
    state.record_shell_evidence_read_output(
        "terminal-output://session-1/cmd-1".to_string(),
        Some("run-1".to_string()),
        "tail".to_string(),
        80,
    );

    assert!(state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        40,
    ));
    assert!(!state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));
}

#[test]
fn recent_shell_evidence_excerpt_query_ignores_summary_coverage() {
    let mut state = ShellEvidenceState::default();
    state.record_host_executed_shell_output(
        "terminal-output://session-1/cmd-1".to_string(),
        Some("run-1".to_string()),
        true,
    );

    assert!(state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));
    assert!(!state.read_output_excerpt_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));

    state.record_shell_evidence_read_output(
        "terminal-output://session-1/cmd-1".to_string(),
        Some("run-1".to_string()),
        "tail".to_string(),
        120,
    );
    assert!(state.read_output_excerpt_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));
}

#[test]
fn recent_shell_evidence_window_evicts_oldest_and_clear_resets() {
    let mut state = ShellEvidenceState::default();
    for index in 1..=RECENT_SHELL_TOOL_OUTPUT_WINDOW + 1 {
        state.record_host_executed_shell_output(
            format!("terminal-output://session-1/cmd-{index}"),
            Some("run-1".to_string()),
            true,
        );
    }

    assert!(!state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-1",
        Some("run-1"),
        "tail",
        120,
    ));
    assert!(state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-6",
        Some("run-1"),
        "tail",
        120,
    ));

    state.clear_recent_shell_tool_outputs();
    assert!(!state.read_output_recently_delivered(
        "terminal-output://session-1/cmd-6",
        Some("run-1"),
        "tail",
        120,
    ));
}
