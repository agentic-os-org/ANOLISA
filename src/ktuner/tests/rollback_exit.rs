//! Actual rollback CLI outcomes using private filesystem fixtures.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn rollback(case: &Path) -> Output {
    // Isolate both the ledger and successful rollback's persistence cleanup.
    // The empty /etc fixture prevents entry into systemctl cleanup branches.
    Command::new("unshare")
        .args([
            "--mount",
            "--propagation",
            "private",
            "sh",
            "-c",
            "mount --bind \"$1\" /var/lib && mount --bind \"$2\" /etc && exec \"$3\" rollback",
            "rollback-test",
        ])
        .arg(case.join("varlib"))
        .arg(case.join("etc"))
        .arg(env!("CARGO_BIN_EXE_ktuner"))
        .output()
        .expect("run isolated rollback")
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_exit_reports_complete_partial_and_failed_restoration() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let scratch = Scratch(
        std::env::temp_dir().join(format!("ktuner-rollback-{}-{nonce}", std::process::id())),
    );
    fs::create_dir(&scratch.0).expect("scratch directory");

    for (name, restored, failed, skipped, code) in [
        ("complete", 1, 0, 0, 0),
        ("empty", 0, 0, 0, 0),
        ("failed", 0, 1, 0, 1),
        ("skipped", 0, 0, 1, 1),
        ("partial-failed", 1, 1, 0, 1),
        ("partial-skipped", 1, 0, 1, 1),
    ] {
        let case = scratch.0.join(name);
        fs::create_dir_all(case.join("varlib/ktuner")).expect("ledger directory");
        fs::create_dir(case.join("etc")).expect("isolated persistence directory");
        let target = case.join("restorable");
        let mut entries = serde_json::Map::new();
        for (param, path) in [
            (
                "vm.audit_restored",
                (restored > 0).then_some(target.clone()),
            ),
            (
                "vm.audit_failed",
                (failed > 0).then_some(case.join("directory")),
            ),
            (
                "vm.audit_skipped",
                (skipped > 0).then_some(case.join("absent")),
            ),
        ] {
            if let Some(path) = path {
                if param == "vm.audit_restored" {
                    fs::write(&path, "1").expect("current value");
                } else if param == "vm.audit_failed" {
                    fs::create_dir(&path).expect("unwritable directory target");
                }
                entries.insert(
                    param.to_owned(),
                    serde_json::json!({
                        "previous": "0", "applied": "1", "path": path,
                    }),
                );
            }
        }
        let ledger = case.join("varlib/ktuner/rollback.json");
        fs::write(
            &ledger,
            serde_json::json!({"version": 1, "entries": entries}).to_string(),
        )
        .expect("ledger");
        let out = rollback(&case);
        assert_eq!(
            out.status.code(),
            Some(code),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout JSON");
        assert_eq!(json["restored"], restored, "{name}");
        assert_eq!(json["failed"], failed, "{name}");
        assert_eq!(json["skipped"], skipped, "{name}");
        assert_eq!(
            ledger.exists(),
            code == 1,
            "{name}: preserve incomplete ledger"
        );
        if restored > 0 {
            assert_eq!(fs::read_to_string(target).expect("restored value"), "0");
        }
        println!("{name}: exit={code}, {json}");
    }

    for malformed in [false, true] {
        let case = scratch
            .0
            .join(if malformed { "malformed" } else { "missing" });
        fs::create_dir_all(case.join("varlib/ktuner")).expect("ledger directory");
        fs::create_dir(case.join("etc")).expect("isolated persistence directory");
        let ledger = case.join("varlib/ktuner/rollback.json");
        if malformed {
            fs::write(&ledger, "{invalid-json").expect("invalid ledger");
        }
        let out = rollback(&case);
        assert_eq!(out.status.code(), Some(2));
        let error: serde_json::Value = serde_json::from_slice(&out.stderr).expect("stderr JSON");
        assert!(error["error"].is_string());
        assert!(out.stdout.is_empty());
        assert_eq!(ledger.exists(), malformed);
        println!(
            "{}: exit=2, {error}",
            if malformed { "malformed" } else { "missing" }
        );
    }
}
