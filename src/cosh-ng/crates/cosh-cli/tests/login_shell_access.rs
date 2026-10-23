//! Exercise the disposable permission probe without executing a candidate shell.

#![cfg(target_os = "linux")]

use nix::unistd::{getgid, getuid, User};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn probe(shell: &Path) -> bool {
    let user = User::from_uid(getuid()).unwrap().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-cli"))
        .args(["__login-shell-access", "--user", &user.name])
        .args([
            "--uid",
            &getuid().to_string(),
            "--gid",
            &getgid().to_string(),
        ])
        .arg("--shell")
        .arg(shell)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], true);
    response["data"].as_bool().unwrap()
}

#[test]
fn access_probe_checks_execute_bits_and_directory_search_without_running_shell() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/login-shell-fixtures");
    fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(fs::canonicalize(base).unwrap()).unwrap();
    let parent = dir.path().join("private");
    fs::create_dir(&parent).unwrap();
    let shell = parent.join("cosh");
    // Deliberately not a program. A successful probe must not execute this file.
    fs::write(&shell, b"not an executable image\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o100)).unwrap();
    assert!(probe(&shell), "an executable binary need not be readable");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!probe(&shell));
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!probe(&parent), "directories cannot be login shells");
    if !getuid().is_root() {
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o600)).unwrap();
        let accessible = probe(&shell);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!accessible, "search permission is required on every parent");
        // An owner cannot borrow the other class's execute permission.
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o001)).unwrap();
        assert!(!probe(&shell));
    }
    fs::remove_file(&shell).unwrap();
    assert!(!probe(&shell));
}

#[test]
fn access_probe_is_hidden_from_normal_cli_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_cosh-cli"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8(output.stdout)
        .unwrap()
        .contains("__login-shell-access"));
}
