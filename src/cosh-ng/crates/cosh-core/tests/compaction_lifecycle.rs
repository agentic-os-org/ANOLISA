//! End-to-end compaction lifecycle tests against the real cosh-core binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

mod common;

fn binary_path() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .expect("deps directory")
        .parent()
        .expect("target profile directory")
        .to_path_buf();
    path.push("cosh-core");
    path
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    store: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    Fixture {
        _temp: temp,
        home,
        workspace,
        store,
    }
}

/// Writes a config with a mock provider and an explicit compaction policy.
fn configure(fixture: &Fixture, model: &str, compaction: &str) {
    let config_dir = fixture.home.join(".copilot-shell");
    fs::create_dir_all(&config_dir).expect("create config directory");
    fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"

[ai.providers.test]
type = "mock"
model = "{model}"

[session]
auto_persist = true
persist_dir = "{}"

{compaction}
"#,
            fixture.store.display()
        ),
    )
    .expect("write config");
}

fn run_core(fixture: &Fixture, args: &[&str]) -> Output {
    let mut command = Command::new(binary_path());
    command
        .env("HOME", &fixture.home)
        .args(["--headless", "--workspace"])
        .arg(&fixture.workspace)
        .args(args);
    common::opt_out_telemetry(&mut command, &fixture.home);
    command.output().expect("run cosh-core")
}

fn json_lines(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSONL"))
        .collect()
}

fn session_id_of(messages: &[Value]) -> String {
    messages
        .iter()
        .find(|message| message["type"] == "result")
        .and_then(|message| message["session_id"].as_str())
        .expect("session id")
        .to_string()
}

fn compaction_recommendation(messages: &[Value]) -> Option<String> {
    messages.iter().find_map(|message| {
        if message["type"] != "system" {
            return None;
        }
        message["status"]
            .as_str()
            .filter(|status| status.starts_with("compaction_recommended_v1:"))
            .map(ToOwned::to_owned)
    })
}

/// Finds the single persisted session envelope beneath the store root.
fn persisted_envelope(store: &Path) -> Value {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(store, &mut found);
    assert_eq!(found.len(), 1, "expected one session file: {found:?}");
    serde_json::from_str(&fs::read_to_string(&found[0]).expect("read session file"))
        .expect("valid session JSON")
}

fn bulky_prompt(index: usize) -> String {
    format!(
        "operations step {index}: {}",
        "inspect memory pressure 排查内存 ".repeat(120)
    )
}

/// A prompt large enough that two of them cross a 16 000-token window's
/// emergency threshold on their own.
fn huge_prompt(index: usize) -> String {
    format!(
        "operations step {index}: {}",
        "inspect memory pressure 排查内存 ".repeat(600)
    )
}

const MANUAL_POLICY: &str = r#"
[session.compaction]
enabled = true
auto = false
preserve_recent_runs = 1
model_context_window = 2000000
"#;

#[test]
fn manual_compact_preserves_identity_and_appends_after_restart() {
    let fixture = fixture();
    configure(&fixture, "mock-compact-summary", MANUAL_POLICY);

    // Build four complete Agent runs across separate processes.
    let mut session_id = None;
    for index in 0..4 {
        let mut args = Vec::new();
        let id;
        if let Some(ref existing) = session_id {
            id = String::clone(existing);
            args.extend(["--resume", id.as_str()]);
        }
        let prompt = bulky_prompt(index);
        args.push(prompt.as_str());
        let messages = json_lines(&run_core(&fixture, &args));
        session_id = Some(session_id_of(&messages));
    }
    let session_id = session_id.expect("session established");

    let before = persisted_envelope(&fixture.store);
    let before_count = before["messages"].as_array().expect("messages").len();
    assert!(before.get("compaction").is_none());

    // Manual Core compaction entry point.
    let compact = run_core(&fixture, &["--resume", &session_id, "--compact"]);
    let envelope: Value = json_lines(&compact)
        .into_iter()
        .next()
        .expect("compact envelope");
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert!(compact.status.success());
    let data = &envelope["data"];
    assert_eq!(data["session_id"], session_id.as_str());
    assert_eq!(data["revision"], 1);
    assert!(data["compacted_through"].as_u64().expect("cut") > 0);
    assert!(
        data["tokens_after"]["value"].as_u64().unwrap()
            < data["tokens_before"]["value"].as_u64().unwrap()
    );
    assert!(data["tokens_after"]["source"].is_string());

    // Identity, transcript, and picker metadata survive unchanged.
    let after = persisted_envelope(&fixture.store);
    assert_eq!(after["session_id"], before["session_id"]);
    assert_eq!(after["created_at_ms"], before["created_at_ms"]);
    assert_eq!(
        after["messages"].as_array().expect("messages").len(),
        before_count
    );
    assert_eq!(after["messages"][0], before["messages"][0]);
    let projection = after.get("compaction").expect("projection persisted");
    assert_eq!(projection["revision"], 1);
    assert!(projection["summary"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));

    // Process restart: resume and run one more turn; the transcript only
    // appends the new user/assistant pair.
    let resumed = json_lines(&run_core(
        &fixture,
        &["--resume", &session_id, "continue gamma"],
    ));
    assert_eq!(session_id_of(&resumed), session_id);
    let final_envelope = persisted_envelope(&fixture.store);
    assert_eq!(
        final_envelope["messages"]
            .as_array()
            .expect("messages")
            .len(),
        before_count + 2
    );
    assert_eq!(final_envelope["messages"][0], before["messages"][0]);
    assert!(final_envelope.get("compaction").is_some());
}

