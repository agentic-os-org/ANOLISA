//! Git and local file facts feed preservation and snapshot through the real process.
//! Scripted HTTP probabilities validate contracts, not model accuracy.

#[path = "support/fixture.rs"]
mod fixture;

use std::fs;
use std::path::Path;
use std::time::Duration;

use fixture::*;
use serde_json::{json, Value};

const PRESERVATION: &str = "preservation";
const TASK_SCOPE: &str = "task_scope";
const SNAPSHOT: &str = "snapshot";

/// git is a build prerequisite of this repository; a missing binary fails the test.
fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("git must be runnable");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Commits `greeting.txt` as `v1`; `working` is the bytes left in the working tree.
fn committed_greeting(fixture: &Fixture, working: &str) {
    let root = fixture.root.path();
    git(root, &["init", "-q", "."]);
    fs::write(root.join("greeting.txt"), "v1\n").unwrap();
    git(root, &["add", "greeting.txt"]);
    git(root, &["commit", "-q", "-m", "init"]);
    fs::write(root.join("greeting.txt"), working).unwrap();
}

fn scripted(preservation: f64, task_scope: f64, snapshot: f64) -> String {
    json!({PRESERVATION: preservation, TASK_SCOPE: task_scope, SNAPSHOT: snapshot}).to_string()
}

fn full_config(fixture: &Fixture, endpoint: &str) -> Value {
    let mut config = fixture.config(endpoint);
    config["scenarios"] = json!(["preservation", "task_scope", "snapshot"]);
    config
}

