//! Per-session prompt capture from `input.submit` and its use by `tool.before`.

#[path = "support/fixture.rs"]
mod fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use fixture::*;
use serde_json::json;

#[test]
fn session_turns_join_in_order_and_redelivery_is_ignored() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let request = fixture.request(fixture.config(&server.endpoint));
    for (turn, text, code) in [
        ("turn/+1", "Update the synthetic greeting.", "task_captured"),
        ("turn/+1", "Redelivered first turn.", "turn_duplicate"),
        ("turn/+2", "Keep it short.", "task_captured"),
        ("turn/+3", "   ", "empty_input"),
    ] {
        let (_, diagnostic) = invoke(&fixture, fixture.submit(&request, turn, text), false);
        assert_eq!(diagnostic["code"], code, "{turn}");
    }
    assert!(fixture.records().is_empty());
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let text = "Update the synthetic greeting.\n\nKeep it short.";
    let body = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(body["state"]["authoritative_task"]["text"], text);
    assert_eq!(body["state"]["authoritative_task"]["version"], digest(text));
    let dir = fixture.records().pop().unwrap();
    let task = read(&dir.join("05-task.json"));
    assert_eq!(task["origin"], "native_user");
    assert_eq!(task["task_version"], digest(text));
    assert_eq!(task["turns"], 2);
    assert_eq!(task["text_bytes"], text.len());
    assert!(task.get("text").is_none());
    let evaluation = read(&dir.join("10-evaluation.json"));
    assert_eq!(evaluation["task"]["text"], text);
    assert_eq!(evaluation["binding"]["task_version"], digest(text));
    let sessions = fixture.records.join("sessions");
    assert_eq!(
        fs::metadata(&sessions).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    let files: Vec<_> = fs::read_dir(&sessions).unwrap().collect();
    assert_eq!(files.len(), 1);
    let file = files.into_iter().next().unwrap().unwrap();
    assert_eq!(
        file.metadata().unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert_eq!(
        format!("sha256:{}", file.file_name().to_str().unwrap()),
        evaluation["binding"]["session_id"]
    );
    assert!(!fs::read_to_string(file.path()).unwrap().contains(KEY));
}

#[test]
fn missing_or_oversized_session_task_is_uncovered_without_model() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let request = fixture.request(fixture.config(&server.endpoint));
    let mut other = request.clone();
    other["event"]["session_id"] = json!("other-native-session");
    fixture.capture(&other);
    let (_, diagnostic) = invoke(&fixture, request.clone(), true);
    assert_eq!(diagnostic["code"], "task_unavailable");
    let (_, diagnostic) = invoke(
        &fixture,
        fixture.submit(&request, "turn/+1", &"x".repeat(16_000)),
        false,
    );
    assert_eq!(diagnostic["code"], "task_captured");
    let (_, diagnostic) = invoke(
        &fixture,
        fixture.submit(&request, "turn/+2", &"y".repeat(1_000)),
        false,
    );
    assert_eq!(diagnostic["code"], "task_too_large");
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "task_too_large");
    for dir in fixture.records() {
        let result = read(&dir.join("40-result.json"));
        assert_eq!(result["status"], "uncovered");
        assert!(!dir.join("05-task.json").exists());
    }
    assert_eq!(fixture.records().len(), 2);
    assert!(server.received.try_recv().is_err());
}

#[test]
fn submission_context_is_checked_before_capture() {
    for (pointer, value, expected) in [
        ("/event/native/cwd", json!("/other"), "context_mismatch"),
        ("/event/agent/adapter", json!("pi"), "context_mismatch"),
        ("/event/turn_id", json!(" "), "missing_identity"),
        ("/event/session_id", json!("session\n"), "missing_identity"),
        (
            "/operation",
            json!("other_operation"),
            "unsupported_operation",
        ),
    ] {
        let fixture = Fixture::new();
        let request = fixture.request(fixture.config("http://127.0.0.1:1"));
        let mut submit = fixture.submit(&request, "turn/+1", TASK_TEXT);
        *submit.pointer_mut(pointer).unwrap() = value;
        let (_, diagnostic) = invoke(&fixture, submit, false);
        assert_eq!(diagnostic["code"], expected, "{pointer}");
        assert!(!fixture.records.join("sessions").exists());
    }
}
