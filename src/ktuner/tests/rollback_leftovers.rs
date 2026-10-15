//! End-to-end contract for the entries a rollback leaves behind: the counters
//! say how many parameters are still tuned, and the body has to name them. A
//! caller that reads `failed: 1` cannot undo (or explain) what the answer does
//! not identify.
//!
//! Runs the real binary against a hand-built ledger in a mount namespace, so
//! the fixture paths, not the host's, are the ones restored: one entry is
//! written for real, one is addressed at a directory (the write is refused),
//! one is recorded at a path that no longer exists.

#![cfg(target_os = "linux")]

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_names_the_entries_it_left() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "ktuner-rollback-left-{}-{nonce}",
        std::process::id()
    )));
    fs::create_dir_all(scratch.0.join("varlib/ktuner")).expect("ledger directory");
    fs::create_dir(scratch.0.join("etc")).expect("isolated persistence directory");
    let restored = scratch.0.join("swappiness");
    let blocked = scratch.0.join("blocked");
    let absent = scratch.0.join("absent");
    fs::write(&restored, "10").expect("writable target");
    fs::create_dir(&blocked).expect("a directory where the recorded file belongs");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    fs::write(
        &ledger,
        serde_json::json!({"version": 1, "entries": {
            "vm.swappiness": {"previous": "60", "applied": "10", "path": restored},
            "vm.dirty_ratio": {"previous": "20", "applied": "5", "path": blocked},
            "net.core.somaxconn": {"previous": "4096", "applied": "128", "path": absent},
        }})
        .to_string(),
    )
    .expect("ledger");

    let out = Command::new("unshare")
        .args([
            "--mount",
            "--propagation",
            "private",
            "sh",
            "-c",
            "mount --bind \"$1\" /var/lib && mount --bind \"$2\" /etc && exec \"$3\" rollback",
            "rollback-leftovers-test",
        ])
        .arg(scratch.0.join("varlib"))
        .arg(scratch.0.join("etc"))
        .arg(env!("CARGO_BIN_EXE_ktuner"))
        .output()
        .expect("run isolated rollback");

    assert_eq!(
        out.status.code(),
        Some(1),
        "an incomplete restoration exits 1: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(body["restored"], 1, "{body}");
    assert_eq!(body["failed"], 1, "{body}");
    assert_eq!(body["skipped"], 1, "{body}");
    assert_eq!(body["status"], "Partial", "{body}");
    // The count is reconcilable: the failure is named, with the reason the
    // write did not land.
    let failures = body["failures"].as_array().expect("failures is an array");
    assert_eq!(failures.len(), 1, "one entry could not be written: {body}");
    assert_eq!(failures[0]["param"], "vm.dirty_ratio", "{body}");
    assert!(
        failures[0]["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "the failure carries why the write did not land: {body}"
    );
    // The skipped entry is named as well, in the spelling the ledger records
    // and `rollback --list` publishes.
    assert_eq!(body["missing"], serde_json::json!(["net.core.somaxconn"]));

    // The entries that did restore are gone from the live system, and the
    // ledger keeps all three records for a retry.
    assert_eq!(fs::read_to_string(&restored).expect("restored file"), "60");
    let recorded: Value =
        serde_json::from_str(&fs::read_to_string(&ledger).expect("ledger survives")).expect("JSON");
    assert_eq!(
        recorded["entries"].as_object().map(|entries| entries.len()),
        Some(3),
        "a retry still needs every original: {recorded}"
    );
}
