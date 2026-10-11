//! CLI agent-contract integration tests.
//!
//! `discover` / `token` are the machine-facing surface agents and scripts
//! depend on: structured `--json` output and non-zero exit on failure. These
//! tests run the real built binary and assert that contract end-to-end, which
//! is what exercises (and covers) the CLI paths — JSON serialization and
//! error -> `exit(1)` — that live in the bin crate and cannot be reached from
//! library unit tests.
//!
//! `discover`/`token` are pure userspace (no eBPF), so these run as a normal
//! user in CI.

use std::path::PathBuf;
use std::process::Command;

fn agentsight() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentsight"))
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("agentsight_clitest_{}_{name}", std::process::id()))
}

// ── discover ────────────────────────────────────────────────────────────────

#[test]
fn discover_list_known_json_is_a_non_empty_agent_array() {
    let out = agentsight()
        .args(["discover", "--list-known", "--json"])
        .output()
        .expect("run agentsight");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--list-known --json must emit valid JSON");
    let arr = v.as_array().expect("known agents must be a JSON array");
    assert!(!arr.is_empty(), "known-agent list must not be empty");
    // Discriminating: the embedded default rules include recognizable agents.
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(
        names
            .iter()
            .any(|n| n.contains("Codex") || n.contains("Cosh")),
        "expected a known agent name, got {names:?}"
    );
}

