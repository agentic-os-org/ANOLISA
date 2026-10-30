//! Exercise the public statistics bridge through its actual optional binary entry.

use aw_exec::{CommandSpec, Limits};
use aw_provider::{view, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const PRIVATE: &str = "private-statistics-body-and-stderr";

struct Fixture {
    root: tempfile::TempDir,
    cli: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(mode: &str, store: bool) -> Self {
        let build = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/panel-validation");
        fs::create_dir_all(&build).unwrap();
        let root = tempfile::tempdir_in(build).unwrap();
        let cli = root.path().join("tokenless");
        let data = root.path().join("state");
        if store {
            fs::create_dir(&data).unwrap();
            fs::write(data.join("stats.db"), "public CLI fixture owns this file").unwrap();
        }
        fs::write(&cli, format!(r#"#!/usr/bin/python3
import json, os, sys, time
from pathlib import Path
mode = {mode:?}
root = Path(__file__).parent
(root / 'pid').write_text(str(os.getpid()))
assert sys.argv[1:] == ['stats', 'summary', '--json', '--limit', '200']
assert os.environ['TOKENLESS_DATA_DIR'] == str(root / 'state')
if mode == 'slow': time.sleep(10)
if mode == 'failed': print({PRIVATE:?}, file=sys.stderr); sys.exit(7)
if mode == 'overflow': print('x' * (1024*1024+1)); sys.exit(0)
if mode == 'stderr_overflow': print('x' * 65537, file=sys.stderr); sys.exit(0)
if mode == 'duplicate': print('{{"schema_version":"1.1","schema_version":"2"}}'); sys.exit(0)
if mode == 'invalid': print('not json'); sys.exit(0)
total = {{'records':0 if mode == 'empty' else 12,'before_tokens':10000,'after_tokens':6500,'tokens_saved':3500}}
if mode == 'empty': total.update(before_tokens=0, after_tokens=0, tokens_saved=0)
if mode == 'invalid_total': total['tokens_saved'] = 9999
if mode == 'unsafe_number': total['before_tokens'] = 9007199254740992
if mode == 'too_many': total['records'] = 201
ops = {{'compress-response': dict(total)}}
if mode == 'mismatch': ops['compress-response']['records'] = 1
print(json.dumps({{'schema_version':'2' if mode == 'version' else '1.1',
 'total':total, 'by_operation':ops, 'dry_run_records_excluded':188,
 'attribution':{{'net_savings_tokens':-999999}},'before_text':{PRIVATE:?}, 'after_text':{PRIVATE:?}}}))
"#)).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, cli, data }
    }

    fn request(&self, method: &str) -> Value {
        let mut value = json!({"protocol":view::VERSION,"operation":method,
            "request_id":"view-1","provider_id":"tokenless-main","config_revision":"a".repeat(64),
            "budget_ms":if method == "query" { 500 } else { 2000 }});
        if method == "query" {
            value["view"] = json!("metrics.summary");
            value["parameters"] = json!({"limit":200});
        }
        value
    }

    fn command(&self, extra: &[(&str, &str)]) -> CommandSpec {
        let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
        for key in ["TOKENLESS_STATS_DB", "DSH_TOKENLESS_STATS_DB", "DSH_SHELL"] {
            environment.remove(std::ffi::OsStr::new(key));
        }
        for (key, value) in extra {
            environment.insert((*key).into(), (*value).into());
        }
        CommandSpec {
            program: env!("CARGO_BIN_EXE_aw-provider-tokenless").into(),
            args: vec![
                "--view".into(),
                "--cli".into(),
                self.cli.as_os_str().into(),
                "--data-dir".into(),
                self.data.as_os_str().into(),
            ],
            cwd: self.root.path().into(),
            environment,
        }
    }

    fn run(&self, request: &Value, extra: &[(&str, &str)]) -> Value {
        let result = aw_exec::run_cooperative(
            &self.command(extra),
            &serde_json::to_vec(request).unwrap(),
            Limits {
                input_bytes: MAX_MESSAGE_BYTES,
                stdout_bytes: MAX_MESSAGE_BYTES,
                stderr_bytes: 65536,
            },
            Instant::now() + Duration::from_secs(4),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(!output.contains(PRIVATE));
        view::Protocol::new()
            .unwrap()
            .check_response(request, output.as_bytes())
            .unwrap()
    }

    fn assert_reaped(&self) {
        if let Ok(pid) = fs::read_to_string(self.root.path().join("pid")) {
            assert!(
                !Path::new("/proc").join(pid.trim()).exists(),
                "query child still exists"
            );
        }
    }
}

#[test]
fn description_and_missing_store_never_invoke_or_initialize_component() {
    let fixture = Fixture::new("ok", false);
    assert_eq!(
        fixture.run(&fixture.request("describe"), &[])["data"]["implementation"],
        "tokenless"
    );
    let reply = fixture.run(&fixture.request("query"), &[]);
    assert_eq!(reply["status"], "no_data");
    assert_eq!(reply["code"], "store_absent");
    assert!(!fixture.data.exists());
    assert!(!fixture.root.path().join("pid").exists());
}

#[test]
fn public_summary_is_whitelisted_and_has_explicit_scope_estimates_and_unknown_adoption() {
    let fixture = Fixture::new("ok", true);
    let reply = fixture.run(
        &fixture.request("query"),
        &[("TOKENLESS_STATS_ENABLED", "0")],
    );
    assert_eq!(reply["status"], "ok");
    let data = &reply["data"];
    assert_eq!(data["records"], 12);
    assert_eq!(data["saved_tokens_estimated"], 3500);
    assert_eq!(data["saved_percent"], 35.0);
    assert_eq!(data["scope"], "component_store_recent_records");
    assert_eq!(data["requested_limit"], 200);
    assert_eq!(data["recording_state"], "unknown");
    assert_eq!(data["adoption"], "unverified");
    assert!(data["range_start"].is_null());
    assert!(data.get("attribution").is_none());
    assert!(data.get("before_text").is_none());
    fixture.assert_reaped();
}

#[test]
fn component_failures_are_bounded_and_do_not_return_raw_diagnostics() {
    for (mode, code) in [
        ("failed", "query_failed"),
        ("slow", "query_timeout"),
        ("overflow", "output_limit"),
        ("stderr_overflow", "output_limit"),
        ("duplicate", "query_failed"),
        ("invalid", "query_failed"),
        ("version", "schema_incompatible"),
        ("invalid_total", "query_failed"),
        ("unsafe_number", "query_failed"),
        ("too_many", "query_failed"),
        ("mismatch", "query_failed"),
    ] {
        let fixture = Fixture::new(mode, true);
        let reply = fixture.run(&fixture.request("query"), &[]);
        assert_eq!(reply["status"], "unavailable", "{mode}");
        assert_eq!(reply["code"], code, "{mode}");
        assert!(reply["data"].is_null());
        fixture.assert_reaped();
    }
}

#[test]
fn empty_history_and_unresolved_database_overrides_are_not_zero_savings() {
    let fixture = Fixture::new("empty", true);
    let reply = fixture.run(&fixture.request("query"), &[]);
    assert_eq!(reply["status"], "no_data");
    assert!(reply["data"].is_null());
    fixture.assert_reaped();
    let fixture = Fixture::new("ok", true);
    for extra in [
        vec![("TOKENLESS_STATS_DB", "/other/stats.db")],
        vec![
            ("DSH_SHELL", "1"),
            ("DSH_TOKENLESS_STATS_DB", "/other/stats.db"),
        ],
    ] {
        let reply = fixture.run(&fixture.request("query"), &extra);
        assert_eq!(reply["status"], "unsupported");
        assert_eq!(reply["code"], "view_unsupported");
    }
    assert!(!fixture.root.path().join("pid").exists());
}

#[test]
fn cancelling_view_wrapper_reaps_its_separate_statistics_process_group() {
    let fixture = Fixture::new("slow", true);
    let mut request = fixture.request("query");
    request["budget_ms"] = json!(2000);
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !fixture.root.path().join("pid").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            cancelled.store(true, std::sync::atomic::Ordering::Release);
        });
        let result = aw_exec::run_cooperative(
            &fixture.command(&[]),
            &serde_json::to_vec(&request).unwrap(),
            Limits {
                input_bytes: MAX_MESSAGE_BYTES,
                stdout_bytes: MAX_MESSAGE_BYTES,
                stderr_bytes: 65536,
            },
            Instant::now() + Duration::from_secs(3),
            &cancelled,
        );
        assert!(matches!(result, Err(aw_exec::Error::Cancelled)));
    });
    assert!(fixture.root.path().join("pid").exists());
    fixture.assert_reaped();
}