#[test]
fn manual_compact_succeeds_with_one_complete_run() {
    let fixture = fixture();
    configure(
        &fixture,
        "mock-compact-summary",
        r#"
[session.compaction]
enabled = true
auto = false
preserve_recent_runs = 9
model_context_window = 2000000
"#,
    );

    let prompt = bulky_prompt(0);
    let turn = json_lines(&run_core(&fixture, &[prompt.as_str()]));
    let session_id = session_id_of(&turn);
    let before = persisted_envelope(&fixture.store);
    let message_count = before["messages"].as_array().expect("messages").len();
    assert_eq!(message_count, 2);

    let compact = run_core(&fixture, &["--resume", &session_id, "--compact"]);
    let envelope = json_lines(&compact)
        .into_iter()
        .next()
        .expect("compact envelope");
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["data"]["compacted_through"], message_count as u64,
        "{envelope}"
    );

    let after = persisted_envelope(&fixture.store);
    assert_eq!(after["messages"], before["messages"]);
    assert_eq!(
        after["compaction"]["compacted_through"],
        message_count as u64
    );
}

#[test]
fn provider_failure_never_commits_a_projection() {
    let fixture = fixture();
    configure(&fixture, "mock-compact-summary", MANUAL_POLICY);
    let mut session_id = None;
    for index in 0..3 {
        let mut args = Vec::new();
        let id;
        if let Some(ref existing) = session_id {
            id = String::clone(existing);
            args.extend(["--resume", id.as_str()]);
        }
        let prompt = bulky_prompt(index);
        args.push(prompt.as_str());
        session_id = Some(session_id_of(&json_lines(&run_core(&fixture, &args))));
    }
    let session_id = session_id.expect("session established");
    let before = persisted_envelope(&fixture.store);

    // The partial-error mock fails the summary request mid-stream.
    configure(&fixture, "mock-partial-error", MANUAL_POLICY);
    let compact = run_core(&fixture, &["--resume", &session_id, "--compact"]);
    let envelope: Value = json_lines(&compact)
        .into_iter()
        .next()
        .expect("compact envelope");
    assert_eq!(envelope["ok"], false, "{envelope}");
    assert_eq!(envelope["error"]["code"], "provider_error");
    assert!(!compact.status.success());

    let after = persisted_envelope(&fixture.store);
    assert!(after.get("compaction").is_none());
    assert_eq!(after["generation"], before["generation"]);
    assert_eq!(
        after["messages"].as_array().expect("messages").len(),
        before["messages"].as_array().expect("messages").len()
    );
}

#[test]
fn compact_without_resume_or_with_disabled_policy_is_rejected() {
    let fixture = fixture();
    configure(&fixture, "mock-compact-summary", MANUAL_POLICY);
    let missing = run_core(&fixture, &["--compact"]);
    let envelope: Value = json_lines(&missing)
        .into_iter()
        .next()
        .expect("error envelope");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "invalid_request");

    let session = json_lines(&run_core(&fixture, &["seed run"]));
    let session_id = session_id_of(&session);
    configure(
        &fixture,
        "mock-compact-summary",
        "[session.compaction]\nenabled = false\n",
    );
    let disabled = run_core(&fixture, &["--resume", &session_id, "--compact"]);
    let envelope: Value = json_lines(&disabled)
        .into_iter()
        .next()
        .expect("disabled envelope");
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "disabled");
}

