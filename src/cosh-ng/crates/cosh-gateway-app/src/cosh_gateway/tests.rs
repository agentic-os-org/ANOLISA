use super::*;

use super::acp_command::{abandoned_permission_fields, prompt_exit_code};
use super::control::verify_expected_workspace;

#[cfg(not(target_os = "linux"))]
#[test]
fn web_is_not_advertised_or_accepted_off_linux() {
    use clap::CommandFactory;

    assert!(Cli::command().find_subcommand("web").is_none());
    assert!(Cli::try_parse_from(["cosh-gateway", "web", "--help"]).is_err());
}

#[test]
fn prompt_stop_reasons_map_to_stable_exit_codes() {
    assert_eq!(prompt_exit_code(AcpV1StopReason::EndTurn), 0);
    assert_eq!(prompt_exit_code(AcpV1StopReason::Cancelled), EXIT_CANCELLED);
    for reason in [
        AcpV1StopReason::MaxTokens,
        AcpV1StopReason::MaxTurnRequests,
        AcpV1StopReason::Refusal,
        AcpV1StopReason::Unsupported,
    ] {
        assert_eq!(prompt_exit_code(reason), EXIT_AGENT);
    }
}

#[test]
fn abandoned_permission_report_is_cancelled_and_omits_provider_request_ids() {
    let fields = abandoned_permission_fields(
        "provider-session",
        &[
            AcpV1RequestId::Number(41),
            AcpV1RequestId::String("raw-provider-request-string".to_owned()),
        ],
    );
    let encoded = serde_json::to_string(&fields).unwrap();

    assert_eq!(fields["pending_count"], 2);
    assert_eq!(fields["stop_reason"], "cancelled");
    assert!(!encoded.contains("41"));
    assert!(!encoded.contains("raw-provider-request-string"));
    assert_eq!(prompt_exit_code(AcpV1StopReason::Cancelled), EXIT_CANCELLED);
}

#[cfg(unix)]
#[test]
fn prompt_file_rejects_a_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("prompt.txt");
    let link = directory.path().join("prompt-link.txt");
    std::fs::write(&target, "inspect safely\n").unwrap();
    symlink(&target, &link).unwrap();

    let error = read_prompt(Some(&link)).unwrap_err();
    let CliError::Input(error) = error else {
        panic!("expected prompt input error")
    };
    assert_eq!(error.raw_os_error(), Some(nix::libc::ELOOP));
}

#[test]
fn terminal_text_escapes_control_sequences() {
    assert_eq!(terminal_safe("ok\u{1b}[2J\rnext"), "ok\\u{1b}[2J\\rnext");
}

#[test]
fn workspace_registration_path_is_lexically_normalized() {
    assert_eq!(
        super::serve::normalize_absolute_workspace(Path::new("/work/project/.")).unwrap(),
        Path::new("/work/project")
    );
    assert_eq!(
        super::serve::normalize_absolute_workspace(Path::new("/work/tmp/../project")).unwrap(),
        Path::new("/work/project")
    );
    assert!(super::serve::normalize_absolute_workspace(Path::new("/../../project")).is_err());
}

#[test]
fn task_submit_fails_if_the_confirmed_workspace_changes() {
    let workspace = cosh_gateway_contracts::common::WorkspaceRef {
        scope_digest: cosh_gateway_contracts::common::Digest::parse(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap(),
        display_name: None,
    };

    verify_expected_workspace(
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        &workspace,
    )
    .unwrap();
    let error = verify_expected_workspace(
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        &workspace,
    )
    .unwrap_err();
    assert!(matches!(error, CliError::Profile(_)));
}

#[test]
fn json_observation_fields_include_driver_sequence() {
    assert_eq!(
        with_observation_sequence(7, json!({"text": "chunk"})),
        json!({"sequence": 7, "text": "chunk"})
    );
}

#[test]
fn cli_does_not_accept_prompt_as_an_argument() {
    assert!(Cli::try_parse_from(["cosh-gateway", "run", "secret prompt"]).is_err());
}

#[test]
fn task_submit_does_not_accept_intent_as_an_argument() {
    assert!(Cli::try_parse_from(["cosh-gateway", "task", "submit", "private intent"]).is_err());
}

#[test]
fn task_event_page_is_bounded_by_clap() {
    assert!(Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "events",
        "tsk_00000000-0000-0000-0000-000000000000",
        "--limit",
        "65",
    ])
    .is_err());
}

