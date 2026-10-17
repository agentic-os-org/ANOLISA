//! Real service identity, immutable evidence and metadata-only journal acceptance.
#![cfg(target_os = "linux")]

#[path = "evidence/bounds.rs"]
mod bounds;
// The shared service fixture also supports Provider scheduling tests.
#[allow(dead_code)]
mod common;
#[path = "evidence/completion.rs"]
mod completion;
#[path = "evidence/lifecycle.rs"]
mod lifecycle;
#[path = "evidence/source_contract.rs"]
mod source_contract;
#[path = "evidence/support.rs"]
mod support;

use aw_contracts::{canonical, evidence::validate_context, Registry};
use aw_service::{Operation, Server};
use common::{audit, close, event, open, Fixture, Running, PRIVATE_MARKER, TIMEOUT};
use serde_json::{json, Value};
use std::time::Instant;
use support::{freeze, gap, hook, native_event, snapshot, trajectory, Source, HISTORY, SECRET};

#[test]
fn verified_native_identity_freezes_history_without_counting_model_echoes() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(2));
    let mut document = fixture.document();
    source.configure(&mut document);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let value = native_event(&binding, "tool.after");
    let handle = hook(&service.client, &binding, value.clone());
    let context = snapshot(&service.client, &handle);
    assert_eq!(context["evidence"].as_array().unwrap().len(), 3);
    assert_eq!(context["execution_refs"].as_array().unwrap().len(), 1);
    assert_eq!(
        context["evidence"][0]["association"]["basis"],
        "native_callback"
    );
    for entry in &context["evidence"].as_array().unwrap()[1..] {
        assert_eq!(entry["association"]["status"], "verified");
        assert_eq!(entry["association"]["basis"], "native_session");
        assert_eq!(entry["association"]["tool_call_id"], Value::Null);
        assert_eq!(entry["derivation"], "model_echo");
        assert_eq!(entry["execution_ref"], Value::Null);
        assert_eq!(entry["source"]["instance_id"], Value::Null);
        assert_eq!(entry["source"]["version"], Value::Null);
        assert_eq!(entry["trust"], "source_reported");
    }
    assert!(gap(&context, "process_identity_unavailable"));
    assert!(gap(&context, "source_time_unverified"));
    validate_context(
        &Registry::new().unwrap(),
        &context,
        &context["subject"],
        &handle.candidate_revision,
    )
    .unwrap();
    assert!(canonical::bytes(&context).is_ok());
    let candidate: Value =
        serde_json::from_str(context["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(candidate["tool"]["input"]["ratio"], 0.125);
    // A repeated callback and source records received later cannot renew the event.
    let duplicate = hook(&service.client, &binding, value);
    assert_eq!(handle.event_id, duplicate.event_id);
    assert_eq!(handle.candidate_revision, duplicate.candidate_revision);
    *source.body.lock().unwrap() = trajectory(5);
    assert_eq!(snapshot(&service.client, &duplicate), context);
    assert_eq!(source.paths.lock().unwrap().len(), 2);
    close(&service.client, &handle);
    assert!(service
        .client
        .call(freeze(&handle), Instant::now() + TIMEOUT)
        .is_err());
    let records = audit(&service.client, &handle.event_id);
    assert_eq!(
        records["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["record"]["phase"] == "context_frozen")
            .count(),
        1
    );
    let durable = records.to_string();
    for private in [PRIVATE_MARKER, HISTORY, SECRET, "json-utf8", "tool_input"] {
        assert!(!durable.contains(private), "retained {private}");
    }
}

#[test]
fn source_scope_and_service_instances_cannot_be_substituted() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(1));
    let mut document = fixture.document();
    source.configure(&mut document);
    document["spec"]["agents"]["other"] = document["spec"]["agents"]["target"].clone();
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let first = hook(
        &service.client,
        &binding,
        native_event(&binding, "tool.before"),
    );
    let mut operation = fixture.bind_operation();
    if let Operation::Bind { target, .. } = &mut operation {
        *target = "other".into();
    }
    let other: aw_service::Binding = serde_json::from_value(
        service
            .client
            .call(operation, Instant::now() + TIMEOUT)
            .unwrap(),
    )
    .unwrap();
    let mut value = native_event(&other, "tool.before");
    value["agent"]["binding_id"] = json!("other");
    let second = hook(&service.client, &other, value);
    let mut wrong_instance = freeze(&first);
    if let Operation::FreezeContext { instance_id, .. } = &mut wrong_instance {
        *instance_id = other.instance_id.clone();
    }
    assert!(service
        .client
        .call(wrong_instance, Instant::now() + TIMEOUT)
        .is_err());
    let mut stale = freeze(&first);
    if let Operation::FreezeContext {
        candidate_revision, ..
    } = &mut stale
    {
        *candidate_revision = "0".repeat(64);
    }
    assert!(service
        .client
        .call(stale, Instant::now() + TIMEOUT)
        .is_err());
    assert!(source.paths.lock().unwrap().is_empty());
    let original = snapshot(&service.client, &first);
    let rejected = snapshot(&service.client, &second);
    assert!(gap(&rejected, "source_session_conflict"));
    assert_eq!(rejected["evidence"].as_array().unwrap().len(), 1);
    assert!(!rejected.to_string().contains(HISTORY));
    assert_eq!(source.paths.lock().unwrap().len(), 2);
    assert!(validate_context(
        &Registry::new().unwrap(),
        &original,
        &rejected["subject"],
        &first.candidate_revision
    )
    .is_err());
    close(&service.client, &first);
    close(&service.client, &second);
}

