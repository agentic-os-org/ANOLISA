// Integration contract for `ktuner rollback --list`: read-only, structured,
// and self-skipping outside the container CI environment. Deliberately does
// NOT run plain `ktuner rollback` — on a host with a live ledger that would
// destroy real tuning state.
use serde_json::Value;
use std::process::Command;

fn ktuner() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
}

#[test]
fn rollback_list_cli_contract() {
    let is_root = unsafe { libc::geteuid() } == 0;
    if !is_root {
        // Non-root: the command must refuse (exit 2) before touching the
        // ledger, with the standard error JSON.
        let out = ktuner().args(["rollback", "--list"]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "non-root must be refused");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("requires root"),
            "stderr should name the root requirement, got: {stderr}"
        );
        return;
    }
    // Root: exit 0 with structured JSON — count is a number, pending is an
    // array of entries carrying the recorded triple plus the live reading
    // (empty on this container, which has no ledger).
    let out = ktuner().args(["rollback", "--list"]).output().unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    let count = body["count"].as_u64().expect("count is a number");
    let pending = body["pending"].as_array().expect("pending is an array");
    assert_eq!(count as usize, pending.len());
    for entry in pending {
        assert!(entry["param"].is_string());
        assert!(entry["applied"].is_string());
        assert!(entry["previous"].is_string());
        // The live reading is a string or an explicit null; the comparison is
        // a bool or null. Both keys are always present on every entry.
        assert!(entry["live"].is_string() || entry["live"].is_null());
        assert!(entry["drifted"].is_boolean() || entry["drifted"].is_null());
        assert_eq!(entry.as_object().unwrap().len(), 5, "entry keys: {entry}");
    }
}

/// End-to-end contract on a private fixture: run the real binary in a mount
/// namespace with a hand-built ledger, and assert that `--list` publishes
/// exactly the five documented keys per entry, that an unreadable path is
/// reported as null inside the JSON (never printed, never an error, stderr
/// empty, exit code untouched), and that a changed value is flagged.
///
/// Requires root and mount namespaces (the gate `rollback_exit.rs` uses), so
/// it is ignored by default; it touches only isolated fixture files.
#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_list_reports_live_drift_without_extra_output() {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!("ktuner-list-{}-{nonce}", std::process::id()));
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(scratch);
    fs::create_dir_all(scratch.0.join("varlib/ktuner")).expect("ledger directory");
    fs::create_dir(scratch.0.join("etc")).expect("isolated persistence directory");

    let still_live = scratch.0.join("still-live");
    let moved = scratch.0.join("moved");
    let gone = scratch.0.join("gone");
    fs::write(&still_live, "1").expect("live value");
    fs::write(&moved, "20").expect("drifted value");
    fs::write(
        scratch.0.join("varlib/ktuner/rollback.json"),
        serde_json::json!({"version": 1, "entries": {
            "vm.swappiness": {"previous": "60", "applied": "1", "path": still_live},
            "vm.dirty_ratio": {"previous": "20", "applied": "0", "path": moved},
            "block/sda/scheduler": {"previous": "mq-deadline", "applied": "none", "path": gone},
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
            "mount --bind \"$1\" /var/lib && mount --bind \"$2\" /etc && exec \"$3\" rollback --list",
            "rollback-list-test",
        ])
        .arg(scratch.0.join("varlib"))
        .arg(scratch.0.join("etc"))
        .arg(env!("CARGO_BIN_EXE_ktuner"))
        .output()
        .expect("run isolated rollback --list");

    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.stderr.is_empty(),
        "an unreadable live value is reported in the JSON, never on stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body.as_object().unwrap().len(),
        2,
        "no top-level key beyond count and pending: {body}"
    );
    let pending = body["pending"].as_array().expect("pending is an array");
    assert_eq!(pending.len(), 3, "the entry set is unchanged: {body}");
    for entry in pending {
        let keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["applied", "drifted", "live", "param", "previous"],
            "entry keys, in the documented alphabetical order: {entry}"
        );
    }
    assert_eq!(pending[0]["param"], "block/sda/scheduler");
    assert!(pending[0]["live"].is_null(), "{body}");
    assert!(pending[0]["drifted"].is_null(), "{body}");
    assert_eq!(pending[1]["param"], "vm.dirty_ratio");
    assert_eq!(pending[1]["live"], "20");
    assert_eq!(pending[1]["drifted"], true);
    assert_eq!(pending[2]["param"], "vm.swappiness");
    assert_eq!(pending[2]["live"], "1");
    assert_eq!(pending[2]["drifted"], false);
}

