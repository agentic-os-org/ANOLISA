use super::*;
use crate::types::{CommandStatus, OutputRefs};

fn block(exit_code: i32, command: &str) -> CommandBlock {
    CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session".to_string(),
        command: command.to_string(),
        origin: CommandOrigin::UserInteractive,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 100,
        ended_at_ms: 200,
        duration_ms: 100,
        exit_code,
        status: if exit_code == 0 {
            CommandStatus::Completed
        } else {
            CommandStatus::Failed
        },
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    }
}

fn class(exit_code: i32, command: &str, output: Option<&str>) -> FailureClass {
    classify_failure(&block(exit_code, command), &[], output).class
}

fn reason_trace(exit_code: i32, command: &str, output: Option<&str>) -> String {
    format!(
        "{:?}",
        classify_failure(&block(exit_code, command), &[], output).reasons
    )
}

#[test]
fn usage_help_exit_two_is_not_generic_failure() {
    assert_eq!(
        class(
            2,
            "demo --bad",
            Some("error: unexpected argument '--bad'\nUsage: demo [OPTIONS]\n")
        ),
        FailureClass::UsageOrHelp
    );
}

#[test]
fn exit_two_without_output_is_unknown() {
    assert_eq!(class(2, "demo --bad", None), FailureClass::UnknownFailure);
}

#[test]
fn real_test_failure_with_usage_footer_stays_test_failure() {
    assert_eq!(
        class(
            2,
            "cargo test",
            Some("test result: FAILED. 1 failed\nUsage: fake-test [OPTIONS]\n")
        ),
        FailureClass::BuildOrTestFailure
    );
}

#[test]
fn explicit_parse_error_wins_over_generic_failure_words() {
    assert_eq!(
        class(
            2,
            "cargo test --bad-flag",
            Some("error: unexpected argument '--bad-flag'\nUsage: cargo test [OPTIONS]\n")
        ),
        FailureClass::UsageOrHelp
    );
}

#[test]
fn expected_nonzero_commands_are_classified_as_no_result() {
    assert_eq!(
        class(1, "grep missing file.txt", Some("")),
        FailureClass::ExpectedNoResult
    );
    assert_eq!(
        class(1, "diff a b", Some("1c1\n< a\n---\n> b\n")),
        FailureClass::ExpectedNoResult
    );
}

#[test]
fn reserved_shell_failures_remain_actionable() {
    assert_eq!(class(127, "nope", Some("")), FailureClass::CommandNotFound);
    assert_eq!(
        class(126, "./script", Some("")),
        FailureClass::PermissionDenied
    );
}

#[test]
fn build_and_test_require_matching_family_and_terminal_summary() {
    for (command, output) in [
        ("cargo test", "test result: FAILED. 1 passed; 1 failed\n"),
        ("make all", "make: *** [all] Error 2\n"),
        ("ninja", "ninja: build stopped: subcommand failed.\n"),
        ("mvn test", "[INFO] BUILD FAILURE\n"),
        ("./gradlew test", "BUILD FAILED in 2s\n"),
        ("npm test", "npm ERR! Test failed.\n"),
        ("pytest", "= 1 failed in 0.02s =\n"),
        ("go test ./...", "FAIL\texample.com/project\t0.02s\n"),
    ] {
        assert_eq!(
            class(1, command, Some(output)),
            FailureClass::BuildOrTestFailure,
            "{command}"
        );
    }

    for output in [
        "test result: FAILED. 1 passed; 1 failed\n",
        "make: *** [all] Error 2\n",
        "ninja: build stopped: subcommand failed.\n",
        "[INFO] BUILD FAILURE\n",
        "BUILD FAILED in 2s\n",
        "npm ERR! Test failed.\n",
        "= 1 failed in 0.02s =\n",
        "FAIL\texample.com/project\t0.02s\n",
    ] {
        assert_ne!(
            class(1, "printf fixture", Some(output)),
            FailureClass::BuildOrTestFailure,
            "{output}"
        );
    }

    assert_eq!(
        class(
            101,
            "cargo test",
            Some("error: test failed, to rerun pass `--lib`\n")
        ),
        FailureClass::BuildOrTestFailure
    );
    let trace = reason_trace(
        101,
        "cargo test",
        Some("error: test failed, to rerun pass `--lib`\n"),
    );
    assert!(
        trace.contains("TerminalSignature(CargoTestRerun)"),
        "{trace}"
    );

    for command in [
        "cargo test",
        "make all",
        "ninja",
        "mvn test",
        "./gradlew test",
        "npm test",
        "pytest",
        "go test ./...",
    ] {
        assert_ne!(
            class(
                1,
                command,
                Some("dependency download failed earlier\nfinal status unavailable\n")
            ),
            FailureClass::BuildOrTestFailure,
            "{command}"
        );
    }
}

#[test]
fn build_and_test_family_supports_simple_env_wrappers() {
    for (command, output) in [
        (
            "env LANG=C cargo test",
            "test result: FAILED. 1 passed; 1 failed\n",
        ),
        ("sudo env LANG=C make all", "make: *** [all] Error 2\n"),
    ] {
        assert_eq!(
            class(1, command, Some(output)),
            FailureClass::BuildOrTestFailure,
            "{command}"
        );
    }
}

#[test]
fn build_and_test_family_rejects_sudo_modes_that_change_execution_target() {
    for command in [
        "sudo --shell make all",
        "sudo --login cargo test",
        "sudo --edit Makefile make all",
        "sudo --help make all",
    ] {
        assert_ne!(
            class(1, command, Some("make: *** [all] Error 2\n")),
            FailureClass::BuildOrTestFailure,
            "{command}"
        );
    }
}

