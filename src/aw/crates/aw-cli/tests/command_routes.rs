//! Public routing aliases preserve legacy failures without executing an Agent.
#![cfg(target_os = "linux")]

use std::{
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
fn native_connect_and_legacy_install_preserve_the_same_validation_failure() {
    let directory = tempfile::tempdir().unwrap();
    let old = invoke(
        &["install", "--config", "missing.yaml", "--agent", "hermes"],
        directory.path(),
    );
    let new = invoke(
        &[
            "agent",
            "connect",
            "--config",
            "missing.yaml",
            "--agent",
            "hermes",
        ],
        directory.path(),
    );
    assert_eq!(old.status.code(), Some(1));
    assert_eq!(old.status.code(), new.status.code());
    assert_eq!(old.stderr, new.stderr);
    assert!(old.stdout.is_empty() && new.stdout.is_empty());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn help_separates_software_installation_from_native_agent_connection() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(&["--help"], directory.path());
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "aw install --prefix",
        "aw agent connect --config",
        "aw config init --prefix",
        "aw config validate --config",
        "local unpacked Preview bundles only",
        "Compatibility: aw install --config",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert!(!text.contains("provider install"));
    assert!(!text.contains("aw-package"));
}

#[test]
fn service_aliases_keep_legacy_errors_without_side_effects() {
    let directory = tempfile::tempdir().unwrap();
    for command in ["serve", "status", "stop"] {
        let old = invoke(&[command, "--config", "missing.yaml"], directory.path());
        let new = invoke(
            &["service", command, "--config", "missing.yaml"],
            directory.path(),
        );
        assert_eq!(old.status.code(), Some(1));
        assert_eq!(new.status.code(), old.status.code());
        assert_eq!(new.stderr, old.stderr);
        assert!(new.stdout.is_empty() && old.stdout.is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
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
