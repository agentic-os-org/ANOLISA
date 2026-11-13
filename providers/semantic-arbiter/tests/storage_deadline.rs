//! Private recording, interruption, concurrency and original-budget regressions.

#[path = "support/fixture.rs"]
mod fixture;

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::time::{Duration, Instant};

use fixture::*;
use serde_json::{json, Value};

#[test]
fn recording_requires_existing_private_nonsymlink_root() {
    for mode in [0o755, 0o770, 0o1700] {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.records, fs::Permissions::from_mode(mode)).unwrap();
        let (_, diagnostic) = invoke(
            &fixture,
            fixture.request(fixture.config("http://127.0.0.1:1")),
            false,
        );
        assert_eq!(diagnostic["code"], "invalid_record_dir");
        assert!(fixture.records().is_empty());
    }
    let fixture = Fixture::new();
    let real = fixture.root.path().join("real");
    fs::rename(&fixture.records, &real).unwrap();
    symlink(&real, &fixture.records).unwrap();
    let (_, diagnostic) = invoke(
        &fixture,
        fixture.request(fixture.config("http://127.0.0.1:1")),
        false,
    );
    assert_eq!(diagnostic["code"], "invalid_record_dir");
    assert!(fs::read_dir(&real).unwrap().next().is_none());
    fs::remove_file(&fixture.records).unwrap();
    let (_, diagnostic) = invoke(
        &fixture,
        fixture.request(fixture.config("http://127.0.0.1:1")),
        false,
    );
    assert_eq!(diagnostic["code"], "invalid_record_dir");
    assert!(!fixture.records.exists());
}

#[test]
fn session_count_quota_spares_unbound_calls_and_keeps_files_private() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1");
    config["max_sessions"] = json!(1);
    let request = fixture.request(config.clone());
    fixture.capture(&request);
    let (_, first) = invoke(&fixture, request.clone(), false);
    assert_eq!(first["code"], "missing_credentials");
    let mut other = request.clone();
    other["event"]["session_id"] = json!("another-native-session");
    let (_, submit) = invoke(
        &fixture,
        fixture.submit(&other, "turn/+1", TASK_TEXT),
        false,
    );
    assert_eq!(submit["code"], "record_unavailable");
    let (_, call) = invoke(&fixture, other, false);
    assert_eq!(call["code"], "record_unavailable");
    let mut unbound = request;
    unbound["event"]["session_id"] = json!(" ");
    let (_, call) = invoke(&fixture, unbound, false);
    assert_eq!(call["code"], "missing_identity");
    let calls = fixture.records();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().any(|call| call.session().is_none()));
    let mut pending = vec![fixture.records.clone()];
    let mut sessions = 0;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let mode = entry.metadata().unwrap().permissions().mode() & 0o7777;
            if entry.file_type().unwrap().is_dir() {
                sessions += 1;
                assert_eq!(mode, 0o700);
                pending.push(entry.path());
            } else {
                assert_eq!(mode, 0o600);
            }
        }
    }
    assert_eq!(sessions, 1);
}

#[test]
fn session_and_storage_byte_caps_count_actual_call_bytes() {
    // The smallest accepted cap: one full session task plus one admitted call.
    let cap = 4 * 1024 * 1024 + 128 * 1024;
    for shared in [false, true] {
        let fixture = Fixture::new();
        let mut config = fixture.config("http://127.0.0.1:1");
        config["max_session_bytes"] = json!(cap);
        if shared {
            config["max_storage_bytes"] = json!(cap);
        }
        let mut request = fixture.request(config);
        request["event"]["tool"]["input"]["content"] = json!("x".repeat(70_000));
        fixture.capture(&request);
        let mut recorded = 0;
        loop {
            let (_, diagnostic) = invoke(&fixture, request.clone(), false);
            if diagnostic["code"] != "oversized_candidate" {
                assert_eq!(diagnostic["code"], "record_unavailable");
                break;
            }
            recorded += 1;
            assert!(recorded < 4, "headroom must run out");
        }
        // Each call stores its full 70 KB payload, so the 128 KiB margin admits two.
        assert_eq!(recorded, 2);
        let calls = fixture.records();
        assert_eq!(calls.len(), 2);
        for call in &calls {
            assert_eq!(call.names(), ["input", "result"]);
        }
        assert!(fs::metadata(&calls[0].log).unwrap().len() <= cap);
        let mut other = request.clone();
        other["event"]["session_id"] = json!("another-native-session");
        other["event"]["tool"]["input"]["content"] = json!("short");
        fixture.capture(&other);
        let (_, diagnostic) = invoke(&fixture, other, false);
        let mut unbound = request;
        unbound["event"]["session_id"] = json!(" ");
        let (_, orphan) = invoke(&fixture, unbound, false);
        if shared {
            assert_eq!(diagnostic["code"], "record_unavailable");
            assert_eq!(orphan["code"], "record_unavailable");
            assert_eq!(fixture.records().len(), 2);
        } else {
            assert_eq!(diagnostic["code"], "missing_credentials");
            assert_eq!(orphan["code"], "missing_identity");
            assert_eq!(fixture.records().len(), 4);
        }
    }
}