#[test]
fn classifier_uses_bounded_head_and_tail() {
    let output = format!(
        "error[E0308]: mismatched types\n{}test result: FAILED. 1 passed; 1 failed\n",
        "middle diagnostic\n".repeat(200)
    );
    assert_eq!(
        class(1, "cargo test", Some(&output)),
        FailureClass::BuildOrTestFailure
    );
    let trace = reason_trace(1, "cargo test", Some(&output));
    assert!(trace.contains("CommandFamily(Cargo)"), "{trace}");
    assert!(trace.contains("TerminalSignature(CargoTest)"), "{trace}");
    assert!(trace.contains("ExcerptDirection(Tail)"), "{trace}");

    let ignored_middle = format!(
        "start\n{}test result: FAILED. 1 passed; 1 failed\n{}end\n",
        "head padding\n".repeat(80),
        "tail padding\n".repeat(80)
    );
    assert_ne!(
        class(1, "cargo test", Some(&ignored_middle)),
        FailureClass::BuildOrTestFailure
    );

    let bounded = BoundedOutput::new(&format!(
        "head diagnostic\n{}tail summary\n",
        "0123456789abcdef\n".repeat(1_000)
    ));
    assert!(bounded.lines.len() <= CLASSIFIER_MAX_LINES);
    let text = bounded.text();
    assert!(text.len() <= CLASSIFIER_MAX_BYTES);
    assert!(text.starts_with("head diagnostic"));
    assert!(text.ends_with("tail summary"));
}

#[test]
fn ordinary_traceback_and_panic_are_runtime_exceptions() {
    let traceback =
        "Traceback (most recent call last):\n  File \"app.py\", line 1\nValueError: boom\n";
    assert_eq!(
        format!("{:?}", class(1, "python app.py", Some(traceback))),
        "RuntimeException"
    );
    let panic = "thread 'main' panicked at src/main.rs:2:5:\nboom\n";
    assert_eq!(
        format!("{:?}", class(101, "./target/debug/app", Some(panic))),
        "RuntimeException"
    );
}

#[test]
fn output_permission_requires_nonzero_terminal_region() {
    let terminal = "deploy: EACCES: permission denied\n";
    assert_eq!(
        class(1, "./deploy", Some(terminal)),
        FailureClass::PermissionDenied
    );
    let trace = reason_trace(1, "./deploy", Some(terminal));
    assert!(
        trace.contains("TerminalSignature(PermissionDenied)"),
        "{trace}"
    );
    assert!(trace.contains("ExcerptDirection(Tail)"), "{trace}");
    assert_eq!(class(0, "./deploy", Some(terminal)), FailureClass::Success);

    let historical = format!(
        "old attempt: permission denied\n{}final request failed\n",
        "ordinary output\n".repeat(100)
    );
    assert_ne!(
        class(1, "./deploy", Some(&historical)),
        FailureClass::PermissionDenied
    );
}

#[test]
fn fatal_and_unknown_signal_semantics_are_fail_closed() {
    for exit_code in [132, 134, 135, 136, 137, 139] {
        assert_eq!(
            class(exit_code, "./crash", Some("core dumped\n")),
            FailureClass::AbnormalSignal,
            "exit {exit_code}"
        );
    }
    assert_eq!(
        class(141, "yes | head -1", Some("")),
        FailureClass::PipelineNormal
    );
    assert_eq!(
        class(130, "sleep 10", Some("")),
        FailureClass::UserInterrupt
    );
    for exit_code in [143, 142, 200] {
        assert_eq!(
            class(exit_code, "./unknown", Some("")),
            FailureClass::UnknownFailure,
            "exit {exit_code}"
        );
    }
    assert_ne!(
        class(1, "./crash", Some("segmentation fault (core dumped)\n")),
        FailureClass::BuildOrTestFailure
    );
}

#[test]
fn unsupported_localized_summary_fails_silent() {
    assert_eq!(
        class(1, "mvn test", Some("构建失败\n")),
        FailureClass::GenericRuntimeFailure
    );
}

#[test]
fn auto_eligibility_preserves_only_legacy_concrete_inputs() {
    for (exit_code, command, output) in [
        (126, "./script", "permission denied\n"),
        (134, "./crash", "core dumped\n"),
        (136, "./crash", "core dumped\n"),
        (137, "./crash", "killed\n"),
        (139, "./crash", "segmentation fault\n"),
        (1, "cargo test", "test result: FAILED. 1 failed\n"),
        (2, "make all", "make: *** [all] Error 2\n"),
        (1, "npm test", "npm ERR! test failed\n"),
        (1, "pytest", "= 1 failed in 0.02s =\n"),
    ] {
        assert_eq!(
            classify_failure(&block(exit_code, command), &[], Some(output)).auto_eligibility,
            FailureAutoEligibility::LegacyAllowlisted,
            "{command} exit={exit_code}"
        );
    }

    for (exit_code, command, output) in [
        (1, "./deploy", "permission denied\n"),
        (132, "./crash", "illegal instruction\n"),
        (135, "./crash", "bus error\n"),
        (1, "ninja", "ninja: build stopped: subcommand failed.\n"),
        (1, "mvn test", "[INFO] BUILD FAILURE\n"),
        (1, "./gradlew test", "BUILD FAILED in 2s\n"),
        (1, "go test ./...", "FAIL\texample.com/project\t0.02s\n"),
        (
            1,
            "python app.py",
            "Traceback (most recent call last):\nValueError: boom\n",
        ),
    ] {
        assert_eq!(
            classify_failure(&block(exit_code, command), &[], Some(output)).auto_eligibility,
            FailureAutoEligibility::SuggestOnly,
            "{command} exit={exit_code}"
        );
    }
}
