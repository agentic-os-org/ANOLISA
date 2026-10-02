//! Subprocess coverage for Clap-owned output and diagnostics.

#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::process::Output;
#[cfg(target_os = "linux")]
use std::process::Stdio;

mod common;

fn run(arguments: &[&str]) -> Output {
    common::run(arguments)
}

#[test]
fn help_renders_to_stdout() {
    // Given a request for top-level help.
    let arguments = ["--help"];

    // When the CLI handles the request.
    let output = run(&arguments);

    // Then the rendered text remains on stdout with a successful status.
    assert_eq!(Some(0), output.status.code());
    assert!(!output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn version_renders_to_stdout() {
    // Given a request for the CLI version.
    let arguments = ["--version"];

    // When the CLI handles the request.
    let output = run(&arguments);

    // Then the rendered text remains on stdout with a successful status.
    assert_eq!(Some(0), output.status.code());
    assert!(!output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_subcommand_renders_clap_diagnostic_to_stderr() {
    // Given an invocation without a required subcommand.
    let arguments = [];

    // When the CLI parses the invocation.
    let output = run(&arguments);

    // Then Clap preserves its stderr destination and usage-error status.
    assert_eq!(Some(2), output.status.code());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn unknown_option_renders_clap_diagnostic_to_stderr() {
    // Given an invocation with an unknown option.
    let arguments = ["--definitely-invalid"];

    // When the CLI parses the invocation.
    let output = run(&arguments);

    // Then Clap preserves its stderr destination and usage-error status.
    assert_eq!(Some(2), output.status.code());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
}

#[test]
fn doctor_fix_help_matches_reserved_behavior() {
    let help = run(&["doctor", "--help"]);
    assert_eq!(Some(0), help.status.code());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("returns `NOT_IMPLEMENTED`"));
    assert!(!help.contains("executes the fix plan"));

    let fix = run(&["--no-color", "doctor", "--fix"]);
    assert_eq!(Some(64), fix.status.code());
    assert_eq!(
        "error: command 'doctor' is not implemented\n\
         hint: doctor is read-only in this release; rerun without --fix to inspect fix_plan suggestions\n",
        String::from_utf8_lossy(&fix.stderr)
    );
}

#[test]
fn handler_errors_use_human_labels_without_exposing_machine_codes() {
    let human = run(&["--no-color", "update"]);
    let stderr = String::from_utf8_lossy(&human.stderr);

    assert_eq!(Some(2), human.status.code());
    assert!(human.stdout.is_empty());
    assert!(
        stderr.starts_with("error: specify a component to update"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("INVALID_ARGUMENT"), "stderr: {stderr}");

    let json = run(&["--json", "update"]);
    let envelope: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("error output must remain valid JSON");

    assert_eq!(Some(2), json.status.code());
    assert!(json.stderr.is_empty());
    assert_eq!(Some("INVALID_ARGUMENT"), envelope["error"]["code"].as_str());
}

#[test]
fn sandbox_remove_local_and_global_dry_run_fail_closed_in_human_output() {
    let help = run(&["osbase", "sandbox", "remove", "--help"]);
    assert_eq!(Some(0), help.status.code());
    assert!(
        String::from_utf8_lossy(&help.stdout)
            .contains("Request a removal preview; currently fails closed without executing")
    );

    let invocations: &[&[&str]] = &[
        &[
            "--no-color",
            "osbase",
            "sandbox",
            "remove",
            "gvisor",
            "--dry-run",
        ],
        &[
            "--no-color",
            "--dry-run",
            "osbase",
            "sandbox",
            "remove",
            "gvisor",
        ],
    ];

    for arguments in invocations {
        let output = run(arguments);

        assert_eq!(Some(2), output.status.code());
        assert!(output.stdout.is_empty());
        assert_eq!(
            "error: command 'osbase sandbox remove' does not support --dry-run; no action was taken\n",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn sandbox_remove_local_and_global_dry_run_share_the_json_error() {
    let invocations: &[&[&str]] = &[
        &[
            "--json",
            "osbase",
            "sandbox",
            "remove",
            "gvisor",
            "--dry-run",
        ],
        &[
            "--json",
            "--dry-run",
            "osbase",
            "sandbox",
            "remove",
            "gvisor",
        ],
    ];

    for arguments in invocations {
        let output = run(arguments);
        let envelope: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("error output must remain valid JSON");

        assert_eq!(Some(2), output.status.code());
        assert!(output.stderr.is_empty());
        assert_eq!(Some(false), envelope["ok"].as_bool());
        assert_eq!(Some(1), envelope["schema_version"].as_u64());
        assert_eq!(Some("osbase sandbox remove"), envelope["command"].as_str());
        assert_eq!(Some("INVALID_ARGUMENT"), envelope["error"]["code"].as_str());
        assert_eq!(
            Some("command 'osbase sandbox remove' does not support --dry-run; no action was taken"),
            envelope["error"]["reason"].as_str()
        );
    }
}

#[test]
fn unsupported_osbase_install_dry_runs_share_human_and_json_errors() {
    for help_args in [
        &["osbase", "kernel", "install", "--help"][..],
        &["osbase", "security", "install", "--help"][..],
    ] {
        let help = run(help_args);
        assert_eq!(Some(0), help.status.code());
        assert!(
            String::from_utf8_lossy(&help.stdout)
                .contains("Request an install preview; currently fails closed without executing")
        );
    }

    let cases: &[(&str, &[&str])] = &[
        (
            "osbase kernel install",
            &["osbase", "kernel", "install", "--dry-run"],
        ),
        (
            "osbase kernel install",
            &["--dry-run", "osbase", "kernel", "install"],
        ),
        (
            "osbase security install",
            &["osbase", "security", "install", "loongshield", "--dry-run"],
        ),
        (
            "osbase security install",
            &["--dry-run", "osbase", "security", "install", "loongshield"],
        ),
    ];

    for (command, arguments) in cases {
        let reason = format!("command '{command}' does not support --dry-run; no action was taken");
        for json in [false, true] {
            let mut invocation = vec![if json { "--json" } else { "--no-color" }];
            invocation.extend_from_slice(arguments);
            let output = run(&invocation);

            assert_eq!(Some(2), output.status.code());
            if json {
                let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
                    .expect("error output must remain valid JSON");
                assert!(output.stderr.is_empty());
                assert_eq!(Some(false), envelope["ok"].as_bool());
                assert_eq!(Some(1), envelope["schema_version"].as_u64());
                assert_eq!(Some(*command), envelope["command"].as_str());
                assert_eq!(Some("INVALID_ARGUMENT"), envelope["error"]["code"].as_str());
                assert_eq!(Some(reason.as_str()), envelope["error"]["reason"].as_str());
            } else {
                assert!(output.stdout.is_empty());
                assert_eq!(
                    format!("error: {reason}\n"),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

#[test]
fn unsupported_osbase_installs_without_dry_run_remain_not_implemented() {
    let invocations: &[(&str, &[&str])] = &[
        (
            "osbase kernel install",
            &["--json", "osbase", "kernel", "install"],
        ),
        (
            "osbase security install loongshield",
            &["--json", "osbase", "security", "install", "loongshield"],
        ),
    ];

    for (command, arguments) in invocations {
        let output = run(arguments);
        let envelope: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("error output must remain valid JSON");

        assert_eq!(Some(64), output.status.code());
        assert!(output.stderr.is_empty());
        assert_eq!(Some(false), envelope["ok"].as_bool());
        assert_eq!(Some(*command), envelope["command"].as_str());
        assert_eq!(Some("NOT_IMPLEMENTED"), envelope["error"]["code"].as_str());
        assert_eq!(
            Some(format!("command '{command}' is not implemented").as_str()),
            envelope["error"]["reason"].as_str()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn help_reports_stdout_failure_when_output_device_is_full() {
    // Given stdout is connected to a device that rejects every write.
    let full = OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("Linux must provide /dev/full");

    // When Clap renders top-level help.
    let output = common::run_with_stdout(&["--help"], Stdio::from(full));

    // Then the CLI reports the failed stdout write instead of exiting successfully.
    assert_eq!(Some(1), output.status.code());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error: failed writing to stdout:"));
}
