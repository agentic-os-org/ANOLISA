//! Exercise Cline discovery and both read endpoints with an isolated HOME.

use super::*;
use actix_web::{App, http::StatusCode, test};
use serde_json::{Value, json};
use std::{fs, process::Command};

#[actix_web::test]
async fn discovered_cline_sessions_are_readable() {
    if std::env::var_os("ANOLISA_CLINE_ENDPOINT_CHILD").is_none() {
        let home = tempfile::tempdir().unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "cline_tests::discovered_cline_sessions_are_readable",
                "--nocapture",
            ])
            .env("ANOLISA_CLINE_ENDPOINT_CHILD", "1")
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    let home = dirs::home_dir().unwrap();
    let tasks = home.join(agentsight_trajectory_collector::cline::LINUX_TASKS_ROOT);
    let task = tasks.join("task-123");
    fs::create_dir_all(&task).unwrap();
    let storage = tasks.parent().unwrap();
    fs::create_dir_all(storage.join("state")).unwrap();
    fs::write(
        storage.join("state/taskHistory.json"),
        json!([{
            "id": "task-123",
            "cwdOnTaskInitialization": "/workspace/demo-app"
        }])
        .to_string(),
    )
    .unwrap();
    let api_path = task.join("api_conversation_history.json");
    fs::write(
        &api_path,
        json!([
            {"role":"user","content":[{"type":"text","text":"inspect auth"}]},
            {"role":"assistant","content":[{"type":"text","text":"done"}]}
        ])
        .to_string(),
    )
    .unwrap();
    fs::write(
        task.join("ui_messages.json"),
        json!([
            {"ts":1763071200000i64,"type":"say","say":"task","text":"inspect auth","conversationHistoryIndex":0},
            {"ts":1763071201000i64,"type":"say","say":"api_req_started","text":"{\"tokensIn\":10,\"tokensOut\":5,\"cost\":0.01}","conversationHistoryIndex":1}
        ])
        .to_string(),
    )
    .unwrap();

    let app = test::init_service(
        App::new()
            .service(list_local_sessions)
            .service(convert_local_to_atif)
            .service(read_local_session_file),
    )
    .await;
    let sessions: Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/local-sessions")
            .to_request(),
    )
    .await;
    assert_eq!(sessions["total"], 1);
    let session = &sessions["sessions"][0];
    assert_eq!(session["agent_id"], "cline");
    assert_eq!(session["session_id"], "task-123");
    assert_eq!(session["project"], "demo-app");
    assert_eq!(session["first_message"], "inspect auth");
    assert_eq!(session["message_count"], 2);
    assert_eq!(session["file_path"], api_path.to_string_lossy().as_ref());

    for endpoint in ["atif", "file"] {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("path", api_path.to_str().unwrap())
            .finish();
        let request = test::TestRequest::get()
            .uri(&format!("/api/local-session/{endpoint}?{query}"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
        let body = test::read_body(response).await;
        if endpoint == "atif" {
            let document: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(document["session_id"], session["session_id"]);
            assert_eq!(document["extra"]["project"], "demo-app");
            assert_eq!(document["steps"][0]["message"], "inspect auth");
            assert_eq!(document["final_metrics"]["total_prompt_tokens"], 10);
        } else {
            assert_eq!(body.as_ref(), fs::read(&api_path).unwrap());
        }
    }

    let outside = storage.join("state/taskHistory.json");
    let escape = task.join("escape.json");
    std::os::unix::fs::symlink(&outside, &escape).unwrap();
    assert!(!is_safe_session_path(outside.to_str().unwrap()));
    assert!(!is_safe_session_path(escape.to_str().unwrap()));
}
