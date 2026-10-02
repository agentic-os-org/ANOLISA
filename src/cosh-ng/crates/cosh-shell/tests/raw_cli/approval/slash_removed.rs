use super::*;

#[test]
fn raw_cli_allow_is_removed_and_does_not_record_recommendation_approval() {
    let output = run_raw_cli_with_input(
        "fake",
        "ls /path/that/does/not/exist\n\
         /allow 2\n\
         echo after-allow\n\
         exit\n",
    );

    assert!(!output.contains("Unknown slash command: /allow"));
    assert!(!output.contains("Use /help to see available commands."));
    assert!(output.contains("Command removed"), "{output}");
    assert!(
        output.contains("/allow is no longer a supported input command."),
        "{output}"
    );
    assert!(
        output.contains("Use the approval card buttons instead; nothing was sent to the shell."),
        "{output}"
    );
    assert!(!output.contains("/allow N records"));
    assert!(!output.contains("Approved recommendation 2"));
    assert!(!output.contains("Governance: approval recorded"));
    assert!(output.contains("after-allow"));
    assert!(!output.contains("/.cargo/bin"));
    assert!(!output.contains("bash: /allow"));
}

#[test]
fn raw_cli_approve_slash_is_not_recommendation_or_governance_alias() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (
                b"ls /path/that/does/not/exist\n/explain last error\n".to_vec(),
                Duration::ZERO,
            ),
            (
                b"/approve 2\n/deny 2\necho after-approve-slash\nexit\n".to_vec(),
                Duration::from_millis(500),
            ),
        ],
    );

    assert!(
        output.contains("The command ls /path/that/does/not/exist failed"),
        "{output}"
    );
    assert!(!output.contains("[Analyze] [Dismiss]"), "{output}");
    assert!(!output.contains("[Details] cmd-"), "{output}");
    assert!(!output.contains("Approved recommendation 2"));
    assert!(!output.contains("Governance: approval recorded"));
    assert!(!output.contains("/.cargo/bin"));
    assert!(output.contains("after-approve-slash"));
    assert!(
        output.contains("/approve is no longer a supported input command."),
        "{output}"
    );
    assert!(
        output.contains("/deny is no longer a supported input command."),
        "{output}"
    );
    assert!(
        output.contains("Use the approval card buttons instead; nothing was sent to the shell."),
        "{output}"
    );
    assert!(!output.contains("bash: /approve"), "{output}");
    assert!(!output.contains("bash: /deny"), "{output}");
}