#[test]
fn discover_list_known_json_reports_matched_pids() {
    // `--list-known` documents itself as listing the known agents *and the
    // PIDs they currently match*, and the text mode prints them; the JSON mode
    // dropped them, so a script could not tell "nothing matched" from "the
    // field is never emitted".
    let dir = tmp("known_pids");
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let config = dir.join("config.json");
    std::fs::write(
        &config,
        r#"{"cmdline":{"allow":[{"rule":["*discover_probe_*"],"agent_name":"Probe"}]}}"#,
    )
    .expect("write config");
    // The probe's own file name carries the pattern, so this rule can only
    // match that process. The name is unique per run because a file another
    // process still holds open is ETXTBSY to exec, and `tmp` reuses its
    // directory across runs.
    let probe = format!(
        "discover_probe_{}_bin",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let exe = dir.join(&probe);
    std::fs::copy("/bin/sleep", &exe).expect("copy a probe binary");
    // ETXTBSY is the documented, transient result of exec'ing a file that was
    // just written; it is not a test failure.
    let mut child = {
        let mut attempt = 0;
        loop {
            match Command::new(&exe).arg("30").spawn() {
                Ok(child) => break child,
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 100 =>
                {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("spawn the probe process: {error:?}"),
            }
        }
    };

    // `spawn` returns before the child has been exec'd, and the scanner reads
    // `/proc/<pid>/cmdline`: wait until the probe is actually visible so the
    // assertion below cannot race the exec.
    let ready = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", child.id())).unwrap_or_default();
        if cmdline.windows(probe.len()).any(|w| w == probe.as_bytes()) {
            break;
        }
        assert!(
            std::time::Instant::now() < ready,
            "the probe process never became visible in /proc"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let out = agentsight()
        .args([
            "discover",
            "--list-known",
            "--json",
            "--config",
            config.to_str().expect("config path"),
        ])
        .output()
        .expect("run agentsight");
    let probe_pid = child.id();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--list-known --json must emit valid JSON");
    let arr = v.as_array().expect("known agents must be a JSON array");
    let probe = arr
        .iter()
        .find(|entry| entry.get("name").and_then(|n| n.as_str()) == Some("Probe"))
        .expect("the configured rule must be listed");
    let pids = probe
        .get("matched_pids")
        .and_then(|p| p.as_array())
        .expect("the JSON mode must report the PIDs the text mode prints");
    assert!(
        pids.iter()
            .any(|pid| pid.as_u64() == Some(u64::from(probe_pid))),
        "the running probe (pid {probe_pid}) must be reported, got {pids:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discover_list_known_text_prints_header() {
    let out = agentsight()
        .args(["discover", "--list-known"])
        .output()
        .expect("run agentsight");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("已知 AI Agent"));
}

#[test]
fn discover_scan_json_is_a_valid_array() {
    // Scans the live system (read-only); may be empty, but must be a JSON array.
    let out = agentsight()
        .args(["discover", "--json"])
        .output()
        .expect("run agentsight");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("scan --json must emit valid JSON");
    assert!(v.is_array(), "scan --json must be a JSON array");
}

#[test]
fn discover_scan_text_runs_cleanly() {
    let out = agentsight()
        .args(["discover"])
        .output()
        .expect("run agentsight");
    assert!(out.status.success());
    // Discriminating: both the "Discovered AI Agents" and "No AI agents found"
    // branches contain "ai agent" case-insensitively; a broken text path
    // (empty output) fails this.
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .to_lowercase()
            .contains("ai agent"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ── token ────────────────────────────────────────────────────────────────────

#[test]
fn skill_metrics_json_on_empty_db_is_the_report_shape() {
    // `--json` must answer with the report whatever the range holds: a caller
    // reads `event_count` and cannot be asked to tell a bare notice object
    // apart from a report. The notice stays in the human mode.
    let db = tmp("skill_metrics_empty.db");
    // The query path opens the store read-only, so the schema has to exist
    // before the command runs: create an empty store through the library API.
    agentsight::storage::sqlite::genai::GenAISqliteStore::new_with_path(
        &db,
        agentsight::config::PeriodicStoragePolicy::default(),
    )
    .expect("create an empty genai store");
    let out = agentsight()
        .args([
            "skill-metrics",
            "all",
            "--db",
            db.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_file(&db);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json must emit valid JSON");
    assert_eq!(
        v["event_count"],
        serde_json::json!(0),
        "an empty range must still report the count: {v}"
    );
    assert!(
        v.get("computed_at").is_some() && v.get("time_range_ns").is_some(),
        "the report fields must be present: {v}"
    );
    assert!(
        v.get("message").is_none(),
        "the notice object must not be the --json contract: {v}"
    );
}

#[test]
fn token_missing_data_file_exits_nonzero_with_message() {
    let missing = tmp("missing.db");
    let _ = std::fs::remove_file(&missing);
    let out = agentsight()
        .args(["token", "--data-file", missing.to_str().unwrap()])
        .output()
        .expect("run agentsight");
    assert!(
        !out.status.success(),
        "a missing --data-file must fail, not silently return zeros"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Database not found") || err.contains("not a file"),
        "stderr: {err}"
    );
}

#[test]
fn token_unreadable_db_exits_nonzero() {
    // A file that exists (passes check_data_file) but is not a valid SQLite DB
    // must fail at open time, not produce garbage.
    let garbage = tmp("garbage.db");
    std::fs::write(&garbage, b"this is not a sqlite database").unwrap();
    let out = agentsight()
        .args(["token", "--data-file", garbage.to_str().unwrap()])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_file(&garbage);
    assert!(
        !out.status.success(),
        "an unreadable DB must fail rather than succeed"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Failed to open token database"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn token_json_on_empty_db_is_valid_json() {
    // An empty file is a valid empty SQLite DB; the store creates its schema,
    // the query returns an empty result, and --json must emit valid JSON.
    let db = tmp("empty.db");
    std::fs::write(&db, b"").unwrap();
    let out = agentsight()
        .args(["token", "--data-file", db.to_str().unwrap(), "--json"])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_file(&db);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("token --json must emit valid JSON");
}

#[test]
fn token_text_on_empty_db_runs_cleanly() {
    let db = tmp("empty_text.db");
    std::fs::write(&db, b"").unwrap();
    let out = agentsight()
        .args(["token", "--data-file", db.to_str().unwrap()])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_file(&db);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Discriminating: the human-readable summary always prints a token total;
    // a broken text path (empty output) fails this.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("tokens"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
