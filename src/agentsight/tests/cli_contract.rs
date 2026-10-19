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
fn skill_metrics_db_path_is_checked_before_the_open() {
    // Every sibling query command checks the database it was pointed at before
    // opening it — `audit --db`, `interruption --db` and `metrics --db` always,
    // `token --data-file` when one is given — and answers with the shared
    // message. This command went straight to the open.
    let missing = tmp("skill_metrics_missing.db");
    let _ = std::fs::remove_file(&missing);
    let out = agentsight()
        .args(["skill-metrics", "all", "--db", missing.to_str().unwrap()])
        .output()
        .expect("run agentsight");
    assert!(
        !out.status.success(),
        "a missing --db must fail, not silently return zeros"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Database not found"), "stderr: {err}");

    // A directory is the plausible typo (`--db` takes the file, not the data
    // directory `serve` accepts). The open answered "SQLite database does not
    // exist" for a path that exists — the shared check names the real problem.
    let dir = tmp("skill_metrics_dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the data directory");
    let out = agentsight()
        .args(["skill-metrics", "all", "--db", dir.to_str().unwrap()])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "a directory --db must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a file"), "stderr: {err}");
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

/// Insert one interruption per (agent, type) in `dir/interruption_events.db`.
///
/// The events are distinguishable by both dimensions, so a filter that reaches
/// the query changes the reported rows instead of only the header.
fn interruption_agent_fixture(
    dir: &std::path::Path,
    events: &[(&str, agentsight::interruption::InterruptionType)],
) -> PathBuf {
    let db = dir.join("interruption_events.db");
    let store = agentsight::storage::sqlite::InterruptionStore::new_with_path(&db)
        .expect("create interruption store");
    for (agent, itype) in events {
        store
            .insert(&agentsight::interruption::InterruptionEvent::new(
                itype.clone(),
                Some(format!("fixture-{agent}")),
                None,
                None,
                None,
                None,
                Some((*agent).to_string()),
                now_ns(),
                None,
            ))
            .expect("insert interruption event");
    }
    drop(store);
    db
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
fn metrics_json_reports_the_same_rows_as_the_prometheus_lines() {
    // `metrics` was the only sight query subcommand without `--json`, so a
    // script had to parse the Prometheus exposition format to get the numbers.
    // Reverting the output branch below (always printing the text format)
    // makes the JSON parse fail.
    let dir = fixture_dir("metrics_json");
    let db = genai_fixture(&dir);

    let out = agentsight()
        .args(["metrics", "--db", db.to_str().unwrap(), "--json"])
        .output()
        .expect("run agentsight");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "--json must be accepted; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("metrics --json must emit valid JSON");
    let rows = v.as_array().expect("metrics --json must emit an array");
    assert_eq!(rows.len(), 1, "the fixture holds one agent, got {v}");
    assert_eq!(rows[0]["agent_name"], serde_json::json!("DbFlagProbe"));
    assert_eq!(rows[0]["input_tokens"], serde_json::json!(120));
    assert_eq!(rows[0]["output_tokens"], serde_json::json!(60));
    assert_eq!(rows[0]["total_tokens"], serde_json::json!(180));
    assert_eq!(rows[0]["request_count"], serde_json::json!(1));
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

#[test]
fn summary_tokenless_db_reads_the_given_store() {
    // `--db` moves the AgentSight databases, but the tokenless store lives in
    // the caller's home directory, so a report over a copy still read the
    // reading host's own stats.db. `--tokenless-db` points it at the copy: an
    // empty file is a present store with no records, which the live path
    // (absent in a fresh environment) cannot report.
    let dir = fixture_dir("summary_tokenless");
    let copy = dir.join("stats.db");
    std::fs::write(&copy, b"").expect("create an empty stats.db copy");

    let out = agentsight()
        .args([
            "summary",
            "--last",
            "24",
            "--json",
            "--tokenless-db",
            copy.to_str().unwrap(),
        ])
        .output()
        .expect("run agentsight");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json must emit valid JSON");
    assert_eq!(v["tokenless"]["available"], serde_json::json!(true), "{v}");
    assert_eq!(v["tokenless"]["records"], serde_json::json!(0), "{v}");

    // A missing copy answers "not available", the state a missing live store
    // reports too.
    let missing = dir.join("absent_stats.db");
    let out = agentsight()
        .args([
            "summary",
            "--last",
            "24",
            "--json",
            "--tokenless-db",
            missing.to_str().unwrap(),
        ])
        .output()
        .expect("run agentsight");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json must emit valid JSON");
    assert_eq!(v["tokenless"]["available"], serde_json::json!(false), "{v}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interruption_count_scopes_to_the_requested_agent() {
    // `count` had only `--last`/`--json` and always counted every unresolved
    // event on the host; the store's `list` already takes an agent filter, so
    // the per-agent total was a usage error. Reverting the wiring (passing
    // `None` for the agent) makes the scoped assertions fail.
    use agentsight::interruption::InterruptionType;

    let dir = fixture_dir("interruption_count_agent");
    let db = interruption_agent_fixture(
        &dir,
        &[
            ("ProbeA", InterruptionType::LlmError),
            ("ProbeA", InterruptionType::AgentCrash),
            ("ProbeB", InterruptionType::TokenLimit),
        ],
    );

    let count = |agent: Option<&str>| -> serde_json::Value {
        let mut args = vec![
            "interruption".to_string(),
            "--db".to_string(),
            db.to_str().unwrap().to_string(),
            "count".to_string(),
            "--last".to_string(),
            "24".to_string(),
            "--json".to_string(),
        ];
        if let Some(agent) = agent {
            args.push("--agent".to_string());
            args.push(agent.to_string());
        }
        let out = agentsight().args(&args).output().expect("run agentsight");
        assert!(
            out.status.success(),
            "--agent must be accepted; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("count --json must emit valid JSON")
    };

    let scoped = count(Some("ProbeA"));
    assert_eq!(
        scoped["total"],
        serde_json::json!(2),
        "only the requested agent's events must be counted: {scoped}"
    );
    assert_eq!(
        scoped["by_severity"]["high"],
        serde_json::json!(1),
        "{scoped}"
    );
    assert_eq!(
        scoped["by_severity"]["critical"],
        serde_json::json!(1),
        "{scoped}"
    );
    assert_eq!(
        scoped["by_severity"]["medium"],
        serde_json::json!(0),
        "{scoped}"
    );

    // The name match ignores case, like `list --agent` and the store's query.
    assert_eq!(count(Some("probea")), scoped, "the match must ignore case");

    // Without the flag every agent is still counted.
    assert_eq!(
        count(None)["total"],
        serde_json::json!(3),
        "the unfiltered view is unchanged"
    );

    // An agent with no events is an empty breakdown, not every agent's total.
    assert_eq!(
        count(Some("ProbeC"))["total"],
        serde_json::json!(0),
        "a name with no events must not fall back to the whole window"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interruption_stats_scopes_to_the_requested_agent() {
    // `list` filters by `--agent`, and the store's `stats` already accepts the
    // same filter (the dashboard passes it), but the CLI hard-coded `None`: a
    // per-agent breakdown was a usage error and the table always mixed every
    // agent on the host. Reverting the wiring (passing `None` again) makes the
    // scoped assertions below fail.
    use agentsight::interruption::InterruptionType;

    let dir = fixture_dir("interruption_stats_agent");
    let db = interruption_agent_fixture(
        &dir,
        &[
            ("ProbeA", InterruptionType::LlmError),
            ("ProbeA", InterruptionType::LlmError),
            ("ProbeB", InterruptionType::TokenLimit),
        ],
    );

    let stats = |agent: Option<&str>| -> serde_json::Value {
        let mut args = vec![
            "interruption".to_string(),
            "--db".to_string(),
            db.to_str().unwrap().to_string(),
            "stats".to_string(),
            "--last".to_string(),
            "24".to_string(),
            "--json".to_string(),
        ];
        if let Some(agent) = agent {
            args.push("--agent".to_string());
            args.push(agent.to_string());
        }
        let out = agentsight().args(&args).output().expect("run agentsight");
        assert!(
            out.status.success(),
            "--agent must be accepted; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("stats --json must emit valid JSON")
    };

    let scoped = stats(Some("ProbeA"));
    let rows = scoped.as_array().expect("stats --json must emit an array");
    assert_eq!(
        rows.len(),
        1,
        "only the rows of the requested agent must be reported: {scoped}"
    );
    assert_eq!(rows[0]["interruption_type"], serde_json::json!("llm_error"));
    assert_eq!(rows[0]["count"], serde_json::json!(2));

    // The name match ignores case, like `list --agent` and the store's query.
    let lower = stats(Some("probea"));
    assert_eq!(lower, scoped, "the agent match must be case-insensitive");

    // Without the flag the window still covers every agent.
    let all = stats(None);
    let rows = all.as_array().expect("stats --json must emit an array");
    assert_eq!(rows.len(), 2, "the unfiltered view is unchanged: {all}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A genai database holding two SKILL.md loads of one skill on two different
/// days inside the same ISO week, so the `--last` window holds both.
fn skill_loads_on_two_days(dir: &std::path::Path) -> PathBuf {
    let db = dir.join("genai_events.db");
    let store = agentsight::storage::sqlite::genai::GenAISqliteStore::new_with_path(
        &db,
        agentsight::config::PeriodicStoragePolicy::default(),
    )
    .expect("create genai store");
    drop(store);

    // Two consecutive days, both inside the same ISO week (2026-03-03 is a
    // Tuesday, 2026-03-04 a Wednesday).
    let day_ns = 86_400_000_000_000_i64;
    let first_ns = now_ns() - 2 * day_ns;
    let second_ns = now_ns() - day_ns;

    let output_messages =
        serde_json::to_string(&vec![agentsight::genai::semantic::OutputMessage {
            role: "assistant".to_string(),
            parts: vec![agentsight::genai::semantic::MessagePart::ToolCall {
                id: Some("call-fixture".to_string()),
                name: "read_file".to_string(),
                arguments: Some(serde_json::json!({"file_path": "/skills/granularity/SKILL.md"})),
            }],
            name: None,
            finish_reason: None,
        }])
        .expect("serialize output messages");

    let conn = rusqlite::Connection::open(&db).expect("open genai fixture");
    for timestamp_ns in [first_ns, second_ns] {
        conn.execute(
            "INSERT INTO genai_events
             (event_type, session_id, conversation_id, call_id, agent_name, model,
              output_messages, start_timestamp_ns, event_json)
             VALUES ('llm_call', 'granularity-session', 'granularity-session', ?1,
                     'GranularityProbe', 'gpt-4', ?2, ?3, '{}')",
            rusqlite::params![
                format!("granularity-call-{timestamp_ns}"),
                output_messages,
                timestamp_ns
            ],
        )
        .expect("insert genai event");
    }
    db
}

#[test]
fn skill_metrics_hotness_accepts_a_day_granularity() {
    // The API has accepted `granularity=day|week` all along and the dashboard
    // renders daily buckets, but the CLI never passed it: `MetricOptions`
    // always kept its weekly default, so `hotness` was the one bucket width a
    // shell could not ask for. Reverting the flag (dropping it from the
    // subcommand and keeping the default) fails the day-bucket assertion
    // below, because two loads on two days then collapse into one weekly row.
    let dir = fixture_dir("skill_hotness_granularity");
    let db = skill_loads_on_two_days(&dir);

    let bucket_labels = |args: &[&str]| -> Vec<String> {
        let mut full = vec![
            "skill-metrics".to_string(),
            "hotness".to_string(),
            "--db".to_string(),
            db.to_str().unwrap().to_string(),
            "--json".to_string(),
        ];
        full.extend(args.iter().map(|arg| arg.to_string()));
        let out = agentsight().args(&full).output().expect("run agentsight");
        assert!(
            out.status.success(),
            "{args:?} must be accepted; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("hotness --json must emit valid JSON");
        let rankings = v["hotness"]["rankings"]
            .as_array()
            .expect("the report must carry the hotness rankings");
        assert_eq!(rankings.len(), 1, "one skill was loaded: {v}");
        rankings[0]["weekly_ranks"]
            .as_array()
            .expect("each skill carries its per-bucket ranks")
            .iter()
            .map(|rank| {
                rank["iso_week"]
                    .as_str()
                    .expect("a bucket carries its label")
                    .to_string()
            })
            .collect()
    };

    // Without the flag the published weekly grouping stays the default.
    let weekly = bucket_labels(&[]);
    assert_eq!(
        weekly.len(),
        1,
        "two loads in one week stay one weekly bucket: {weekly:?}"
    );
    assert!(
        weekly[0].contains("-W"),
        "the weekly bucket label keeps its ISO-week spelling: {weekly:?}"
    );

    // Daily buckets are what the API and the dashboard already expose.
    let daily = bucket_labels(&["--granularity", "day"]);
    assert_eq!(
        daily.len(),
        2,
        "two loads on two days must be two daily buckets: {daily:?}"
    );
    assert!(
        daily.iter().all(|label| !label.contains("-W")),
        "a daily bucket is labelled with its date, not an ISO week: {daily:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn skill_metrics_granularity_is_only_declared_where_it_buckets() {
    // `granularity` buckets only the hotness trend, so the subcommands
    // without one do not declare it: the flag is not advertised where it
    // would have to be refused. `structopt` then answers "Found argument
    // '--granularity' which wasn't expected" — a usage error pointing at the
    // real problem, not a silently dropped parameter like the API's own
    // pre-fix behaviour. Reverting the declaration removal would make this
    // pass the flag through and print buckets nobody asked for.
    let dir = fixture_dir("skill_hotness_granularity_scope");
    let db = skill_loads_on_two_days(&dir);

    for subcommand in ["downloads", "loads", "usage-ratio", "distribution"] {
        let out = agentsight()
            .args([
                "skill-metrics",
                subcommand,
                "--db",
                db.to_str().unwrap(),
                "--granularity",
                "day",
                "--json",
            ])
            .output()
            .expect("run agentsight");
        assert!(
            !out.status.success(),
            "{subcommand} has no hotness trend; granularity must be refused there"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--granularity") && err.contains("wasn't expected"),
            "{subcommand} must reject the unknown flag: {err}"
        );
    }

    // An unknown value is rejected rather than silently served as weekly —
    // the same closed set the API validates against.
    let out = agentsight()
        .args([
            "skill-metrics",
            "hotness",
            "--db",
            db.to_str().unwrap(),
            "--granularity",
            "dayy",
            "--json",
        ])
        .output()
        .expect("run agentsight");
    assert!(
        !out.status.success(),
        "an unknown granularity must be refused"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("day") && err.contains("week"),
        "the error must list the valid values: {err}"
    );

    // `all` computes the hotness trend, so it accepts the flag too.
    let out = agentsight()
        .args([
            "skill-metrics",
            "all",
            "--db",
            db.to_str().unwrap(),
            "--granularity",
            "day",
            "--json",
        ])
        .output()
        .expect("run agentsight");
    assert!(
        out.status.success(),
        "`all` computes the hotness trend, so it must accept the flag; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