#[test]
fn auto_compact_argument_combinations_fail_closed() {
    let fixture = fixture();
    configure(&fixture, "mock-compact-summary", MANUAL_POLICY);
    let session = "00000000-0000-4000-8000-000000000000";
    // Every malformed manual/auto combination must fail closed with a non-zero
    // exit and without ever emitting a success envelope — the clap constraint
    // layer rejects these before session loading, auth, or any provider work.
    // The engine-layer validation remains as a second fail-closed layer for
    // callers that construct arguments programmatically.
    let illegal: Vec<Vec<&str>> = vec![
        // auto with no expected revision at all
        vec!["--resume", session, "--compact", "--auto-compact"],
        // auto missing one half of the expected revision
        vec![
            "--resume",
            session,
            "--compact",
            "--auto-compact",
            "--expect-generation",
            "1",
        ],
        vec![
            "--resume",
            session,
            "--compact",
            "--auto-compact",
            "--expect-revision",
            "0",
        ],
        // manual run carrying an expected revision
        vec![
            "--resume",
            session,
            "--compact",
            "--expect-generation",
            "1",
            "--expect-revision",
            "0",
        ],
        vec!["--resume", session, "--compact", "--expect-generation", "1"],
        // hidden compaction flags without --compact at all
        vec![
            "--resume",
            session,
            "--auto-compact",
            "--expect-generation",
            "1",
            "--expect-revision",
            "0",
        ],
        vec!["--resume", session, "--expect-generation", "1"],
        vec!["--resume", session, "--expect-revision", "0"],
    ];
    for args in illegal {
        let output = run_core(&fixture, &args);
        assert!(
            !output.status.success(),
            "args={args:?} unexpectedly succeeded"
        );
        // No committed-compaction success envelope may be produced.
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("\"ok\":true"),
            "args={args:?} stdout={stdout}"
        );
    }
}

#[test]
fn automatic_idle_compaction_triggers_past_the_soft_threshold() {
    let fixture = fixture();
    // A large window with a tight absolute limit keeps the 70% trigger low
    // while the 90% emergency threshold stays far away.
    configure(
        &fixture,
        "mock-compact-summary",
        r#"
[session.compaction]
enabled = true
auto = true
preserve_recent_runs = 1
model_context_window = 2000000
auto_compact_token_limit = 1500
"#,
    );

    // The idle-boundary auto trigger is asynchronous: cosh-core only emits a
    // `compaction_recommended` status so it can exit and return the native
    // shell prompt immediately. The shell (not this process) owns the
    // background `--compact` compactor that actually commits the projection,
    // so the recommending process must never block on or commit compaction.
    let mut session_id = None;
    let mut recommendation: Option<String> = None;
    for index in 0..4 {
        let mut args = Vec::new();
        let id;
        if let Some(ref existing) = session_id {
            id = String::clone(existing);
            args.extend(["--resume", id.as_str()]);
        }
        let prompt = bulky_prompt(index);
        args.push(prompt.as_str());
        let messages = json_lines(&run_core(&fixture, &args));
        session_id = Some(session_id_of(&messages));
        recommendation = compaction_recommendation(&messages);
        if recommendation.is_some() {
            break;
        }
    }
    let session_id = session_id.expect("session established");
    let recommendation =
        recommendation.expect("automatic idle compaction was never recommended past the threshold");
    // Payload contract:
    //   compaction_recommended_v1:<session-id>:<gen>:<rev>:<history>:<usable>
    let fields: Vec<&str> = recommendation.split(':').collect();
    assert_eq!(fields.len(), 6, "recommendation payload: {recommendation}");
    // The recommendation is bound to the exact session that produced it.
    assert_eq!(
        fields[1], session_id,
        "recommendation payload: {recommendation}"
    );
    let history: u64 = fields[4].parse().expect("history tokens");
    let usable: u64 = fields[5].parse().expect("usable budget");
    assert!(
        usable > 0,
        "usable budget must be positive: {recommendation}"
    );
    assert!(
        history > 1500,
        "history must exceed the tightened trigger: {recommendation}"
    );

    // The recommending process stays non-blocking and commits nothing inline;
    // the complete transcript is preserved with no projection written here.
    let envelope = persisted_envelope(&fixture.store);
    assert!(
        envelope.get("compaction").is_none(),
        "idle recommendation must not commit a projection inline: {envelope}"
    );
}

