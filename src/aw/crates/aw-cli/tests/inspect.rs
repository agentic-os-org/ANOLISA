//! Real local service and read facade, isolated from event Provider execution.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
mod common;

use aw_host::ProcessContext;
use aw_provider::view;
use aw_service::inspect;
use common::{close, event, invoke, open, Fixture, Running, PRIVATE_MARKER, TIMEOUT};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn document(f: &Fixture, mode: &str) -> Value {
    let mut doc = f.document();
    doc["spec"]["daemon"]["state_dir"] = json!(f.state());
    doc["spec"]["providers"]["policy"]["view"] = json!({
        "protocol":view::VERSION,"implementation":"tokenless",
        "transport":{"type":"stdio","location":"agent","argv":[
            doc["spec"]["providers"]["policy"]["transport"]["argv"][0],
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/view.py"),f.0,mode]}});
    doc
}

fn snapshot(f: &Fixture, doc: &Value, timeout: Duration) -> Value {
    let result = inspect::snapshot(
        &serde_json::to_vec(doc).unwrap(),
        &ProcessContext {
            cwd: f.0.clone(),
            environment: BTreeMap::new(),
            stderr_bytes: 65536,
        },
        Instant::now() + timeout,
        &AtomicBool::new(false),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&result).unwrap();
    view::Protocol::new().unwrap().check_panel(&bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for secret in [
        PRIVATE_MARKER,
        "private stderr",
        "private_body",
        "must not escape",
    ] {
        assert!(!text.contains(secret));
    }
    assert!(!text.contains(f.0.to_str().unwrap()));
    result
}

#[test]
fn offline_inspection_never_starts_service_or_event_providers() {
    let f = Fixture::new();
    let doc = document(&f, "ok");
    let result = snapshot(&f, &doc, Duration::from_secs(3));
    assert_eq!(result["service"]["state"], "offline");
    assert!(!f.state().exists());
    assert_eq!(
        result["providers"][0]["views"]["metrics.summary"]["status"],
        "ok"
    );
    assert_eq!(result["providers"][0]["admission"], "unknown");
    assert!(f.calls("describe").is_empty());
    assert!(f.calls("invoke").is_empty());
    f.assert_reaped();
}

#[test]
fn view_failures_disabled_views_and_missing_components_do_not_run_tool_hooks() {
    let fixture = Fixture::new();
    let unsupported = snapshot(&fixture, &document(&fixture, "unsupported"), TIMEOUT);
    assert_eq!(
        unsupported["providers"][0]["views"]["metrics.summary"]["status"],
        "unsupported"
    );
    fixture.assert_reaped();
    for (mode, code) in [
        ("failure", "query_failed"),
        ("identity", "identity_mismatch"),
        ("private", "schema_incompatible"),
        ("overflow", "output_limit"),
        ("timeout", "query_timeout"),
    ] {
        let f = Fixture::new();
        let doc = document(&f, mode);
        let result = snapshot(&f, &doc, Duration::from_millis(700));
        assert_eq!(
            result["providers"][0]["views"]["metrics.summary"]["code"], code,
            "{mode}"
        );
        assert!(f.calls("describe").is_empty());
        f.assert_reaped();
    }
    let f = Fixture::new();
    let mut doc = document(&f, "ok");
    doc["spec"]["providers"]["policy"]["view"]["transport"]["argv"] =
        json!(["/not-installed/view"]);
    let result = snapshot(&f, &doc, TIMEOUT);
    assert_eq!(
        result["providers"][0]["views"]["metrics.summary"]["code"],
        "component_missing"
    );
    doc["spec"]["events"]["tool.before"]["enabled"] = json!(false);
    doc["spec"]["events"]["tool.after"]["enabled"] = json!(false);
    let result = snapshot(&f, &doc, TIMEOUT);
    assert_eq!(
        result["providers"][0]["views"]["metrics.summary"]["status"],
        "disabled"
    );
    assert!(!f.0.join("views.jsonl").exists());
}

#[test]
fn read_projection_preserves_security_and_keeps_only_twenty_metadata_completions() {
    let f = Fixture::new();
    let mut doc = document(&f, "ok");
    // A second ordinary Provider has no view endpoint and must stay usable.
    doc["spec"]["providers"]["security"] = doc["spec"]["providers"]["policy"].clone();
    doc["spec"]["providers"]["security"]
        .as_object_mut()
        .unwrap()
        .remove("view");
    let mut service = Running::start(&f, &doc);
    let binding = f.bind(&service.client);
    let before = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "block"),
    );
    let result = service
        .client
        .call(invoke(&before, "check"), Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(result["effects"][0]["type"], "block");
    close(&service.client, &before);
    for _ in 0..22 {
        let after = open(
            &service.client,
            &binding,
            event(&binding, "tool.after", "observe"),
        );
        service
            .client
            .call(invoke(&after, "record"), Instant::now() + TIMEOUT)
            .unwrap();
        close(&service.client, &after);
    }
    let described = f.calls("describe").len();
    let invoked = f.calls("invoke").len();
    let result = snapshot(&f, &doc, TIMEOUT);
    assert_eq!(result["service"]["state"], "online");
    assert_eq!(
        result["service"]["generation"],
        service.client.identity().generation
    );
    assert_eq!(
        result["recent_execution"]["data"]["scope"],
        "service_generation"
    );
    let items = result["recent_execution"]["data"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 20);
    assert!(items.iter().all(|item| item["step_id"] == "record"
        && item["method"] == "invoke"
        && item["status"] == "ok"));
    assert_eq!(f.calls("describe").len(), described);
    assert_eq!(f.calls("invoke").len(), invoked);
    let old_generation = service.client.identity().generation.clone();
    service.finish().unwrap();
    let new_service = Running::start(&f, &doc);
    let result = snapshot(&f, &doc, TIMEOUT);
    assert_ne!(result["service"]["generation"], old_generation);
    assert_eq!(result["recent_execution"]["data"]["items"], json!([]));
    drop(new_service);
    f.assert_reaped();
}

#[test]
fn configuration_mismatch_and_cancellation_never_claim_current_service_state() {
    let f = Fixture::new();
    let doc = document(&f, "ok");
    let _service = Running::start(&f, &doc);
    let mut changed = doc.clone();
    changed["metadata"]["name"] = json!("changed");
    let result = snapshot(&f, &changed, TIMEOUT);
    assert_eq!(result["service"]["code"], "identity_mismatch");
    assert!(result["recent_execution"]["data"].is_null());
    let result = inspect::snapshot(
        &serde_json::to_vec(&doc).unwrap(),
        &ProcessContext {
            cwd: f.0.clone(),
            environment: BTreeMap::new(),
            stderr_bytes: 65536,
        },
        Instant::now() + TIMEOUT,
        &AtomicBool::new(true),
    )
    .unwrap();
    assert_ne!(
        result["providers"][0]["views"]["metrics.summary"]["status"],
        "ok"
    );
    f.assert_reaped();
}

#[test]
fn inspect_cli_treats_empty_runtime_directory_as_unset() {
    let f = Fixture::new();
    let bytes = serde_json::to_vec(&f.document()).unwrap();
    let path = f.0.join("aw.yaml");
    fs::write(&path, &bytes).unwrap();
    for runtime in [None, Some(""), Some("relative")] {
        let output = aw_exec::run(
            &aw_exec::CommandSpec {
                program: env!("CARGO_BIN_EXE_aw").into(),
                args: vec![
                    "inspect".into(),
                    "--config".into(),
                    path.as_os_str().into(),
                    "--json".into(),
                ],
                cwd: f.0.clone(),
                environment: runtime
                    .into_iter()
                    .map(|value| ("XDG_RUNTIME_DIR".into(), value.into()))
                    .collect(),
            },
            &[],
            aw_exec::Limits {
                input_bytes: 0,
                stdout_bytes: 1024 * 1024,
                stderr_bytes: 65536,
            },
            Instant::now() + TIMEOUT,
            &AtomicBool::new(false),
        )
        .unwrap();
        if runtime == Some("relative") {
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("service_path_must_be_absolute")
            );
        } else {
            assert!(
                output.status.success(),
                "runtime {runtime:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let panel = view::Protocol::new()
                .unwrap()
                .check_panel(&output.stdout)
                .unwrap();
            assert_eq!(panel["service"]["state"], "offline");
        }
    }
    let paths = aw_service::launch_service::resolve(&bytes, None).unwrap();
    assert!(!paths.state_dir.exists());
    assert!(f.calls("describe").is_empty());
    assert!(f.calls("invoke").is_empty());
    f.assert_reaped();
}

#[test]
fn inspect_cli_accepts_only_explicit_read_configuration_and_json() {
    let f = Fixture::new();
    let doc = document(&f, "ok");
    let path = f.0.join("aw.yaml");
    fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let context = |args: Vec<std::ffi::OsString>| aw_exec::CommandSpec {
        program: env!("CARGO_BIN_EXE_aw").into(),
        args,
        cwd: f.0.clone(),
        environment: BTreeMap::new(),
    };
    let run = |args| {
        aw_exec::run(
            &context(args),
            &[],
            aw_exec::Limits {
                input_bytes: 0,
                stdout_bytes: 1024 * 1024,
                stderr_bytes: 65536,
            },
            Instant::now() + TIMEOUT,
            &AtomicBool::new(false),
        )
        .unwrap()
    };
    let result = run(vec![
        "inspect".into(),
        "--config".into(),
        path.as_os_str().into(),
        "--json".into(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    view::Protocol::new()
        .unwrap()
        .check_panel(&result.stdout)
        .unwrap();
    for args in [
        vec!["inspect", "--config", "aw.yaml", "--json"],
        vec!["inspect", "--json"],
        vec!["inspect", "--config", path.to_str().unwrap()],
        vec![
            "inspect",
            "--config",
            path.to_str().unwrap(),
            "--json",
            "--socket",
            "/other/aw.sock",
        ],
    ] {
        assert!(!run(args.into_iter().map(Into::into).collect())
            .status
            .success());
    }
    assert!(!f.state().exists());
    assert!(f.calls("invoke").is_empty());
    f.assert_reaped();
}

#[test]
fn inspection_retains_skipped_steps_without_reporting_provider_execution() {
    let f = Fixture::new();
    let mut doc = document(&f, "ok");
    doc["spec"]["events"]["tool.before"]["steps"][0]["match"] = json!({"tools":["bash"]});
    let service = Running::start(&f, &doc);
    let binding = f.bind(&service.client);
    let before = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "block"),
    );
    let reply = service
        .client
        .call(invoke(&before, "check"), Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(reply["kind"], "skipped");
    close(&service.client, &before);
    let mut result = snapshot(&f, &doc, TIMEOUT);
    let items = result["recent_execution"]["data"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "skipped");
    assert_eq!(items[0]["status"], "ok");
    assert_eq!(items[0]["policy_block"], false);
    assert!(f.calls("invoke").is_empty());
    let protocol = view::Protocol::new().unwrap();
    result["recent_execution"]["data"]["items"][0]["kind"] = json!("unknown");
    assert!(protocol
        .check_panel(&serde_json::to_vec(&result).unwrap())
        .is_err());
    result["recent_execution"]["data"]["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("kind");
    assert!(protocol
        .check_panel(&serde_json::to_vec(&result).unwrap())
        .is_ok());
    drop(service);
    f.assert_reaped();
}