fn answers(result: &Value) -> Vec<(String, String)> {
    result["evaluation"]["answers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|answer| {
            (
                answer["unit"].as_str().unwrap().to_owned(),
                answer["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn obligation_kinds(result: &Value) -> Vec<String> {
    result["evaluation"]["outcome"]["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|obligation| obligation["action"]["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn unique_bytes_require_a_snapshot_without_a_model_question_and_stay_advisory() {
    let fixture = Fixture::new();
    committed_greeting(&fixture, "v2\n");
    let server = Server::new(&scripted(0.01, 0.99, 0.99), 200, Duration::ZERO);
    let request = fixture.request(full_config(&fixture, &server.endpoint));
    fixture.capture(&request);
    let (response, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    assert_eq!(response["effects"], json!([]));

    let received = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let mut asked: Vec<_> = received["questions"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    asked.sort();
    assert_eq!(asked, [PRESERVATION, TASK_SCOPE]);
    assert_eq!(received["state"]["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(
        received["state"]["evidence"][0]["fact"],
        json!({"kind":"rebuild","parameters":{"resource_id":"greeting","result":"unique"}})
    );

    let call = &fixture.records()[0];
    assert_eq!(
        call.names(),
        [
            "input",
            "task",
            "evidence",
            "evaluation",
            "model_request",
            "model_result",
            "result"
        ]
    );
    let evidence = call.get("evidence");
    assert_eq!(evidence["status"], "collected");
    assert_eq!(evidence["reports"][0]["source_id"], "git-rebuild");
    assert_eq!(evidence["reports"][0]["status"], "complete");
    let evaluation = call.get("evaluation");
    assert_eq!(evaluation["budget"]["max_evidence_items"], 64);
    assert_eq!(evaluation["gaps"], json!([]));
    assert_eq!(evaluation["evidence"][0]["provenance"], "local_verifier");

    let result = call.get("result");
    assert_eq!(
        answers(&result),
        [
            ("preservation".to_owned(), "allow".to_owned()),
            ("task_scope".to_owned(), "allow".to_owned()),
            ("snapshot".to_owned(), "needs_snapshot".to_owned()),
        ]
    );
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "allow"
    );
    let snapshot = &result["evaluation"]["judgments"][2];
    assert_eq!(snapshot["question_id"], SNAPSHOT);
    assert_eq!(snapshot["probability"], Value::Null);
    assert_eq!(snapshot["model_used"], false);
    assert_trace(
        &result,
        SNAPSHOT,
        "allowed",
        "local_policy",
        Some("protect_unique_bytes"),
    );
    assert_trace(&result, PRESERVATION, "allowed", "model_fallback", None);
    assert_trace(&result, TASK_SCOPE, "allowed", "model_fallback", None);
    assert_eq!(obligation_kinds(&result), ["snapshot", "audit"]);
    assert_eq!(
        result["evaluation"]["outcome"]["obligations"][0]["action"]["parameters"]["workspace_id"],
        "synthetic-workspace"
    );
    // The working bytes are untouched: a recommendation is recorded, never executed.
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("greeting.txt")).unwrap(),
        "v2\n"
    );
    assert!(!fs::read_to_string(&call.log).unwrap().contains(KEY));
}

#[test]
fn identical_bytes_without_a_model_recommendation_carry_no_snapshot() {
    let fixture = Fixture::new();
    committed_greeting(&fixture, "v1\n");
    let server = Server::new(&scripted(0.01, 0.99, 0.01), 200, Duration::ZERO);
    let request = fixture.request(full_config(&fixture, &server.endpoint));
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let received = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        received["state"]["evidence"][0]["fact"]["parameters"]["result"],
        "identical"
    );
    let result = fixture.records()[0].get("result");
    assert_eq!(
        answers(&result),
        [
            ("preservation".to_owned(), "allow".to_owned()),
            ("task_scope".to_owned(), "allow".to_owned()),
            ("snapshot".to_owned(), "allow".to_owned()),
        ]
    );
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "allow"
    );
    assert_eq!(obligation_kinds(&result), ["audit"]);
}

#[test]
fn retention_intent_blocks_and_suppresses_the_snapshot_recommendation() {
    let fixture = Fixture::new();
    committed_greeting(&fixture, "v2\n");
    let server = Server::new(&scripted(0.99, 0.99, 0.99), 200, Duration::ZERO);
    let request = fixture.request(full_config(&fixture, &server.endpoint));
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let result = fixture.records()[0].get("result");
    assert_eq!(
        answers(&result),
        [
            ("preservation".to_owned(), "preserve".to_owned()),
            ("task_scope".to_owned(), "allow".to_owned()),
            ("snapshot".to_owned(), "needs_snapshot".to_owned()),
        ]
    );
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "block"
    );
    assert_eq!(obligation_kinds(&result), ["audit"]);
}

#[test]
fn undetermined_rebuild_falls_back_except_for_preexisting_snapshot_policy() {
    let fixture = Fixture::new();
    let root = fixture.root.path();
    git(root, &["init", "-q", "."]);
    // The repository has no tracked baseline for these preexisting bytes.
    fs::write(root.join("greeting.txt"), "untracked\n").unwrap();
    let server = Server::new(&scripted(0.01, 0.99, 0.99), 200, Duration::ZERO);
    let request = fixture.request(full_config(&fixture, &server.endpoint));
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let received = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let asked: Vec<_> = received["questions"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(asked, [PRESERVATION, TASK_SCOPE]);
    assert_eq!(
        received["state"]["candidate"]["native_tool"]["input"],
        json!({"file_path":"greeting.txt","content":"synthetic full payload"})
    );
    assert_eq!(
        received["state"]["local_context"]["files"][0]["existed_at_start"],
        true
    );
    let call = &fixture.records()[0];
    assert_eq!(
        received["state"]["evidence"],
        call.get("evaluation")["evidence"]
    );
    assert_eq!(received["state"]["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(
        received["state"]["evidence"][0]["fact"]["parameters"]["result"],
        "unknown"
    );
    assert_eq!(
        call.get("evaluation")["evidence"][0]["fact"]["parameters"]["result"],
        "unknown"
    );
    let result = call.get("result");
    assert_eq!(
        answers(&result),
        [
            (PRESERVATION.to_owned(), "allow".to_owned()),
            (TASK_SCOPE.to_owned(), "allow".to_owned()),
            (SNAPSHOT.to_owned(), "needs_snapshot".to_owned()),
        ]
    );
    assert_trace(
        &result,
        PRESERVATION,
        "undetermined",
        "model_fallback",
        None,
    );
    assert_trace(&result, TASK_SCOPE, "allowed", "model_fallback", None);
    assert_trace(
        &result,
        SNAPSHOT,
        "undetermined",
        "local_policy",
        Some("protect_preexisting_bytes"),
    );
    assert_eq!(call.get("model_result")["status"], "parsed");
    assert_eq!(result["evaluation"]["judgments"][0]["model_used"], true);
    assert_eq!(result["evaluation"]["judgments"][2]["model_used"], false);
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "allow"
    );
    assert_eq!(obligation_kinds(&result), ["snapshot", "audit"]);
}

#[test]
fn unattributable_shell_write_retains_evidence_gap_and_falls_back_with_raw_args() {
    let fixture = Fixture::new();
    committed_greeting(&fixture, "v1\n");
    let server = Server::new(&scripted(0.01, 0.99, 0.99), 200, Duration::ZERO);
    let mut config = fixture.config(&server.endpoint);
    config["scenarios"] = json!(["preservation", "snapshot"]);
    config["task"]["allowed_resource_ids"] = json!([]);
    config["tools"]["Bash"] = json!({"command_pointer":"/command"});
    let mut request = fixture.request(config);
    request["event"]["tool"]["native_name"] = json!("Bash");
    request["event"]["tool"]["name"] = json!("bash");
    request["event"]["tool"]["input"] = json!({"command":"python build.py"});
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let received = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        received["state"]["candidate"]["native_tool"],
        json!({"name":"Bash","input":{"command":"python build.py"}})
    );
    assert_eq!(
        received["questions"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [PRESERVATION, SNAPSHOT]
    );
    let call = &fixture.records()[0];
    assert_eq!(call.get("model_request")["state"], received["state"]);
    assert_eq!(call.get("model_result")["status"], "parsed");
    assert_eq!(call.get("evidence")["status"], "unbound");
    let evaluation = call.get("evaluation");
    assert_eq!(evaluation["evidence"], json!([]));
    assert_eq!(
        evaluation["gaps"],
        json!([
            {"source_id":"git-rebuild","scenario":"preservation"},
            {"source_id":"git-rebuild","scenario":"snapshot"}
        ])
    );
    let result = call.get("result");
    assert_eq!(
        answers(&result),
        [
            (PRESERVATION.to_owned(), "allow".to_owned()),
            (SNAPSHOT.to_owned(), "needs_snapshot".to_owned()),
        ]
    );
    for unit in [PRESERVATION, SNAPSHOT] {
        assert_trace(&result, unit, "evidence_gap", "model_fallback", None);
    }
    for trace in result["evaluation"]["judgments"].as_array().unwrap() {
        assert_eq!(trace["model_used"], true);
        assert_eq!(trace["error"], Value::Null);
    }
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "allow"
    );
    assert_eq!(obligation_kinds(&result), ["snapshot", "audit"]);
}

#[test]
fn task_scope_only_collects_local_facts_without_git_evidence_and_still_asks_model() {
    let fixture = Fixture::new();
    committed_greeting(&fixture, "v2\n");
    let server = Server::new("0.99", 200, Duration::ZERO);
    let request = fixture.request(fixture.config(&server.endpoint));
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let call = &fixture.records()[0];
    assert_eq!(call.get("evidence"), json!({"status": "scenario_gated"}));
    let evaluation = call.get("evaluation");
    assert_eq!(evaluation["scenarios"], json!(["task_scope"]));
    assert_eq!(evaluation["evidence"], json!([]));
    assert_eq!(evaluation["budget"]["max_evidence_items"], 0);
    assert_eq!(evaluation["budget"]["max_evidence_bytes"], 2);
    assert_eq!(
        evaluation["local_context"]["candidate_revision"],
        evaluation["binding"]["candidate_revision"]
    );
    assert_eq!(
        evaluation["local_context"]["files"][0]["sha256"],
        digest("v2\n")
    );
    assert_trace(
        &call.get("result"),
        TASK_SCOPE,
        "allowed",
        "model_fallback",
        None,
    );
    assert_eq!(
        call.get("result")["evaluation"]["outcome"]["control"]["candidate"],
        "allow"
    );
}

#[test]
fn snapshot_only_local_file_policies_need_neither_credentials_nor_model() {
    for (case, before, input, state, delta, baseline, policy, answer) in [
        (
            "preexisting_write",
            Some("original\n"),
            json!({"file_path":"greeting.txt","content":"replacement\n"}),
            "regular",
            "changed",
            "undetermined",
            "protect_preexisting_bytes",
            "needs_snapshot",
        ),
        (
            "preexisting_delete",
            Some("original\n"),
            json!({"file_path":"greeting.txt"}),
            "regular",
            "unknown",
            "undetermined",
            "protect_preexisting_bytes",
            "needs_snapshot",
        ),
        (
            "noop_write",
            Some("original\n"),
            json!({"file_path":"greeting.txt","content":"original\n"}),
            "regular",
            "unchanged",
            "undetermined",
            "no_current_bytes_lost",
            "allow",
        ),
        (
            "noop_edit",
            Some("original\n"),
            json!({"file_path":"greeting.txt","old_string":"original","new_string":"original","replace_all":false}),
            "regular",
            "unchanged",
            "undetermined",
            "no_current_bytes_lost",
            "allow",
        ),
        (
            "absent_write",
            None,
            json!({"file_path":"greeting.txt","content":"new\n"}),
            "absent",
            "unknown",
            "allowed",
            "no_current_bytes_lost",
            "allow",
        ),
        (
            "unique_git_changed",
            Some("v2\n"),
            json!({"file_path":"greeting.txt","content":"v3\n"}),
            "regular",
            "changed",
            "allowed",
            "protect_unique_bytes",
            "needs_snapshot",
        ),
        (
            "unique_git_noop",
            Some("v2\n"),
            json!({"file_path":"greeting.txt","content":"v2\n"}),
            "regular",
            "unchanged",
            "allowed",
            "no_current_bytes_lost",
            "allow",
        ),
    ] {
        let fixture = Fixture::new();
        let path = fixture.root.path().join("greeting.txt");
        if case.starts_with("unique_git_") {
            committed_greeting(&fixture, before.unwrap());
        } else if let Some(bytes) = before {
            fs::write(&path, bytes).unwrap();
        }
        let server = Server::new("0.01", 500, Duration::ZERO);
        let mut config = fixture.config(&server.endpoint);
        config["scenarios"] = json!(["snapshot"]);
        config["tools"]["Delete"] = json!({"operation":"delete","path_pointer":"/file_path"});
        let mut request = fixture.request(config);
        request["event"]["tool"]["input"] = input;
        request["event"]["tool"]["native_name"] = json!(match case {
            "preexisting_delete" => "Delete",
            "noop_edit" => "Edit",
            _ => "Write",
        });
        fixture.capture(&request);
        let (response, diagnostic) = invoke(&fixture, request, false);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{case}");
        assert_eq!(response["effects"], json!([]));
        assert!(server.received.try_recv().is_err());
        let call = &fixture.records()[0];
        assert_eq!(
            call.names(),
            ["input", "task", "evidence", "evaluation", "result"]
        );
        let evaluation = call.get("evaluation");
        let local = &evaluation["local_context"];
        assert_eq!(
            local["candidate_revision"],
            evaluation["binding"]["candidate_revision"]
        );
        assert!(
            local["captured_at_ms"].as_u64().unwrap() < local["valid_until_ms"].as_u64().unwrap()
        );
        assert_eq!(
            local["files"],
            json!([{"resource_id":"greeting","state":state,"delta":delta,
            "existed_at_start":before.is_some(),"sha256":before.map(digest)}]),
            "{case}"
        );
        let result = call.get("result");
        assert_eq!(
            answers(&result),
            [(SNAPSHOT.to_owned(), answer.to_owned())],
            "{case}"
        );
        assert_trace(&result, SNAPSHOT, baseline, "local_policy", Some(policy));
        assert_eq!(result["evaluation"]["model_called"], false);
        let trace = &result["evaluation"]["judgments"][0];
        assert_eq!(trace["model_used"], false);
        assert_eq!(trace["probability"], Value::Null);
        assert_eq!(trace["error"], Value::Null);
        assert_eq!(
            result["evaluation"]["outcome"]["control"]["candidate"],
            "allow"
        );
        assert_eq!(
            obligation_kinds(&result),
            if answer == "needs_snapshot" {
                vec!["snapshot", "audit"]
            } else {
                vec!["audit"]
            }
        );
        assert_eq!(
            fs::read_to_string(&path).ok().as_deref(),
            before,
            "advisory only: {case}"
        );
        if !case.starts_with("unique_git_") {
            assert!(!fixture.root.path().join(".git").exists());
        }
    }
}

#[test]
fn file_created_after_capture_cannot_claim_preexisting_snapshot_protection() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let mut config = fixture.config(&server.endpoint);
    config["scenarios"] = json!(["snapshot"]);
    let request = fixture.request(config);
    fixture.capture(&request);
    fs::write(fixture.root.path().join("greeting.txt"), "created later\n").unwrap();
    let (_, diagnostic) = invoke(&fixture, request, false);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let call = &fixture.records()[0];
    assert_eq!(
        call.get("evaluation")["local_context"]["files"],
        json!([{
        "resource_id":"greeting","state":"regular","delta":"changed",
        "existed_at_start":false,"sha256":digest("created later\n")} ])
    );
    assert_model_error(call, "missing_credentials");
    assert_trace(
        &call.get("result"),
        SNAPSHOT,
        "undetermined",
        "model_fallback",
        None,
    );
    assert_eq!(obligation_kinds(&call.get("result")), ["audit"]);
    assert!(server.received.try_recv().is_err());
}

#[test]
fn noop_snapshot_fact_never_substitutes_for_task_authorization() {
    let fixture = Fixture::new();
    fs::write(fixture.root.path().join("greeting.txt"), "same\n").unwrap();
    let server = Server::new("0.01", 200, Duration::ZERO);
    let mut config = fixture.config(&server.endpoint);
    config["scenarios"] = json!(["task_scope", "snapshot"]);
    let mut request = fixture.request(config);
    request["event"]["tool"]["input"]["content"] = json!("same\n");
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let body = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        body["questions"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [TASK_SCOPE]
    );
    assert_eq!(
        body["state"]["local_context"]["files"][0]["delta"],
        "unchanged"
    );
    let call = &fixture.records()[0];
    assert_eq!(call.get("model_result")["status"], "parsed");
    let result = call.get("result");
    assert_eq!(
        answers(&result),
        [
            (TASK_SCOPE.to_owned(), "outside_scope".to_owned()),
            (SNAPSHOT.to_owned(), "allow".to_owned())
        ]
    );
    assert_trace(&result, TASK_SCOPE, "allowed", "model_fallback", None);
    assert_trace(
        &result,
        SNAPSHOT,
        "undetermined",
        "local_policy",
        Some("no_current_bytes_lost"),
    );
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "block"
    );
    assert_eq!(obligation_kinds(&result), ["audit"]);
}
