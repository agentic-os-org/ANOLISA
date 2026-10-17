//! Optional evidence reads cannot extend a Hook, block closure or weaken audit failures.

use super::{common, support};
use common::{audit, close, invoke, Fixture, Running, TIMEOUT};
use serde_json::{json, Value};
use std::{
    thread,
    time::{Duration, Instant},
};
use support::{freeze, gap, hook, native_event, snapshot, trajectory, Source};

#[test]
fn item_byte_and_source_limits_report_gaps_without_truncating_the_candidate() {
    for scenario in ["items", "bytes", "source_bytes", "candidate"] {
        let fixture = Fixture::new();
        let mut history = trajectory(4);
        if scenario == "bytes" {
            history["steps"][3]["message"] = json!("x".repeat(20000));
        }
        let source = Source::new(&fixture, history);
        let mut document = fixture.document();
        source.configure(&mut document);
        let settings = &mut document["spec"]["agents"]["target"]["evidence"];
        settings["max_items"] = json!(2);
        if matches!(scenario, "bytes" | "candidate") {
            settings["max_bytes"] = json!(8192);
        }
        if scenario == "source_bytes" {
            settings["query_bytes"] = json!(128);
        }
        let service = Running::start(&fixture, &document);
        let binding = fixture.bind(&service.client);
        let mut value = native_event(&binding, "tool.after");
        if scenario == "candidate" {
            value["tool"]["result"] = json!("x".repeat(10000));
            value["native"]["tool_response"] = value["tool"]["result"].clone();
        }
        let handle = hook(&service.client, &binding, value);
        if scenario == "candidate" {
            assert!(service
                .client
                .call(freeze(&handle), Instant::now() + TIMEOUT)
                .is_err());
            let requests = source.paths.lock().unwrap().len();
            assert!(service
                .client
                .call(freeze(&handle), Instant::now() + TIMEOUT)
                .is_err());
            assert_eq!(source.paths.lock().unwrap().len(), requests);
        } else {
            let context = snapshot(&service.client, &handle);
            assert!(context["evidence"].as_array().unwrap().len() <= 2);
            assert!(
                aw_contracts::canonical::bytes(&context).unwrap().len() as u64
                    <= context["limits"]["max_bytes"].as_u64().unwrap()
            );
            let reason = match scenario {
                "items" => "item_limit",
                "bytes" => "content_byte_limit",
                _ => "source_limit",
            };
            assert!(gap(&context, reason), "{context}");
            if scenario == "items" {
                let content: Value =
                    serde_json::from_str(context["contents"][2]["text"].as_str().unwrap()).unwrap();
                assert_eq!(content["step_id"], 4);
            }
        }
        // Evidence errors do not consume Provider claims or change effect admission.
        let result = service
            .client
            .call(invoke(&handle, "record"), Instant::now() + TIMEOUT)
            .unwrap();
        assert_eq!(result["status"], "ok");
    }
}

#[test]
fn original_event_deadline_caps_a_later_context_request() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(1));
    *source.delay.lock().unwrap() = Duration::from_secs(1);
    let mut document = fixture.document();
    source.configure(&mut document);
    document["spec"]["execution"]["default_event_budget_ms"] = json!(250);
    document["spec"]["agents"]["target"]["evidence"]["query_timeout_ms"] = json!(5000);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = hook(
        &service.client,
        &binding,
        native_event(&binding, "tool.before"),
    );
    let started = Instant::now();
    assert!(service
        .client
        .call(freeze(&handle), Instant::now() + TIMEOUT)
        .is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(service
        .client
        .call(freeze(&handle), Instant::now() + TIMEOUT)
        .is_err());
    assert!(service
        .client
        .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        .is_err());
    assert!(!audit(&service.client, &handle.event_id)
        .to_string()
        .contains("context_frozen"));
}

#[test]
fn shorter_query_timeout_freezes_an_explicit_gap_and_preserves_hook_budget() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(1));
    *source.delay.lock().unwrap() = Duration::from_millis(500);
    let mut document = fixture.document();
    source.configure(&mut document);
    document["spec"]["agents"]["target"]["evidence"]["query_timeout_ms"] = json!(50);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = hook(
        &service.client,
        &binding,
        native_event(&binding, "tool.before"),
    );
    let context = snapshot(&service.client, &handle);
    assert!(gap(&context, "source_deadline"));
    assert_eq!(snapshot(&service.client, &handle), context);
    assert_eq!(
        service
            .client
            .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
            .unwrap()["status"],
        "ok"
    );
}

#[test]
fn concurrent_freeze_and_closure_do_not_wait_for_http_or_publish_late_evidence() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(1));
    *source.delay.lock().unwrap() = Duration::from_millis(700);
    let mut document = fixture.document();
    source.configure(&mut document);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = hook(
        &service.client,
        &binding,
        native_event(&binding, "tool.before"),
    );
    thread::scope(|scope| {
        let pending = scope.spawn(|| {
            service
                .client
                .call(freeze(&handle), Instant::now() + TIMEOUT)
        });
        source.wait_for(2);
        assert!(service
            .client
            .call(freeze(&handle), Instant::now() + TIMEOUT)
            .is_err());
        let started = Instant::now();
        close(&service.client, &handle);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(pending.join().unwrap().is_err());
    });
    let chain = audit(&service.client, &handle.event_id);
    assert_eq!(chain["terminal"], true);
    assert!(!chain.to_string().contains("context_frozen"));
    assert_eq!(source.paths.lock().unwrap().len(), 2);
}

#[test]
fn source_children_and_process_metadata_do_not_expand_session_scope() {
    let fixture = Fixture::new();
    let mut history = trajectory(1);
    let mut child = trajectory(1);
    child["session_id"] = json!("other-subject");
    child["steps"][0]["message"] = json!("child-private-content");
    history["subagent_trajectories"] = json!([child]);
    history["extra"] = json!({"pid":42,"process_name":"qoder", "host_id":"other-host", "boot_id":"other-boot", "start_ticks":100});
    let source = Source::new(&fixture, history);
    let mut document = fixture.document();
    source.configure(&mut document);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = hook(
        &service.client,
        &binding,
        native_event(&binding, "tool.after"),
    );
    let context = snapshot(&service.client, &handle);
    assert!(gap(&context, "subagent_scope_excluded"));
    assert!(gap(&context, "process_identity_unavailable"));
    assert!(!context.to_string().contains("child-private-content"));
    assert_eq!(context["execution_refs"].as_array().unwrap().len(), 1);
    close(&service.client, &handle);
}