#[test]
#[ignore = "requires a built public Tokenless CLI; package CI runs this contract"]
fn bundled_cli_statistics_exclude_dry_runs_and_match_selected_candidate_totals() {
    let mut fixture = Fixture::new("ok", false);
    fixture.data = fs::canonicalize(fixture.root.path()).unwrap().join("state");
    fixture.cli = std::env::var_os("AW_TOKENLESS_TEST_CLI")
        .expect("set AW_TOKENLESS_TEST_CLI")
        .into();
    assert!(fixture.cli.is_absolute());
    let input = json!({"protocol_version":2,"operation":"post_tool","attribution":{"agent_id":"aw-view-contract","session_id":"test-session","tool_use_id":"test-call"},
        "input":{"result_kind":"tool","tool_name":"api","content":serde_json::to_string_pretty(&json!({"numbers":(0..100).collect::<Vec<_>>(),"nested":{"ok":true}})).unwrap(),
            "status":"success","content_origin":"api_response","output_optimization":"none",
            "capabilities":{"replace_output":true,"replace_with_text":true,"recovery":{"kind":"none"}}}});
    for (enabled, disposition) in [("0", "dry_run"), ("1", "applied")] {
        let mut command = fixture.command(&[]);
        command.program = fixture.cli.clone();
        command.args = vec!["compress".into()];
        command.environment.retain(|key, _| {
            !key.to_string_lossy().starts_with("TOKENLESS_")
                && !key.to_string_lossy().starts_with("DSH_")
        });
        for (key, value) in [
            ("TOKENLESS_COMPRESSION_ENABLED", enabled),
            ("TOKENLESS_STATS_ENABLED", "1"),
            ("TOKENLESS_SLS_ENABLED", "0"),
        ] {
            command.environment.insert(key.into(), value.into());
        }
        command
            .environment
            .insert("TOKENLESS_DATA_DIR".into(), fixture.data.as_os_str().into());
        let output = aw_exec::run(
            &command,
            &serde_json::to_vec(&input).unwrap(),
            Limits {
                input_bytes: MAX_MESSAGE_BYTES,
                stdout_bytes: MAX_MESSAGE_BYTES,
                stderr_bytes: 65536,
            },
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            view::decode(&output.stdout).unwrap()["result"]["disposition"],
            disposition
        );
        assert!(
            fixture.data.join("stats.db").is_file(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reply = fixture.run(
            &fixture.request("query"),
            &[
                ("TOKENLESS_STATS_ENABLED", "0"),
                ("TOKENLESS_SLS_ENABLED", "0"),
            ],
        );
        if enabled == "0" {
            assert_eq!(reply["status"], "no_data");
        } else {
            assert_eq!(reply["status"], "ok", "{reply}");
            assert_eq!(reply["data"]["records"], 1);
            let mut summary = command;
            summary.args = ["stats", "summary", "--json", "--limit", "200"]
                .map(Into::into)
                .to_vec();
            let output = aw_exec::run(
                &summary,
                &[],
                Limits {
                    input_bytes: 0,
                    stdout_bytes: MAX_MESSAGE_BYTES,
                    stderr_bytes: 65536,
                },
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
            )
            .unwrap();
            assert!(output.status.success());
            let summary = view::decode(&output.stdout).unwrap();
            assert_eq!(summary["dry_run_records_excluded"], 1);
            assert_eq!(
                reply["data"]["before_tokens_estimated"],
                summary["total"]["before_tokens"]
            );
            assert_eq!(
                reply["data"]["after_tokens_estimated"],
                summary["total"]["after_tokens"]
            );
            assert_eq!(
                reply["data"]["saved_tokens_estimated"],
                summary["total"]["tokens_saved"]
            );
        }
    }
}