#[test]
fn automatic_idle_compaction_waits_for_a_compactable_run() {
    let fixture = fixture();
    configure(
        &fixture,
        "mock-compact-summary",
        r#"
[session.compaction]
enabled = true
auto = true
preserve_recent_runs = 2
model_context_window = 2000000
auto_compact_token_limit = 1
"#,
    );

    let first_prompt = bulky_prompt(0);
    let first = json_lines(&run_core(&fixture, &[first_prompt.as_str()]));
    assert!(
        compaction_recommendation(&first).is_none(),
        "one protected run must not trigger automatic compaction: {first:?}"
    );
    let session_id = session_id_of(&first);

    let second_prompt = bulky_prompt(1);
    let second = json_lines(&run_core(
        &fixture,
        &["--resume", session_id.as_str(), second_prompt.as_str()],
    ));
    assert!(
        compaction_recommendation(&second).is_none(),
        "all runs are still protected by preserve_recent_runs: {second:?}"
    );

    let third_prompt = bulky_prompt(2);
    let third = json_lines(&run_core(
        &fixture,
        &["--resume", session_id.as_str(), third_prompt.as_str()],
    ));
    assert!(
        compaction_recommendation(&third).is_some(),
        "a newly eligible complete run should trigger compaction: {third:?}"
    );
}

#[test]
fn emergency_preflight_recovers_under_the_default_recent_run_protection() {
    let fixture = fixture();
    // Exactly two Agent runs exist at the second turn's preflight: one complete
    // run plus the in-flight prompt. `preserve_recent_runs = 2` therefore offers
    // no candidate cut at all, which used to fail closed with `context_limit:`
    // even though `/session compact` would have succeeded (#2240). The ladder
    // must degrade to one protected run and recover.
    configure(
        &fixture,
        "mock-compact-summary",
        r#"
[session.compaction]
enabled = true
auto = false
preserve_recent_runs = 2
model_context_window = 16000
"#,
    );

    let first = json_lines(&run_core(&fixture, &[huge_prompt(0).as_str()]));
    let session_id = session_id_of(&first);

    let output = run_core(
        &fixture,
        &["--resume", session_id.as_str(), huge_prompt(1).as_str()],
    );
    let messages = json_lines(&output);
    let statuses: Vec<&str> = messages
        .iter()
        .filter(|message| message["type"] == "system")
        .filter_map(|message| message["status"].as_str())
        .collect();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Self-validating: without this the assertions below would pass vacuously.
    assert!(
        statuses.contains(&"compaction_emergency_started"),
        "the scenario never reached the emergency threshold: {statuses:?}"
    );
    assert!(
        statuses.contains(&"compaction_emergency_completed"),
        "emergency compaction did not degrade past preserve_recent_runs = 2: \
         {statuses:?}\n{stdout}"
    );
    assert!(
        !statuses
            .iter()
            .any(|status| status.starts_with("compaction_emergency_failed")),
        "{statuses:?}"
    );
    assert!(!stdout.contains("context_limit:"), "{stdout}");

    // The transcript stays complete; compaction only added a projection whose
    // cut lands on the run boundary before the in-flight prompt.
    let envelope = persisted_envelope(&fixture.store);
    let transcript = envelope["messages"].as_array().expect("messages").len();
    assert_eq!(transcript, 4);
    let projection = envelope
        .get("compaction")
        .expect("a projection was committed");
    assert_eq!(projection["revision"], 1);
    assert_eq!(projection["compacted_through"], 2);
}

#[test]
fn emergency_preflight_protects_oversized_tool_free_runs() {
    let fixture = fixture();
    // Small explicit window: the burst and output reserves consume most of
    // it, so accumulated history crosses the 90% threshold quickly.
    configure(
        &fixture,
        "mock-compact-summary",
        r#"
[session.compaction]
enabled = true
auto = false
preserve_recent_runs = 1
model_context_window = 16000
"#,
    );

    let mut session_id = None;
    let mut saw_emergency = false;
    for index in 0..6 {
        let mut args = Vec::new();
        let id;
        if let Some(ref existing) = session_id {
            id = String::clone(existing);
            args.extend(["--resume", id.as_str()]);
        }
        let prompt = bulky_prompt(index);
        args.push(prompt.as_str());
        let output = run_core(&fixture, &args);
        let messages = json_lines(&output);
        saw_emergency |= messages.iter().any(|message| {
            message["type"] == "system"
                && message["status"]
                    .as_str()
                    .is_some_and(|status| status.starts_with("compaction_emergency"))
        });
        if saw_emergency {
            break;
        }
        session_id = Some(session_id_of(&messages));
    }
    assert!(
        saw_emergency,
        "emergency preflight never engaged before an oversized request"
    );
}
