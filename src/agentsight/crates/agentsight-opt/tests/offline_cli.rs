//! Local statistics must stay usable without credentials or network access.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use agentsight_opt::{AnalyzePipeline, AtifTrajectory};
use serde_json::{json, Value};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

struct Input(PathBuf);

impl Input {
    fn new(content: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "agentsight-offline-{}-{}.json",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, content).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_analyze"));
        command.arg(&self.0).env_remove("OPENAI_API_KEY");
        command
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn fixture(empty: bool) -> String {
    let steps = if empty {
        json!([])
    } else {
        json!([
            {"step_id":1,"source":"user","message":"Inspect the project.",
             "timestamp":"2026-01-01T00:00:00Z"},
            {"step_id":2,"source":"agent","message":"Inspecting.",
             "timestamp":"2026-01-01T00:00:02Z",
             "tool_calls":[{"tool_call_id":"a","function_name":"read","arguments":{}}],
             "observation":{"results":[{"source_call_id":"a","content":"fixture",
                                           "extra":{"is_error":false}}]},
             "metrics":{"prompt_tokens":150,"completion_tokens":20,"cached_tokens":50}},
            {"step_id":3,"source":"agent","message":"Done.",
             "timestamp":"2026-01-01T00:00:05Z"}
        ])
    };
    json!({"schema_version":"ATIF-v1.7","session_id":"fixture-offline",
           "agent":{"name":"fixture","model_name":"fixture-model"},"steps":steps})
    .to_string()
}

fn decode(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn check_stats(dim: &str, empty: bool) {
    let text = fixture(empty);
    let input = Input::new(&text);
    let actual = decode(
        input
            .command()
            .args(["--dim", dim, "--stats-only"])
            .output()
            .unwrap(),
    );
    let trajectory = AtifTrajectory::from_json(&text).unwrap();
    let expected = match dim {
        "perf" => json!({"perf": AnalyzePipeline::run_perf(&trajectory).unwrap()}),
        "cost" => json!({"cost": AnalyzePipeline::run_cost(&trajectory).unwrap()}),
        _ => unreachable!(),
    };
    assert_eq!(actual, expected);
    assert_eq!(actual.as_object().unwrap().len(), 1);
}

#[test]
fn perf_matches_the_public_computation_without_credentials() {
    check_stats("perf", false);
}

#[test]
fn cost_matches_the_public_computation_without_credentials() {
    check_stats("cost", false);
}

#[test]
fn empty_perf_is_valid() {
    check_stats("perf", true);
}

#[test]
fn empty_cost_is_valid() {
    check_stats("cost", true);
}

#[test]
fn configured_credentials_do_not_enable_network_in_local_mode() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let input = Input::new(&fixture(false));
    for dim in ["perf", "cost"] {
        let result = decode(
            input
                .command()
                .args(["--stats-only", &format!("--dim={dim}")])
                .env("OPENAI_API_KEY", "fixture-not-a-real-key")
                .env(
                    "OPENAI_BASE_URL",
                    format!("http://{}/v1", listener.local_addr().unwrap()),
                )
                .output()
                .unwrap(),
        );
        assert!(result.get(dim).is_some());
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }
}

#[test]
fn local_accuracy_is_rejected_before_loading_the_file() {
    let input = Input::new("invalid JSON");
    let output = input
        .command()
        .args(["--dim", "accuracy", "--stats-only"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("accuracy requires LLM"));
}

#[test]
fn malformed_atif_is_reported_without_an_api_key() {
    let input = Input::new("{");
    let output = input
        .command()
        .args(["--dim", "cost", "--stats-only"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to parse ATIF"));
}

#[test]
fn missing_input_keeps_a_contextual_error() {
    let input = Input::new(&fixture(false));
    std::fs::remove_file(&input.0).unwrap();
    let output = input
        .command()
        .args(["--dim", "perf", "--stats-only"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read"));
}

#[test]
fn unknown_dimensions_are_rejected() {
    let input = Input::new(&fixture(false));
    let output = input
        .command()
        .args(["--dim", "unknown", "--stats-only"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown dimension"));
}

#[test]
fn local_mode_still_requires_a_dimension() {
    let input = Input::new(&fixture(false));
    let output = input.command().arg("--stats-only").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing --dim"));
}

#[test]
fn full_analysis_still_requires_credentials() {
    let input = Input::new(&fixture(false));
    let output = input.command().args(["--dim", "perf"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("LLM not configured"));
    assert!(output.stdout.is_empty());
}

#[test]
fn help_explains_the_local_mode() {
    let output = Command::new(env!("CARGO_BIN_EXE_analyze"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--stats-only"));
}

fn check_batch_stats(dim: &str) {
    let input = Input::new(
        "{\"type\":\"user\",\"message\":{\"content\":\"检查 🐱\"}}\n\
         {\"type\":\"assistant\",\"message\":{\"content\":\"Done.\",\"usage\":{\"input_tokens\":25,\"output_tokens\":5}}}\n",
    );
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/batch-analyze.py");
    let output = Command::new("python3")
        .arg(script)
        .arg("--file")
        .arg(&input.0)
        .args([
            "--min-steps",
            "1",
            "--dim",
            dim,
            "--stats-only",
            "--binary",
            env!("CARGO_BIN_EXE_analyze"),
        ])
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap();
    let result = decode(output);
    assert_eq!(result["_steps"], 2);
    assert_eq!(result["_source_file"], input.0.to_str().unwrap());
    assert!(result.get(dim).is_some());
    assert!(result.get("perf_issues").is_none());
    assert!(result.get("cost_waste").is_none());
    if dim == "cost" {
        assert_eq!(result["cost"]["total_real_input_tok"], 25);
        assert_eq!(result["cost"]["total_real_output_tok"], 5);
    }
}

#[test]
fn batch_perf_reaches_the_real_offline_binary() {
    check_batch_stats("perf");
}

#[test]
fn batch_cost_reaches_the_real_offline_binary() {
    check_batch_stats("cost");
}

#[test]
fn batch_accuracy_rejects_local_mode_before_spawning() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/batch-analyze.py");
    let output = Command::new("python3")
        .arg(script)
        .args([
            "--file",
            "missing-fixture.jsonl",
            "--dim",
            "accuracy",
            "--stats-only",
        ])
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("accuracy requires LLM"));
    assert!(output.stdout.is_empty());
}