#[test]
fn task_submit_defaults_to_typed_core_auto_allow_all() {
    let defaults = Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "submit",
        "--idempotency-key",
        "stable-submit-key",
    ])
    .unwrap();
    let Command::Task(TaskArgs {
        command: TaskCommand::Submit(defaults),
        ..
    }) = defaults.command
    else {
        panic!("expected task submit command");
    };
    assert!(matches!(defaults.runtime, TaskRuntimeArg::Core));
    assert!(matches!(defaults.checkpoint, CheckpointArg::Auto));
    assert!(matches!(
        defaults.approval_policy,
        ApprovalPolicyArg::AllowAll
    ));

    let explicit = Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "submit",
        "--idempotency-key",
        "explicit-codex-key",
        "--runtime",
        "codex",
        "--checkpoint",
        "on",
        "--approval-policy",
        "allow-all",
    ])
    .unwrap();
    let Command::Task(TaskArgs {
        command: TaskCommand::Submit(explicit),
        ..
    }) = explicit.command
    else {
        panic!("expected explicit task submit command");
    };
    assert!(matches!(explicit.runtime, TaskRuntimeArg::Codex));
    assert!(matches!(explicit.checkpoint, CheckpointArg::On));
    assert!(matches!(
        explicit.approval_policy,
        ApprovalPolicyArg::AllowAll
    ));
    assert!(Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "submit",
        "--idempotency-key",
        "raw-profile-key",
        "--runtime-profile",
        "codex",
    ])
    .is_err());
}

#[test]
fn task_submit_rejects_a_noncanonical_expected_workspace_digest() {
    assert!(Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "submit",
        "--idempotency-key",
        "stable-submit-key",
        "--expected-workspace-digest",
        "NOT-A-DIGEST",
    ])
    .is_err());
}

#[test]
fn task_capabilities_is_a_first_class_command() {
    let cli =
        Cli::try_parse_from(["cosh-gateway", "task", "--output", "jsonl", "capabilities"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Task(TaskArgs {
            output: Output::Jsonl,
            command: TaskCommand::Capabilities,
            ..
        })
    ));
}

#[test]
fn task_list_has_a_small_bounded_default() {
    let cli = Cli::try_parse_from(["cosh-gateway", "task", "list"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Task(TaskArgs {
            command: TaskCommand::List(TaskListArgs { limit: 20 }),
            ..
        })
    ));
    assert!(Cli::try_parse_from(["cosh-gateway", "task", "list", "--limit", "65"]).is_err());
}

#[test]
fn task_submit_rejects_hand_assembled_target_flags() {
    for removed in [
        vec!["--target-kind", "workspace"],
        vec!["--target-authority", "ws-ckpt"],
        vec!["--target", "checkpoint-create-v1"],
    ] {
        let parsed = Cli::try_parse_from(
            [
                "cosh-gateway",
                "task",
                "submit",
                "--idempotency-key",
                "fixed-target-key",
            ]
            .into_iter()
            .chain(removed.iter().copied()),
        );
        assert!(
            parsed.is_err(),
            "removed target flags must not parse: {removed:?}"
        );
    }
}

#[test]
fn task_approval_decision_needs_no_internal_ledger_revision() {
    let cli = Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "resolve-approval",
        "apr_00000000-0000-0000-0000-000000000000",
        "--decision",
        "approve",
        "--idempotency-key",
        "stable-approval-key",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Command::Task(TaskArgs {
            command: TaskCommand::ResolveApproval(TaskResolveApprovalArgs {
                decision: ApprovalChoice::Approve,
                ..
            }),
            ..
        })
    ));
    assert!(Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "resolve-approval",
        "apr_00000000-0000-0000-0000-000000000000",
        "--decision",
        "approve",
        "--idempotency-key",
        "stable-approval-key",
        "--expected-revision",
        "1",
    ])
    .is_err());
}

#[test]
fn task_append_parses_exact_input_identity_and_bounded_options() {
    let cli = Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "append",
        "tsk_00000000-0000-0000-0000-000000000000",
        "--input-request-id",
        "inp_00000000-0000-0000-0000-000000000001",
        "--select",
        "0",
        "--select",
        "2",
        "--idempotency-key",
        "stable-input-key",
        "--expected-revision",
        "5",
    ])
    .unwrap();
    let Command::Task(TaskArgs {
        command: TaskCommand::Append(append),
        ..
    }) = cli.command
    else {
        panic!("expected task append command");
    };
    assert_eq!(
        append.input_request_id,
        "inp_00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(append.selections, vec![0, 2]);
    assert_eq!(append.expected_revision, Some(5));

    assert!(Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "append",
        "tsk_00000000-0000-0000-0000-000000000000",
        "--input-request-id",
        "inp_00000000-0000-0000-0000-000000000001",
        "--input-file",
        "/tmp/input",
        "--select",
        "0",
        "--idempotency-key",
        "conflicting-input-source",
    ])
    .is_err());
}

#[test]
fn task_retry_requires_exact_previous_run_and_stable_key() {
    let cli = Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "retry",
        "tsk_00000000-0000-0000-0000-000000000000",
        "--previous-run-id",
        "run_00000000-0000-0000-0000-000000000001",
        "--idempotency-key",
        "stable-retry-key",
        "--expected-revision",
        "4",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Command::Task(TaskArgs {
            command: TaskCommand::Retry(TaskRetryArgs {
                expected_revision: Some(4),
                ..
            }),
            ..
        })
    ));
    assert!(Cli::try_parse_from([
        "cosh-gateway",
        "task",
        "retry",
        "tsk_00000000-0000-0000-0000-000000000000",
        "--idempotency-key",
        "stable-retry-key",
    ])
    .is_err());
}
