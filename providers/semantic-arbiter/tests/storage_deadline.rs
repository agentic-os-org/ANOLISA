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
fn private_files_and_fixed_reserve_enforce_both_quotas() {
    for byte_quota in [false, true] {
        let fixture = Fixture::new();
        let mut config = fixture.config("http://127.0.0.1:1");
        if byte_quota {
            config["max_storage_bytes"] = json!(4 * 1024 * 1024);
        } else {
            config["max_records"] = json!(1);
        }
        let (_, first) = invoke(&fixture, fixture.request(config.clone()), false);
        assert_eq!(first["code"], "missing_credentials");
        let (_, second) = invoke(&fixture, fixture.request(config), false);
        assert_eq!(second["code"], "record_unavailable");
        assert_eq!(fixture.records().len(), 1);
        let dir = &fixture.records()[0];
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        for file in fs::read_dir(dir).unwrap() {
            assert_eq!(
                file.unwrap().metadata().unwrap().permissions().mode() & 0o7777,
                0o600
            );
        }
    }
}

#[test]
fn record_lock_is_nonblocking_and_concurrency_cannot_overreserve() {
    let fixture = Fixture::new();
    let root = File::open(&fixture.records).unwrap();
    // SAFETY: a live file descriptor is the only flock precondition.
    assert_eq!(unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_EX) }, 0);
    let start = Instant::now();
    let (_, diagnostic) = invoke(
        &fixture,
        fixture.request(fixture.config("http://127.0.0.1:1")),
        false,
    );
    assert_eq!(diagnostic["code"], "record_unavailable");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(unsafe { libc::flock(root.as_raw_fd(), libc::LOCK_UN) }, 0);
    let mut config = fixture.config("http://127.0.0.1:1");
    config["max_records"] = json!(1);
    let request = bind(fixture.request(config));
    let children: Vec<_> = (0..8).map(|_| spawn(fixture.root.path(), false)).collect();
    let outputs: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = children
            .into_iter()
            .map(|child| {
                let bytes = request.to_string().into_bytes();
                scope.spawn(move || finish(child, &bytes))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(fixture.records().len(), 1);
    let mut recorded = 0;
    for output in outputs {
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        let code = diagnostic["code"].as_str().unwrap();
        assert!(["missing_credentials", "record_unavailable"].contains(&code));
        recorded += usize::from(code == "missing_credentials");
    }
    assert_eq!(recorded, 1);
}

#[test]
fn input_and_model_projection_survive_killed_process() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::from_millis(200));
    let request = bind(fixture.request(fixture.config(&server.endpoint)));
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
    let dir = &fixture.records()[0];
    assert_eq!(read(&dir.join("00-input.json"))["aw_request"], request);
    assert!(dir.join("20-model-request.json").is_file());
    assert!(!dir.join("40-result.json").exists());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(dir.join("00-input.json").is_file());
    let mut config = fixture.config(&server.endpoint);
    config["max_records"] = json!(1);
    let (_, diagnostic) = invoke(&fixture, fixture.request(config), true);
    assert_eq!(diagnostic["code"], "record_unavailable");
}

#[test]
fn pinned_root_ignores_path_replacement_during_inference() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::from_millis(200));
    let request = bind(fixture.request(fixture.config(&server.endpoint)));
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
    let name = fixture.records()[0].file_name().unwrap().to_owned();
    let moved = fixture.root.path().join("pinned-original");
    fs::rename(&fixture.records, &moved).unwrap();
    let outside = fixture.root.path().join("replacement");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &fixture.records).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        read(&moved.join(name).join("40-result.json"))["status"],
        "evaluated"
    );
    assert!(fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn atomic_publication_never_overwrites_existing_result() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::from_millis(200));
    let request = bind(fixture.request(fixture.config(&server.endpoint)));
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
    let result = fixture.records()[0].join("40-result.json");
    fs::write(&result, "sentinel").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(fs::read_to_string(result).unwrap(), "sentinel");
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "record_write_failed");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["effects"],
        json!([])
    );
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
        let dir = &fixture.records()[0];
        let evaluation = read(&dir.join("10-evaluation.json"));
        assert_eq!(
            evaluation["budget"]["deadline_ms"].as_u64().unwrap()
                - evaluation["budget"]["opened_at_ms"].as_u64().unwrap(),
            20
        );
        assert_eq!(read(&dir.join("40-result.json"))["code"], "budget_exceeded");
        assert!(server.received.try_recv().is_err());
    }
}

#[test]
fn slow_http_keeps_budget_failure_and_no_effects() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::from_millis(400));
    let mut request = fixture.request(fixture.config(&server.endpoint));
    request["budget_ms"] = json!(150);
    let start = Instant::now();
    invoke(&fixture, request, true);
    assert!(start.elapsed() < Duration::from_millis(380));
    let result = read(&fixture.records()[0].join("40-result.json"));
    assert_eq!(
        read(&fixture.records()[0].join("30-model-result.json"))["error"],
        "timeout"
    );
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
    invoke(
        &fixture,
        fixture.request(fixture.config(&server.endpoint)),
        true,
    );
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
