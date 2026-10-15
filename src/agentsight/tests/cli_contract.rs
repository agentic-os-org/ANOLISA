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

// ── query subcommands: --db ─────────────────────────────────────────────────
//
// `audit`, `interruption`, `metrics`, and `summary` read fixed default
// database paths and accepted no override, so a database copied from another
// host (or one in a non-default data directory) could not be queried with
// them. Each test builds a fixture database with recognizable data and
// asserts the command reports *that* database's contents.

/// A per-test directory, so the databases a test copies around (including the
/// interruption sibling `summary --db` derives) cannot collide with another
/// fixture.
fn fixture_dir(tag: &str) -> PathBuf {
    let dir = tmp(&format!("dir_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos() as i64
}

/// Insert one interruption in `dir/interruption_events.db` and return the
/// path together with the id the store generated.
fn interruption_fixture(dir: &std::path::Path) -> (PathBuf, String) {
    let db = dir.join("interruption_events.db");
    let store = agentsight::storage::sqlite::InterruptionStore::new_with_path(&db)
        .expect("create interruption store");
    store
        .insert(&agentsight::interruption::InterruptionEvent::new(
            agentsight::interruption::InterruptionType::LlmError,
            Some("db-flag-fixture".to_string()),
            None,
            None,
            None,
            None,
            Some("DbFlagProbe".to_string()),
            now_ns(),
            None,
        ))
        .expect("insert interruption event");
    drop(store);
    let conn = rusqlite::Connection::open(&db).expect("reopen interruption fixture");
    let id: String = conn
        .query_row(
            "SELECT interruption_id FROM interruption_events LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read the fixture interruption id");
    (db, id)
}

/// A genai_events.db holding one LLM call by agent `DbFlagProbe`.
fn genai_fixture(dir: &std::path::Path) -> PathBuf {
    let db = dir.join("genai_events.db");
    let store = agentsight::storage::sqlite::GenAISqliteStore::new_with_path(
        &db,
        agentsight::config::PeriodicStoragePolicy::default(),
    )
    .expect("create genai store");
    drop(store);
    let conn = rusqlite::Connection::open(&db).expect("open genai fixture");
    conn.execute(
        "INSERT INTO genai_events
         (event_type, session_id, call_id, agent_name, model, input_tokens, output_tokens, start_timestamp_ns, event_json)
         VALUES ('llm_call', 'db-flag-fixture', 'db-flag-call', 'DbFlagProbe', 'gpt-4', 120, 60, ?1, '{}')",
        rusqlite::params![now_ns()],
    )
    .expect("insert genai event");
    db
}

#[test]
fn audit_db_reads_the_given_database() {
    let dir = fixture_dir("audit");
    let db = dir.join("agentsight.db");
    let store = agentsight::AuditStore::new(&db).expect("create audit store");
    store
        .insert(&agentsight::AuditRecord {
            id: None,
            event_type: agentsight::AuditEventType::ProcessAction,
            timestamp_ns: now_ns() as u64,
            pid: std::process::id(),
            ppid: None,
            comm: "db_flag_probe".to_string(),
            duration_ns: 0,
            extra: agentsight::AuditExtra::ProcessAction {
                filename: Some("/usr/bin/db_flag_probe".to_string()),
                args: Some("--probe".to_string()),
                exit_code: Some(0),
            },
            session_id: None,
        })
        .expect("insert audit event");
    drop(store);

    let out = agentsight()
        .args([
            "audit",
            "--db",
            db.to_str().unwrap(),
            "--last",
            "24",
            "--json",
        ])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("audit --json must emit valid JSON");
    let records = v.as_array().expect("audit --json must emit an array");
    assert!(
        records
            .iter()
            .any(|r| r["comm"] == serde_json::json!("db_flag_probe")),
        "the row from --db must be reported, got {v}"
    );
}

#[test]
fn interruption_db_reads_the_given_database() {
    let dir = fixture_dir("interruption");
    let (db, id) = interruption_fixture(&dir);
    let out = agentsight()
        .args([
            "interruption",
            "--db",
            db.to_str().unwrap(),
            "list",
            "--last",
            "24",
            "--json",
        ])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("interruption --json must emit valid JSON");
    let rows = v
        .as_array()
        .expect("interruption --json must emit an array");
    assert_eq!(rows.len(), 1, "the event from --db must be listed, got {v}");
    assert_eq!(rows[0]["interruption_id"], serde_json::json!(id));
}

#[test]
fn metrics_db_reads_the_given_database() {
    let dir = fixture_dir("metrics");
    let db = genai_fixture(&dir);
    let out = agentsight()
        .args(["metrics", "--db", db.to_str().unwrap()])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("agentsight_token_input_total{agent=\"DbFlagProbe\"} 120"),
        "the series from --db must be reported, got: {stdout}"
    );
}

#[test]
fn summary_db_reads_the_given_databases() {
    let dir = fixture_dir("summary");
    let db = genai_fixture(&dir);
    // The interruption database is read from the same directory as the genai
    // database (interruption_events.db), which is what makes --db a complete
    // override for the AgentSight sources.
    let _ = interruption_fixture(&dir);

    let out = agentsight()
        .args(["summary", "--db", db.to_str().unwrap(), "--json"])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("summary --json must emit valid JSON");
    assert_eq!(
        v["sessions"]["count"],
        serde_json::json!(1),
        "the session from --db must be counted: {v}"
    );
    assert_eq!(
        v["interruptions"]["total"],
        serde_json::json!(1),
        "the interruption from the sibling of --db must be counted: {v}"
    );
}

#[test]
fn query_commands_reject_a_db_path_that_is_missing_or_not_a_file() {
    let dir = fixture_dir("missing");
    let missing = dir.join("absent.db");
    let directory = dir.join("not_a_file");

    for args in [
        vec!["audit", "--db", missing.to_str().unwrap()],
        vec!["interruption", "--db", missing.to_str().unwrap(), "list"],
        vec!["metrics", "--db", missing.to_str().unwrap()],
    ] {
        let out = agentsight().args(&args).output().expect("run agentsight");
        assert!(
            !out.status.success(),
            "{args:?} must fail rather than report zeros"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("Database not found"), "{args:?} stderr: {err}");
    }

    // `summary` documents that it always exits 0 with whatever is available
    // ("safe to run on a fresh box"), so a missing --db degrades to zeros
    // instead of failing like the three single-source commands above.
    let out = agentsight()
        .args(["summary", "--db", missing.to_str().unwrap(), "--json"])
        .output()
        .expect("run agentsight");
    assert!(
        out.status.success(),
        "summary degrades instead of failing: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("summary --json must emit valid JSON");
    assert_eq!(v["sessions"]["count"], serde_json::json!(0));
    assert_eq!(v["interruptions"]["total"], serde_json::json!(0));

    // A directory passes `exists()` but is not a database file; the shared
    // check must say so before the open fails with a closer-to-the-metal error.
    std::fs::create_dir_all(&directory).expect("create the directory fixture");
    let out = agentsight()
        .args(["metrics", "--db", directory.to_str().unwrap()])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "a directory must not be read as a database"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a file"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
