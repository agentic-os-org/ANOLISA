//! Login-shell CLI contracts. Mutating platform paths are covered by private fixtures.

use std::process::Command;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cosh-cli"))
}

#[test]
fn login_shell_help_exposes_separate_operations_and_explicit_account_guards() {
    for action in ["status", "register", "unregister", "set", "restore"] {
        let output = command()
            .args(["login-shell", action, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--shell"));
        if matches!(action, "set" | "restore") {
            assert!(
                help.contains("--user")
                    && help.contains("--expect-shell")
                    && help.contains("--dry-run")
            );
        }
        if action == "restore" {
            assert!(help.contains("--to"));
        }
    }
    let missing = command()
        .args(["login-shell", "set", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn login_shell_rejects_relative_entries_without_touching_system_configuration() {
    let output = command()
        .args(["login-shell", "--shell", "relative/cosh", "register"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "InvalidInput");
    assert_eq!(response["meta"]["subsystem"], "login-shell");
}

#[test]
#[cfg(target_os = "linux")]
fn login_shell_status_and_dry_run_leave_shell_table_unchanged() {
    let before = std::fs::read("/etc/shells").ok();
    let output = command()
        .args([
            "login-shell",
            "status",
            "--shell",
            "/nonexistent-cosh-fixture-entry",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["data"]["executable"], false);
    assert_eq!(response["data"]["registered"], false);
    assert_eq!(response["data"]["changed"], false);
    let preview = command()
        .args([
            "login-shell",
            "unregister",
            "--shell",
            "/nonexistent-cosh-fixture-entry",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(preview.status.success());
    let response: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(response["meta"]["dry_run"], true);
    assert_eq!(std::fs::read("/etc/shells").ok(), before);
}
