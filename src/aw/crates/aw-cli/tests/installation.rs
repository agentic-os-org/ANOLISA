//! Unified package CLI contracts without local Agent or backend installations.
use std::process::Command;
#[test]
fn help_exposes_native_installation_and_dual_agent_configuration() {
    for (arguments, expected) in [
        (vec!["install", "--help"], vec!["--prefix", "--bundle"]),
        (
            vec!["config", "init", "--help"],
            vec!["--prefix", "--qoder", "--openclaw"],
        ),
        (vec!["uninstall", "--help"], vec!["--prefix", "--yes"]),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for option in expected {
            assert!(text.contains(option), "missing {option}");
        }
        assert!(!text.contains("aw-package"));
    }
}
#[test]
fn invalid_commands_and_duplicate_flags_fail_before_touching_disk() {
    for args in [
        vec!["unknown"],
        vec!["install", "--prefix", "/unused", "--prefix", "/other"],
        vec!["config", "init", "--prefix"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr).unwrap().contains("aw:"));
    }
}

#[test]
fn non_utf8_arguments_are_rejected_without_panicking() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let invalid = OsStr::from_bytes(b"/tmp/prefix-\xff");
    let arguments = ["install".into(), "--prefix".into(), invalid.to_os_string()];
    let output = Command::new(env!("CARGO_BIN_EXE_aw"))
        .args(arguments)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("aw:"), "{stderr}");
    assert!(stderr.contains("UTF-8"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
