//! Exercise discovery and both read endpoints with an isolated process HOME.

use super::*;
use actix_web::{App, http::StatusCode, test};
use serde_json::{Value, json};
use std::{fs, process::Command};

#[actix_web::test]
async fn discovered_gemini_sessions_are_readable() {
    if std::env::var_os("ANOLISA_GEMINI_ENDPOINT_CHILD").is_none() {
        let home = tempfile::tempdir().unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "gemini_tests::discovered_gemini_sessions_are_readable",
                "--nocapture",
            ])
            .env("ANOLISA_GEMINI_ENDPOINT_CHILD", "1")
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
    let chats = home.join(".gemini/tmp/fixture-project/chats");
    fs::create_dir_all(&chats).unwrap();
    let messages = json!([
        {"id":"u","type":"user","content":"fixture question"},
        {"id":"a","type":"gemini","content":"fixture answer"}
    ]);
    let legacy = json!({"sessionId":"legacy","projectHash":"fixture-project","messages":messages});
    fs::write(chats.join("session-legacy.json"), legacy.to_string()).unwrap();
    let mut stream = format!(
        "{}\n",
        json!({"sessionId":"stream","projectHash":"fixture-project"})
    );
    for message in messages.as_array().unwrap() {
        stream.push_str(&format!("{message}\n"));
    }
    fs::write(chats.join("session-stream.jsonl"), &stream).unwrap();
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
    assert_eq!(sessions["total"], 2);
    for session in sessions["sessions"].as_array().unwrap() {
        assert_eq!(session["agent_id"], "gemini-cli");
        let path = session["file_path"].as_str().unwrap();
        for endpoint in ["atif", "file"] {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("path", path)
                .finish();
            let request = test::TestRequest::get()
                .uri(&format!("/api/local-session/{endpoint}?{query}"))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK, "{endpoint}: {path}");
            let body = test::read_body(response).await;
            if endpoint == "atif" {
                let document: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(document["session_id"], session["session_id"]);
                assert_eq!(document["steps"][0]["message"], "fixture question");
            } else {
                assert_eq!(body.as_ref(), fs::read(path).unwrap());
            }
        }
    }
    // Allowing the Gemini session root must not permit adjacent files or escapes.
    let outside = home.join(".gemini/private.json");
    fs::write(&outside, "{\"private\":true}").unwrap();
    let escape = chats.join("session-escape.json");
    std::os::unix::fs::symlink(&outside, &escape).unwrap();
    for path in [&outside, &escape] {
        assert!(!is_safe_session_path(path.to_str().unwrap()));
    }
    assert!(!is_safe_session_path(
        chats.join("../outside.txt").to_str().unwrap()
    ));
}
