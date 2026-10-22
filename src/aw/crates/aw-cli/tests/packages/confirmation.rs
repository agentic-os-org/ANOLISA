//! Removal consent belongs to one command, not package ownership or authorization.

use super::{output::invoke, *};
use std::{io::Write, process::Stdio};

#[test]
fn noninteractive_and_json_removal_require_explicit_consent_without_writes() {
    let f = Fixture::new();
    for json in [false, true] {
        let mut command = f.command();
        command.args(["uninstall", "--prefix"]).arg(&f.prefix);
        if json {
            command.arg("--json");
        }
        let output = invoke(&command);
        assert_eq!(output.status.code(), Some(1));
        if json {
            assert!(output.stderr.is_empty());
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["status"], "failed");
            assert_eq!(report["error"]["code"], "confirmation_required");
            assert!(report.get("mutation").is_none());
        } else {
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("requires --yes"));
        }
        assert!(!f.prefix.exists());
    }
    let mut child = f
        .command()
        .args(["uninstall", "--prefix"])
        .arg(&f.prefix)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(b"yes\n");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!f.prefix.exists());
}

#[test]
fn explicit_consent_preserves_integrity_checks() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    fs::write(f.prefix.join("bin/aw"), b"modified").unwrap();
    let output = invoke(
        f.command()
            .args(["uninstall", "-y", "--json", "--prefix"])
            .arg(&f.prefix),
    );
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["error"]["code"], "plan_blocked");
    assert!(!report["result"]["plan"]["problems"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(f.prefix.join(".aw-packages/aw-core.json").is_file());
    let f = Fixture::new();
    success(f.install().output().unwrap());
    let output = f
        .command()
        .args(["uninstall", "--yes", "--prefix"])
        .arg(&f.prefix)
        .current_dir(&f.root)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!f.prefix.join("bin/aw").exists());
}