#[test]
fn arbitrary_session_ids_and_pid_reuse_never_open_a_source_query() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(1));
    let mut document = fixture.document();
    source.configure(&mut document);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    for started in [100, 200] {
        let mut value = event(&binding, "tool.after", "allow");
        value["session_id"] = Value::Null;
        value["tool"]["call_id"] = Value::Null;
        value["native"] = json!({"pid":42,"name":"qoder","host":"same-host","boot":"same-boot","start_ticks":started});
        let handle = open(&service.client, &binding, value);
        let context = snapshot(&service.client, &handle);
        assert_eq!(
            context["evidence"][0]["association"]["status"],
            "unassociated"
        );
        assert!(context["execution_refs"].as_array().unwrap().is_empty());
        assert!(gap(&context, "process_identity_unavailable"));
        close(&service.client, &handle);
    }
    // Normalized IDs without original Adapter bytes cannot establish the mapping.
    let handle = open(
        &service.client,
        &binding,
        native_event(&binding, "tool.after"),
    );
    assert_eq!(
        snapshot(&service.client, &handle)["evidence"][0]["association"]["status"],
        "unverified"
    );
    close(&service.client, &handle);
    let mut forged = native_event(&binding, "tool.before");
    forged["session_id"] = json!("forged-session");
    let handle = hook(&service.client, &binding, forged);
    assert!(gap(
        &snapshot(&service.client, &handle),
        "native_identity_unverified"
    ));
    close(&service.client, &handle);
    let mut missing = native_event(&binding, "tool.before");
    missing["session_id"] = Value::Null;
    assert!(service
        .client
        .call(
            Operation::OpenHookEvent {
                instance_id: binding.instance_id.clone(),
                native_input: serde_json::to_vec(&missing["native"]).unwrap(),
                event: missing,
            },
            Instant::now() + TIMEOUT
        )
        .is_err());
    assert!(source.paths.lock().unwrap().is_empty());
}

#[test]
fn unavailable_or_mismatched_history_remains_distinct_from_an_empty_source() {
    for scenario in ["empty", "missing", "adapter", "session"] {
        let fixture = Fixture::new();
        let mut history = trajectory(0);
        if scenario == "adapter" {
            history["agent"]["name"] = json!("claude-code");
        }
        if scenario == "session" {
            history["session_id"] = json!("another-session");
        }
        let source = Source::new(&fixture, history);
        if scenario == "missing" {
            *source.status.lock().unwrap() = 404;
        }
        let mut document = fixture.document();
        source.configure(&mut document);
        let service = Running::start(&fixture, &document);
        let binding = fixture.bind(&service.client);
        let handle = hook(
            &service.client,
            &binding,
            native_event(&binding, "tool.before"),
        );
        let context = snapshot(&service.client, &handle);
        let (status, reason) = match scenario {
            "empty" => ("available", "coverage_unknown"),
            "missing" => ("unknown", "record_or_store_unavailable"),
            "adapter" => ("unknown", "source_adapter_mismatch"),
            "session" => ("unavailable", "source_invalid"),
            _ => unreachable!(),
        };
        assert_eq!(context["source_status"], status);
        assert!(gap(&context, reason));
        assert_eq!(context["evidence"].as_array().unwrap().len(), 1);
        assert!(context["execution_refs"].as_array().unwrap().is_empty());
        close(&service.client, &handle);
    }
}

#[test]
fn restart_and_configuration_changes_do_not_reanimate_a_frozen_context() {
    let fixture = Fixture::new();
    let document = fixture.document();
    let mut service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = hook(
        &service.client,
        &binding,
        native_event(&binding, "tool.after"),
    );
    let context = snapshot(&service.client, &handle);
    assert_eq!(context["source_status"], "not_configured");
    service.finish().unwrap();
    let old_client = service.client.clone();
    let mut changed = document.clone();
    changed["metadata"]["name"] = json!("changed-configuration");
    let service = Running::start(&fixture, &changed);
    assert!(old_client
        .call(freeze(&handle), Instant::now() + TIMEOUT)
        .is_err());
    assert!(service
        .client
        .call(freeze(&handle), Instant::now() + TIMEOUT)
        .is_err());
    assert_ne!(old_client.identity(), service.client.identity());
    assert_eq!(audit(&service.client, &handle.event_id)["terminal"], true);
}

#[test]
fn invalid_source_configuration_fails_before_starting_the_service() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(0));
    for (field, value) in [
        ("max_items", json!(0)),
        ("max_bytes", json!(2097153)),
        ("query_timeout_ms", json!(60001)),
        ("endpoint", json!("http://remote.invalid")),
    ] {
        let mut document = fixture.document();
        source.configure(&mut document);
        document["spec"]["agents"]["target"]["evidence"][field] = value;
        assert!(
            Server::bind(serde_json::to_vec(&document).unwrap(), fixture.state()).is_err(),
            "{field}"
        );
        assert!(!fixture.state().join("aw.sock").exists());
    }
    assert!(source.paths.lock().unwrap().is_empty());
}
