//! Public command routes reject removed spellings without executing an Agent.
#![cfg(target_os = "linux")]

use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn invoke(arguments: &[&str], cwd: &Path) -> aw_exec::Output {
    aw_exec::run(
        &aw_exec::CommandSpec {
            program: env!("CARGO_BIN_EXE_aw").into(),
            args: arguments.iter().map(Into::into).collect(),
            cwd: cwd.into(),
            environment: std::env::vars_os().collect(),
        },
        b"",
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        },
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
    )
    .unwrap()
}

#[test]
fn removed_spellings_fail_before_native_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["install", "--config", "missing.yaml", "--agent", "hermes"],
        vec![
            "agent",
            "connect",
            "--config",
            "missing.yaml",
            "--agent",
            "hermes",
        ],
        vec!["instance", "update", "--instance", "one"],
        vec!["instance", "attach", "--sdk-instance", "one"],
        vec!["instance", "attach", "--agent", "qoder", "--pid", "1"],
        vec!["instance", "detach", "--sdk-instance", "one"],
        vec!["connect", "--agent", "qoder", "--pid", "1", "--remove"],
        vec!["serve", "--config", "missing.yaml"],
        vec!["status", "--config", "missing.yaml"],
        vec!["stop", "--config", "missing.yaml"],
        vec!["validate", "--config", "missing.yaml"],
    ] {
        let output = invoke(&arguments, directory.path());
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("unexpected") || error.contains("unrecognized"),
            "{error}"
        );
    }
    let connected = invoke(
        &["connect", "--config", "missing.yaml", "--agent", "hermes"],
        directory.path(),
    );
    assert_eq!(connected.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&connected.stderr).contains("Usage:"));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn update_defaults_to_the_same_shared_configuration_without_a_socket() {
    let directory = tempfile::tempdir().unwrap();
    for selector in ["--agent", "--instance"] {
        let implicit = invoke(&["update", selector, "one"], directory.path());
        let explicit = invoke(
            &["update", "--config", "aw.yaml", selector, "one"],
            directory.path(),
        );
        assert_eq!(implicit.status.code(), Some(1));
        assert_eq!(implicit.stderr, explicit.stderr);
        assert!(!String::from_utf8_lossy(&implicit.stderr).contains("Usage:"));
        assert!(implicit.stdout.is_empty() && explicit.stdout.is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn help_separates_software_installation_from_native_agent_connection() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(&["--help"], directory.path());
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "install",
        "connect",
        "disconnect",
        "config",
        "package.json",
        "Use aw connect",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert!(!text.contains("manifest.json"));
    for (path, options) in [
        (vec!["install", "--help"], vec!["--prefix", "--bundle"]),
        (
            vec!["connect", "--help"],
            vec!["--config", "--agent", "--sdk-instance", "--sdk-pid"],
        ),
        (
            vec!["disconnect", "--help"],
            vec!["--config", "--agent", "--pid", "--sdk-instance"],
        ),
        (
            vec!["config", "init", "--help"],
            vec!["--prefix", "--state-dir"],
        ),
        (
            vec!["config", "validate", "--help"],
            vec!["--config", "--against"],
        ),
    ] {
        let help = invoke(&path, directory.path());
        assert!(help.status.success());
        let help = String::from_utf8(help.stdout).unwrap();
        for option in options {
            assert!(help.contains(option), "missing {option} in {path:?}");
        }
    }
    assert!(!text.contains("provider install"));
    assert!(!text.contains("aw-package"));
}

#[test]
fn service_commands_reject_missing_configuration_without_side_effects() {
    let directory = tempfile::tempdir().unwrap();
    for command in ["serve", "status", "stop"] {
        let mut arguments = vec!["service", command, "--config", "missing.yaml"];
        if command == "serve" {
            arguments.extend(["--state-dir", "/uncreated/state"]);
        }
        let output = invoke(&arguments, directory.path());
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Usage:"));
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn every_public_level_has_help_without_accessing_files_or_services() {
    let directory = tempfile::tempdir().unwrap();
    for command in [
        "",
        "run",
        "commands",
        "completion",
        "install",
        "uninstall",
        "connect",
        "disconnect",
        "config",
        "config init",
        "config show",
        "config validate",
        "config add-provider",
        "config remove-provider",
        "config add-event",
        "config add-hook",
        "config remove-hook",
        "service",
        "service serve",
        "service status",
        "service stop",
        "update",
        "inspect",
        "request",
        "policy",
    ] {
        for flag in ["--help", "-h"] {
            let mut arguments: Vec<_> = command.split_whitespace().collect();
            arguments.push(flag);
            let output = invoke(&arguments, directory.path());
            assert!(
                output.status.success(),
                "{arguments:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        }
    }
    let root = invoke(&[], directory.path());
    assert!(root.status.success());
    let text = String::from_utf8_lossy(&root.stdout);
    assert!(!text.contains("  hook ") && !text.contains("  bridge "));
    assert!(!text.contains("  provider ") && !text.contains("  migrate "));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn malformed_queries_and_unsupported_output_flags_never_succeed() {
    let directory = tempfile::tempdir().unwrap();
    for command in [
        "--help extra",
        "--help --help",
        "--version extra",
        "--version --version",
        "--unknown --help",
        "agent --help extra",
        "config validate --help --unknown",
        "run --config x --config y --agent qoder",
        "run --unknown",
        "run --config x --config y --help",
        "run --reloadable --reloadable --config x --agent qoder",
        "service start",
        "service status --socket /missing --json",
        "config show --config /missing --json",
        "--json service status",
        "run --version",
        "inspect --config /missing",
        "install --bundle /missing",
        "request --socket /missing --timeout-ms 0",
    ] {
        let arguments: Vec<_> = command.split_whitespace().collect();
        let output = invoke(&arguments, directory.path());
        assert_eq!(output.status.code(), Some(1), "{command}");
        assert!(output.stdout.is_empty(), "{command}");
        assert!(!output.stderr.is_empty(), "{command}");
    }
    let version = invoke(&["-V"], directory.path());
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!("aw {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn missing_uninstall_dry_run_reports_only_stderr_without_writes() {
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let prefix = directory.path().join("missing");
    let output = invoke(
        &[
            "uninstall",
            "--prefix",
            prefix.to_str().unwrap(),
            "--dry-run",
        ],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("not_installed"));
    assert!(!prefix.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn internal_callback_errors_keep_native_blocking_and_lifecycle_exit_contracts() {
    let directory = tempfile::tempdir().unwrap();
    let hook = invoke(
        &[
            "hook",
            "--adapter",
            "qoder",
            "--event",
            "tool.before",
            "--on-error",
            "block",
            "--unknown",
            "value",
        ],
        directory.path(),
    );
    assert_eq!(hook.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&hook.stderr).contains("aw hook failed"));
    assert!(!String::from_utf8_lossy(&hook.stderr).contains("Usage:"));
    let bridge = invoke(
        &["bridge", "--adapter", "qoder", "--unknown", "value"],
        directory.path(),
    );
    assert_eq!(bridge.status.code(), Some(2));
    assert!(bridge.stdout.is_empty());
    assert!(String::from_utf8_lossy(&bridge.stderr).contains("AW lifecycle failed"));
}

#[test]
fn version_and_service_help_are_explicit_and_readonly() {
    let directory = tempfile::tempdir().unwrap();
    let version = invoke(&["--version"], directory.path());
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!("aw {}\n", env!("CARGO_PKG_VERSION"))
    );
    let help = invoke(&["service", "--help"], directory.path());
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("foreground"));
    assert!(help.contains("No system service is installed"));
    assert!(!invoke(&["service", "start"], directory.path())
        .status
        .success());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