/// End-to-end contract for `--list <param>...` on the same kind of private
/// fixture: the named entries — any accepted spelling, plus the mutually
/// exclusive twin the restore takes along — come back in the unfiltered
/// listing's shape and order, and a name the ledger does not record is the
/// restore's command error (exit 2, stderr JSON, nothing on stdout) with the
/// ledger left byte for byte as it was.
#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_list_narrows_to_the_named_entries() {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let scratch =
        std::env::temp_dir().join(format!("ktuner-list-filter-{}-{nonce}", std::process::id()));
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(scratch);
    fs::create_dir_all(scratch.0.join("varlib/ktuner")).expect("ledger directory");
    fs::create_dir(scratch.0.join("etc")).expect("isolated persistence directory");

    let swappiness = scratch.0.join("swappiness");
    let bytes = scratch.0.join("dirty_bytes");
    let ratio = scratch.0.join("dirty_ratio");
    let somaxconn = scratch.0.join("somaxconn");
    fs::write(&swappiness, "1").expect("live value");
    fs::write(&bytes, "1073741824").expect("live value");
    fs::write(&ratio, "0").expect("live value");
    fs::write(&somaxconn, "4096").expect("live value");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    fs::write(
        &ledger,
        serde_json::json!({"version": 1, "entries": {
            "vm.swappiness": {"previous": "60", "applied": "1", "path": swappiness},
            "vm.dirty_bytes": {"previous": "0", "applied": "1073741824", "path": bytes},
            "vm.dirty_ratio": {"previous": "20", "applied": "0", "path": ratio},
            "net.core.somaxconn": {"previous": "128", "applied": "4096", "path": somaxconn},
        }})
        .to_string(),
    )
    .expect("ledger");
    let ledger_before = fs::read(&ledger).expect("ledger bytes");

    let list = |params: &[&str]| {
        Command::new("unshare")
            .args([
                "--mount",
                "--propagation",
                "private",
                "sh",
                "-c",
                "mount --bind \"$1\" /var/lib && mount --bind \"$2\" /etc && \
                 bin=\"$3\" && shift 3 && exec \"$bin\" rollback --list \"$@\"",
                "rollback-list-filter-test",
            ])
            .arg(scratch.0.join("varlib"))
            .arg(scratch.0.join("etc"))
            .arg(env!("CARGO_BIN_EXE_ktuner"))
            .args(params)
            .output()
            .expect("run isolated rollback --list")
    };

    let out = list(&["vm/swappiness", "vm.dirty_bytes"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        body.as_object().unwrap().len(),
        2,
        "the unfiltered listing's top-level keys, nothing added: {body}"
    );
    assert_eq!(body["count"], 3, "{body}");
    let params: Vec<&str> = body["pending"]
        .as_array()
        .expect("pending is an array")
        .iter()
        .map(|entry| entry["param"].as_str().expect("param"))
        .collect();
    assert_eq!(
        params,
        ["vm.dirty_bytes", "vm.dirty_ratio", "vm.swappiness"],
        "named entries plus the recorded twin, in param order: {body}"
    );
    assert_eq!(body["pending"][2]["live"], "1");
    assert_eq!(body["pending"][2]["drifted"], false);

    let out = list(&["vm.swappiness", "vm.typo"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let error: Value = serde_json::from_slice(&out.stderr).expect("stderr JSON");
    assert_eq!(
        error["error"], "parameter not recorded in the rollback ledger: vm.typo",
        "{error}"
    );
    assert_eq!(
        fs::read(&ledger).expect("ledger bytes"),
        ledger_before,
        "the preview never writes the ledger"
    );
}
