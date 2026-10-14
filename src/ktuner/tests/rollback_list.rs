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
        // a bool or null; the recorded category is one of the two documented
        // strings or an explicit null; the clearer of a zeroed twin is a
        // parameter name or an explicit null. All four keys are always present
        // on every entry.
        assert!(entry["live"].is_string() || entry["live"].is_null());
        assert!(entry["drifted"].is_boolean() || entry["drifted"].is_null());
        assert!(
            entry["category"] == "performance"
                || entry["category"] == "security"
                || entry["category"].is_null()
        );
        assert!(entry["cleared_by"].is_string() || entry["cleared_by"].is_null());
        assert_eq!(entry.as_object().unwrap().len(), 7, "entry keys: {entry}");
    }
}

/// End-to-end contract on a private fixture: run the real binary in a mount
/// namespace with a hand-built ledger, and assert that `--list` publishes
/// exactly the seven documented keys per entry, that an unreadable path is
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
            [
                "applied",
                "category",
                "cleared_by",
                "drifted",
                "live",
                "param",
                "previous"
            ],
            "entry keys, in the documented alphabetical order: {entry}"
        );
        // This fixture is a ledger written before the category was recorded:
        // every entry publishes null, never a guess.
        assert!(entry["category"].is_null(), "{entry}");
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

/// End-to-end contract for the recorded category: a ledger written after the
/// field existed publishes each entry's recorded category with the spelling
/// `check` uses for the same field, the category listing selects by it, and a
/// record that carries none publishes null rather than a guess.
#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_list_reports_the_recorded_category() {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!(
        "ktuner-list-category-{}-{nonce}",
        std::process::id()
    ));
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(scratch);
    fs::create_dir_all(scratch.0.join("varlib/ktuner")).expect("ledger directory");
    fs::create_dir(scratch.0.join("etc")).expect("isolated persistence directory");

    let somaxconn = scratch.0.join("somaxconn");
    let bpf_jit_harden = scratch.0.join("bpf_jit_harden");
    let swappiness = scratch.0.join("swappiness");
    fs::write(&somaxconn, "4096").expect("live value");
    fs::write(&bpf_jit_harden, "1").expect("live value");
    fs::write(&swappiness, "10").expect("live value");
    fs::write(
        scratch.0.join("varlib/ktuner/rollback.json"),
        serde_json::json!({"version": 1, "entries": {
            "net.core.somaxconn": {"previous": "128", "applied": "4096", "path": somaxconn, "category": "performance"},
            "net.core.bpf_jit_harden": {"previous": "0", "applied": "1", "path": bpf_jit_harden, "category": "security"},
            "vm.swappiness": {"previous": "60", "applied": "10", "path": swappiness},
        }})
        .to_string(),
    )
    .expect("ledger");

    let list = |extra: &[&str]| {
        Command::new("unshare")
            .args([
                "--mount",
                "--propagation",
                "private",
                "sh",
                "-c",
                "mount --bind \"$1\" /var/lib && mount --bind \"$2\" /etc && \
                 bin=\"$3\" && shift 3 && exec \"$bin\" rollback --list \"$@\"",
                "rollback-list-category-test",
            ])
            .arg(scratch.0.join("varlib"))
            .arg(scratch.0.join("etc"))
            .arg(env!("CARGO_BIN_EXE_ktuner"))
            .args(extra)
            .output()
            .expect("run isolated rollback --list")
    };

    // The plain listing: the recorded categories come back verbatim — the
    // security finding on the `net.*` knob keeps its category — and the
    // pre-category record publishes null.
    let out = list(&[]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(body["count"], 3, "{body}");
    assert_eq!(body["pending"][0]["param"], "net.core.bpf_jit_harden");
    assert_eq!(body["pending"][0]["category"], "security");
    assert_eq!(body["pending"][1]["param"], "net.core.somaxconn");
    assert_eq!(body["pending"][1]["category"], "performance");
    assert_eq!(body["pending"][2]["param"], "vm.swappiness");
    assert!(body["pending"][2]["category"].is_null(), "{body}");

    // The category listing selects by the recorded value, and every entry it
    // publishes carries the category it was selected for: `net` selects the
    // recorded performance entry on the `net.*` knob, not the security one.
    let out = list(&["--category", "net"]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(body["count"], 1, "{body}");
    let pending = body["pending"].as_array().expect("pending is an array");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["param"], "net.core.somaxconn");
    assert_eq!(pending[0]["category"], "performance");
}

