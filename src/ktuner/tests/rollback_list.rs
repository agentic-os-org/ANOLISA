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
    // array of string triples (empty on this container, which has no ledger).
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
    }
}
