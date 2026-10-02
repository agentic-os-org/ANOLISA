use std::time::Duration;

use crate::support_tools::{run_shell_tool, run_tokenized_tool, ToolExecutionStatus};

#[test]
fn readonly_tool_executes_tokenized_command_without_shell() {
    let result = run_tokenized_tool(
        "printf tokenized-output",
        &["printf", "tokenized-output"],
        Duration::from_secs(3),
    );

    assert_eq!(result.status, ToolExecutionStatus::Executed);
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "tokenized-output");
    assert!(result.reason.contains("without a shell"));
}

#[test]
fn readonly_tool_does_not_interpret_shell_syntax_in_tokens() {
    let result = run_tokenized_tool(
        "echo alpha | grep beta",
        &["echo", "alpha", "|", "grep", "beta"],
        Duration::from_secs(3),
    );

    assert_eq!(result.status, ToolExecutionStatus::Executed);
    assert_eq!(result.stdout, "alpha | grep beta\n");
}

#[test]
fn shell_expands_globs_so_verbatim_token_assertions_are_not_vacuous() {
    // Control group for the no-parsing-layer evidence (issue #1882): a
    // real shell in this (non-empty) working directory expands `*`, so
    // the literal-glob assertions of the readonly executors prove the
    // absence of a parsing layer rather than an environment that cannot
    // expand.
    let result = run_shell_tool("echo *", None);

    assert_eq!(result.status, ToolExecutionStatus::Executed);
    assert_ne!(result.stdout, "*\n", "control group must expand the glob");
}

#[test]
fn user_approved_tool_runs_shell_syntax_through_bash() {
    let result = run_shell_tool("printf 'alpha\\nbeta\\n' | grep beta", None);

    assert_eq!(result.status, ToolExecutionStatus::Executed);
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "beta\n");
    assert!(result.reason.contains("bash -lc"));
}

#[test]
fn user_approved_tool_has_no_default_timeout() {
    let result = run_shell_tool("sleep 1; printf done", None);

    assert_eq!(result.status, ToolExecutionStatus::Executed);
    assert_eq!(result.stdout, "done");
}

#[test]
fn user_approved_tool_honors_configured_timeout() {
    let result = run_shell_tool("sleep 2; printf done", Some(Duration::from_millis(50)));

    assert_eq!(result.status, ToolExecutionStatus::TimedOut);
    assert!(result.reason.contains("timed out"));
}