#[test]
fn record_lock_wait_obeys_original_budget_and_concurrent_sessions_cannot_overcommit() {
    let fixture = Fixture::new();
    let root = File::open(&fixture.records).unwrap();
    // SAFETY: a live file descriptor is the only flock precondition.
    assert_eq!(unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_EX) }, 0);
    let mut request = fixture.request(fixture.config("http://127.0.0.1:1"));
    request["budget_ms"] = json!(100);
    let start = Instant::now();
    let (_, diagnostic) = invoke(&fixture, request, false);
    assert_eq!(diagnostic["code"], "budget_exceeded");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(fixture.records().is_empty());
    assert_eq!(unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_UN) }, 0);
    let mut config = fixture.config("http://127.0.0.1:1");
    config["max_sessions"] = json!(1);
    let children: Vec<_> = (0..8)
        .map(|index| {
            let mut request = fixture.request(config.clone());
            request["event"]["session_id"] = json!(format!("concurrent-session-{index}"));
            (spawn(fixture.root.path(), false), bind(request))
        })
        .collect();
    let outputs: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = children
            .into_iter()
            .map(|(child, request)| {
                let bytes = request.to_string().into_bytes();
                scope.spawn(move || finish(child, &bytes))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(fs::read_dir(&fixture.records).unwrap().count(), 1);
    assert_eq!(fixture.records().len(), 1);
    let mut recorded = 0;
    for output in outputs {
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        let code = diagnostic["code"].as_str().unwrap();
        assert!(["task_unavailable", "record_unavailable"].contains(&code));
        recorded += usize::from(code == "task_unavailable");
    }
    assert_eq!(recorded, 1);
}

#[test]
fn released_record_lock_allows_inference_with_remaining_budget() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let request = fixture.request(fixture.config(&server.endpoint));
    fixture.capture(&request);
    let root = File::open(&fixture.records).unwrap();
    // SAFETY: flock only accesses the live descriptor.
    assert_eq!(unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_EX) }, 0);
    std::thread::scope(|scope| {
        let pending = scope.spawn(|| invoke(&fixture, request, true));
        assert!(matches!(
            server.received.recv_timeout(Duration::from_millis(150)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(fixture.records().is_empty());
        assert_eq!(unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_UN) }, 0);
        let (_, diagnostic) = pending.join().unwrap();
        assert_eq!(diagnostic["code"], "shadow_evaluated");
    });
    assert_eq!(fixture.records().len(), 1);
    let evaluation = fixture.records()[0].get("evaluation");
    assert_eq!(
        evaluation["budget"]["deadline_ms"].as_u64().unwrap()
            - evaluation["budget"]["opened_at_ms"].as_u64().unwrap(),
        2000
    );
}

#[test]
fn input_and_model_projection_survive_killed_process() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::from_millis(200));
    let request = fixture.request(fixture.config(&server.endpoint));
    fixture.capture(&request);
    let request = bind(request);
    let mut child = spawn(fixture.root.path(), true);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    server
        .received
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    let call = &fixture.records()[0];
    assert_eq!(call.get("input")["aw_request"], request);
    assert!(call.has("model_request"));
    assert!(!call.has("result"));
    child.kill().unwrap();
    child.wait().unwrap();
    let calls = fixture.records();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].names(),
        ["input", "task", "evaluation", "model_request"]
    );
    // The killed call's session still counts against the session quota.
    let mut config = fixture.config(&server.endpoint);
    config["max_sessions"] = json!(1);
    let mut other = fixture.request(config);
    other["event"]["session_id"] = json!("another-native-session");
    let (_, diagnostic) = invoke(&fixture, other, true);
    assert_eq!(diagnostic["code"], "record_unavailable");
}

#[test]
fn pinned_root_ignores_path_replacement_during_inference() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::from_millis(200));
    let request = fixture.request(fixture.config(&server.endpoint));
    fixture.capture(&request);
    let request = bind(request);
    let mut child = spawn(fixture.root.path(), true);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    server
        .received
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    let moved = fixture.root.path().join("pinned-original");
    fs::rename(&fixture.records, &moved).unwrap();
    let outside = fixture.root.path().join("replacement");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &fixture.records).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let calls = calls_in(&moved);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].get("result")["status"], "evaluated");
    assert!(fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn preparation_time_consumes_original_aw_and_config_budgets() {
    for config_cap in [false, true] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let mut config = fixture.config(&server.endpoint);
        if config_cap {
            config["model"]["timeout_ms"] = json!(20);
        }
        let mut request = fixture.request(config);
        if !config_cap {
            request["budget_ms"] = json!(20);
        }
        let request = bind(request);
        let child = spawn(fixture.root.path(), true);
        std::thread::sleep(Duration::from_millis(100));
        let output = finish(child, request.to_string().as_bytes());
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["code"], "budget_exceeded");
        assert!(fixture.records().is_empty());
        assert!(server.received.try_recv().is_err());
    }
}

#[test]
fn slow_http_keeps_budget_failure_and_no_effects() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::from_millis(400));
    let mut request = fixture.request(fixture.config(&server.endpoint));
    fixture.capture(&request);
    request["budget_ms"] = json!(150);
    let start = Instant::now();
    invoke(&fixture, request, true);
    assert!(start.elapsed() < Duration::from_millis(380));
    let result = fixture.records()[0].get("result");
    assert_eq!(fixture.records()[0].get("model_result")["error"], "timeout");
    assert_eq!(
        result["evaluation"]["answers"][0]["reason"],
        "budget_exceeded"
    );
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "nop"
    );
}

#[test]
fn process_cwd_mismatch_is_uncovered_and_candidate_symlink_is_not_read() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let request = bind(fixture.request(fixture.config(&server.endpoint)));
    let output = finish(
        spawn(&fixture.records, true),
        request.to_string().as_bytes(),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
        "context_mismatch"
    );
    assert!(server.received.try_recv().is_err());
    let target = fixture.root.path().join("private-target");
    fs::write(&target, "never-read-secret-marker").unwrap();
    symlink(&target, fixture.root.path().join("greeting.txt")).unwrap();
    let request = fixture.request(fixture.config(&server.endpoint));
    fixture.capture(&request);
    invoke(&fixture, request, true);
    let body = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(!body.to_string().contains("never-read-secret-marker"));
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        "never-read-secret-marker"
    );
}