/// End-to-end contract for `--list <param>...` on the same kind of private
/// fixture: the named entries — any accepted spelling, plus the mutually
/// exclusive twin the restore takes along — come back in the unfiltered
/// listing's shape and order, so a zeroed half's `cleared_by` still names an
/// entry the filtered set carries, and a name the ledger does not record is
/// the restore's command error (exit 2, stderr JSON, nothing on stdout) with
/// the ledger left byte for byte as it was.
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
    // The pair in this fixture is one write: `vm.dirty_bytes` was written, so
    // the kernel zeroed `vm.dirty_ratio` and the ledger records both. The row
    // whose 0 the write caused names the entry that caused it — in the
    // spelling the other row publishes as `param` — and the written half,
    // which was asked for, names nobody.
    assert_eq!(body["pending"][0]["param"], "vm.dirty_bytes");
    assert!(
        body["pending"][0]["cleared_by"].is_null(),
        "the written half was asked for: {body}"
    );
    assert_eq!(body["pending"][1]["param"], "vm.dirty_ratio");
    assert_eq!(
        body["pending"][1]["cleared_by"], "vm.dirty_bytes",
        "the zeroed twin names its clearer: {body}"
    );

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

/// End-to-end contract for `--list --drifted`: on the same kind of private
/// fixture, the listing keeps only the entries whose live value is known to
/// differ from the recorded one. A matching entry and an entry whose path
/// cannot be read (`drifted: null` — an unreadable value is not evidence of
/// drift) are both left out, the shape and order are the unfiltered ones, and
/// `count` is the filtered count; the plain listing still shows all three.
#[test]
#[ignore = "requires root and mount namespaces; writes only isolated fixture files"]
fn rollback_list_drifted_keeps_only_known_drift() {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!(
        "ktuner-list-drifted-{}-{nonce}",
        std::process::id()
    ));
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(scratch);
    fs::create_dir_all(scratch.0.join("varlib/ktuner")).expect("ledger directory");
    fs::create_dir(scratch.0.join("etc")).expect("isolated persistence directory");

    let ratio = scratch.0.join("dirty_ratio");
    let swappiness = scratch.0.join("swappiness");
    let gone = scratch.0.join("device-gone");
    fs::write(&ratio, "20").expect("live value");
    fs::write(&swappiness, "1").expect("live value");
    let ledger = scratch.0.join("varlib/ktuner/rollback.json");
    fs::write(
        &ledger,
        serde_json::json!({"version": 1, "entries": {
            "vm.dirty_ratio": {"previous": "20", "applied": "0", "path": ratio},
            "vm.swappiness": {"previous": "60", "applied": "1", "path": swappiness},
            "block/sda/scheduler": {"previous": "mq-deadline", "applied": "none", "path": gone},
        }})
        .to_string(),
    )
    .expect("ledger");

    let list = |extra: &[&str]| {
        Command::new("unshare")
            .args([
                "--mount",
                "--propagation",
                "private",
                "sh",
                "-c",
                "mount --bind \"$1\" /var/lib && mount --bind \"$2\" /etc && \
                 bin=\"$3\" && shift 3 && exec \"$bin\" rollback --list \"$@\"",
                "rollback-list-drifted-test",
            ])
            .arg(scratch.0.join("varlib"))
            .arg(scratch.0.join("etc"))
            .arg(env!("CARGO_BIN_EXE_ktuner"))
            .args(extra)
            .output()
            .expect("run isolated rollback --list")
    };

    // The filtered listing: only vm.dirty_ratio drifted (applied 0, live 20);
    // vm.swappiness still matches and the missing path cannot be read.
    let out = list(&["--drifted"]);
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(body["count"], 1);
    let pending = body["pending"].as_array().expect("pending is an array");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["param"], "vm.dirty_ratio");
    assert_eq!(pending[0]["drifted"], true);
    assert_eq!(pending[0]["live"], "20");
    assert_eq!(pending[0]["applied"], "0");
    assert_eq!(pending[0]["previous"], "20");

    // The plain listing is unchanged: all three entries, same shape.
    let out = list(&[]);
    assert!(out.status.success());
    let body: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(body["count"], 3);
    let pending = body["pending"].as_array().expect("pending is an array");
    let params: Vec<&str> = pending
        .iter()
        .map(|entry| entry["param"].as_str().expect("param is a string"))
        .collect();
    assert_eq!(
        params,
        vec!["block/sda/scheduler", "vm.dirty_ratio", "vm.swappiness"]
    );
    assert_eq!(body["pending"][0]["drifted"], Value::Null);
    assert_eq!(body["pending"][1]["drifted"], true);
    assert_eq!(body["pending"][2]["drifted"], false);
    // No clearer is recorded for this zero: the write that would have caused
    // it is not in the ledger, so the row publishes null rather than guessing
    // at the pair table.
    assert_eq!(body["pending"][1]["cleared_by"], Value::Null);
    assert_eq!(body["pending"][0]["cleared_by"], Value::Null);
    assert_eq!(body["pending"][2]["cleared_by"], Value::Null);
}
