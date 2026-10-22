//! Idle activation uses real RPCs, Provider preparation and request admission races.

use super::common::{self, Fixture, Running, TIMEOUT};
use aw_service::{Binding, Client, LifecyclePhase, Operation};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Instant,
};

#[path = "policy/preparation.rs"]
mod preparation;

fn call(client: &Client, operation: Operation) -> Result<Value, aw_service::Error> {
    client.call(operation, Instant::now() + TIMEOUT)
}

fn bind(fixture: &Fixture, client: &Client, adapter: &str) -> Binding {
    let mut operation = fixture.bind_operation();
    if let Operation::Bind { capabilities, .. } = &mut operation {
        capabilities.adapter = adapter.into();
        capabilities.version = if adapter == "qoder" {
            "1.1.73"
        } else {
            "2.2.2b4"
        }
        .into();
        capabilities.entrypoint = if adapter == "qoder" { "tui" } else { "app" }.into();
    }
    let binding: Binding = serde_json::from_value(call(client, operation).unwrap()).unwrap();
    call(
        client,
        Operation::EnableReload {
            instance_id: binding.instance_id.clone(),
        },
    )
    .unwrap();
    call(
        client,
        Operation::AssociateProcess {
            instance_id: binding.instance_id.clone(),
            pid: std::process::id(),
        },
    )
    .unwrap();
    lifecycle(client, &binding, LifecyclePhase::Ready, "");
    binding
}

fn lifecycle(client: &Client, binding: &Binding, phase: LifecyclePhase, key: &str) {
    call(
        client,
        Operation::BridgeLifecycle {
            instance_id: binding.instance_id.clone(),
            pid: std::process::id(),
            phase,
            key: key.into(),
        },
    )
    .unwrap();
}

fn update(binding: &Binding, expected: &str, document: &Value) -> Operation {
    Operation::UpdatePolicy {
        instance_id: binding.instance_id.clone(),
        expected_revision: expected.into(),
        config: serde_json::to_string(document).unwrap(),
    }
}

fn revision(client: &Client, binding: &Binding) -> String {
    call(client, Operation::Status).unwrap()["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|instance| instance["instance_id"] == binding.instance_id)
        .unwrap()["policy_revision"]
        .as_str()
        .unwrap()
        .into()
}

fn rejected(result: Result<Value, aw_service::Error>, expected: &str) {
    assert!(
        matches!(result, Err(aw_service::Error::Rejected(ref code) | aw_service::Error::Attempt { ref code, .. }) if code == expected),
        "{result:?}"
    );
}

#[test]
fn maximum_size_policy_with_escaped_comments_activates_exact_bytes() {
    let fixture = Fixture::new();
    let mut source = String::from(concat!(
        "apiVersion: aw/v1alpha1\nkind: AWConfiguration\nmetadata: {name: p}\n",
        "spec:\n  daemon: {startup: external, endpoint: auto, state_dir: auto}\n",
        "  execution: {guarantee: native_hook, default_event_budget_ms: 1000}\n",
        "  audit: {enabled: true, payload: metadata_only}\n",
        "  agents: {target: {adapter: qoder, argv: [/bin/true]}}\n",
        "  providers: {}\n  events: {}\n#"
    ));
    source.push_str(&"\\".repeat(aw_config::MAX_DOCUMENT_BYTES - source.len()));
    let baseline = aw_config::Validator::new()
        .unwrap()
        .parse(source.as_bytes())
        .unwrap();
    let service = Running::start(&fixture, baseline.as_value());
    let binding = bind(&fixture, &service.client, "qoder");
    let operation = Operation::UpdatePolicy {
        instance_id: binding.instance_id.clone(),
        expected_revision: revision(&service.client, &binding),
        config: source.clone(),
    };
    let request = aw_service::Request {
        api_version: aw_service::VERSION.into(),
        identity: Some(binding.identity.clone()),
        deadline_ns: u64::MAX,
        operation: operation.clone(),
    };
    assert!(serde_json::to_vec(&request).unwrap().len() > 8 * 1024 * 1024);
    let result = call(&service.client, operation).unwrap();
    let expected = format!("{:x}", Sha256::digest(source.as_bytes()));
    assert_eq!(result["policy_revision"], expected);
    assert_eq!(revision(&service.client, &binding), expected);
    assert_eq!(service.client.identity(), &binding.identity);
}

#[test]
fn oversized_update_metadata_is_audited_without_poisoning_the_service() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Running::start(&fixture, &baseline);
    let binding = bind(&fixture, &service.client, "qoder");
    let original = revision(&service.client, &binding);
    for length in [4 * 1024 * 1024, 65] {
        for oversized_instance in [false, true] {
            let mut operation = update(&binding, &original, &baseline);
            if let Operation::UpdatePolicy {
                instance_id,
                expected_revision,
                ..
            } = &mut operation
            {
                if oversized_instance {
                    *instance_id = "x".repeat(length);
                } else {
                    *expected_revision = "x".repeat(length);
                }
            }
            let error = call(&service.client, operation).unwrap_err();
            let aw_service::Error::Attempt { code, audit_key } = error else {
                panic!("missing bounded rejection audit: {error:?}");
            };
            assert_eq!(code, "invalid_update_metadata");
            let audit = common::audit(&service.client, &audit_key);
            assert_eq!(audit["terminal"], true);
            assert!(serde_json::to_vec(&audit).unwrap().len() < 4096);
            let seed = &audit["records"][0]["record"]["plan"];
            if oversized_instance {
                assert!(seed["instance_id"].is_null());
                assert_eq!(seed["expected_revision"], original);
            } else {
                assert_eq!(seed["instance_id"], binding.instance_id);
                assert!(seed["expected_revision"].is_null());
            }
            assert_eq!(seed["caller"]["pid"], std::process::id());
            assert_eq!(revision(&service.client, &binding), original);
            call(&service.client, update(&binding, &original, &baseline)).unwrap();
        }
    }
}

#[test]
fn rejected_updates_retain_metadata_audit_and_caller_identity() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Running::start(&fixture, &baseline);
    let binding = bind(&fixture, &service.client, "qoder");
    let original = revision(&service.client, &binding);
    let mut frozen = baseline.clone();
    frozen["metadata"]["name"] = json!("changed");
    let mut denied = baseline.clone();
    denied["spec"]["providers"]["policy"]["config"]["reject"] = json!(true);
    let mut native = baseline.clone();
    native["spec"]["execution"]["default_event_budget_ms"] = json!(60000);
    let malformed = Operation::UpdatePolicy {
        instance_id: binding.instance_id.clone(),
        expected_revision: original.clone(),
        config: format!("{{{}", common::PRIVATE_MARKER),
    };
    let cases = [
        update(&binding, &original, &baseline),
        update(&binding, "stale-revision", &baseline),
        malformed,
        update(&binding, &original, &frozen),
        update(&binding, &original, &native),
        update(&binding, &original, &denied),
    ];
    lifecycle(
        &service.client,
        &binding,
        LifecyclePhase::RequestStart,
        "busy",
    );
    for (index, operation) in cases.into_iter().enumerate() {
        let Operation::UpdatePolicy { ref config, .. } = operation else {
            unreachable!()
        };
        let digest = format!("{:x}", Sha256::digest(config.as_bytes()));
        let failure = call(&service.client, operation).unwrap_err();
        let aw_service::Error::Attempt { code, audit_key } = failure else {
            panic!("missing audit key: {failure:?}");
        };
        let audit = common::audit(&service.client, &audit_key);
        let seed = &audit["records"][0]["record"]["plan"];
        let terminal = &audit["records"].as_array().unwrap().last().unwrap()["record"];
        assert_eq!(seed["kind"], "policy_update");
        assert_eq!(seed["caller"]["pid"], std::process::id());
        assert!(seed["caller"]["uid"].is_u64());
        assert_eq!(seed["previous_revision"], original);
        assert_eq!(seed["candidate_revision"], digest);
        assert_eq!(terminal["reason"], code);
        assert_eq!(terminal["policy_revision"], original);
        assert_eq!(audit["terminal"], true);
        assert!(!audit.to_string().contains(common::PRIVATE_MARKER));
        if index == 0 {
            assert_eq!(code, "busy");
            assert_eq!(terminal["status"], "busy");
            lifecycle(
                &service.client,
                &binding,
                LifecyclePhase::RequestEnd,
                "busy",
            );
        } else {
            assert_eq!(terminal["status"], "rejected");
        }
    }
    assert_eq!(revision(&service.client, &binding), original);
}

#[test]
fn idle_add_remove_keeps_process_service_and_other_instance_unchanged() {
    for adapter in ["qoder", "qwenpaw"] {
        let fixture = Fixture::new();
        let mut baseline = fixture.document();
        baseline["spec"]["agents"]["target"]["adapter"] = json!(adapter);
        let candidate = baseline.clone();
        baseline["spec"]["providers"] = json!({});
        baseline["spec"]["events"] = json!({});
        let service = Running::start(&fixture, &baseline);
        let binding = bind(&fixture, &service.client, adapter);
        let other = bind(&fixture, &service.client, adapter);
        let original = revision(&service.client, &binding);
        call(
            &service.client,
            Operation::CloseHookEvent {
                instance_id: binding.instance_id.clone(),
                event: "tool.before".into(),
                session_id: "replayed-session".into(),
                call_id: "replayed-call".into(),
            },
        )
        .unwrap();
        let result = call(&service.client, update(&binding, &original, &candidate)).unwrap();
        let active = result["policy_revision"].as_str().unwrap();
        assert_ne!(active, original);
        assert_eq!(revision(&service.client, &other), original);
        assert_eq!(service.client.identity(), &binding.identity);
        lifecycle(
            &service.client,
            &binding,
            LifecyclePhase::RequestStart,
            "request",
        );
        let plan = call(
            &service.client,
            Operation::HookPlan {
                instance_id: binding.instance_id.clone(),
                event: "tool.before".into(),
            },
        )
        .unwrap();
        assert_eq!(plan["policy_revision"], active);
        assert_eq!(plan["plan"]["groups"][0][0]["id"], "check");
        let mut replay = common::event(&binding, "tool.before", "allow");
        replay["agent"]["adapter"] = json!(adapter);
        replay["session_id"] = json!("replayed-session");
        replay["tool"]["call_id"] = json!("replayed-call");
        rejected(
            call(
                &service.client,
                Operation::OpenHookEvent {
                    instance_id: binding.instance_id.clone(),
                    event: replay,
                    native_input: b"{}".to_vec(),
                },
            ),
            "native_event_closed",
        );
        let mut event = common::event(&binding, "tool.before", "allow");
        event["agent"]["adapter"] = json!(adapter);
        let handle = common::open(&service.client, &binding, event);
        call(&service.client, common::invoke(&handle, "check")).unwrap();
        common::close(&service.client, &handle);
        assert_eq!(
            fixture.calls("invoke")[0]["request"]["config_revision"],
            active
        );
        let audit = common::audit(&service.client, result["audit_key"].as_str().unwrap());
        assert!(!audit.to_string().contains(common::PRIVATE_MARKER));
        lifecycle(
            &service.client,
            &binding,
            LifecyclePhase::RequestEnd,
            "request",
        );
        call(&service.client, update(&binding, active, &baseline)).unwrap();
        assert_eq!(revision(&service.client, &binding), original);
        fixture.assert_reaped();
    }
}

#[test]
fn whole_request_and_unfinished_tools_return_busy_without_deferred_activation() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Running::start(&fixture, &baseline);
    let binding = bind(&fixture, &service.client, "qoder");
    let original = revision(&service.client, &binding);
    let mut candidate = baseline.clone();
    candidate["spec"]["execution"]["default_event_budget_ms"] = json!(4000);
    lifecycle(
        &service.client,
        &binding,
        LifecyclePhase::RequestStart,
        "model-wait-no-events",
    );
    rejected(
        call(&service.client, update(&binding, &original, &candidate)),
        "busy",
    );
    lifecycle(
        &service.client,
        &binding,
        LifecyclePhase::ToolStart,
        "long-running-tool",
    );
    lifecycle(
        &service.client,
        &binding,
        LifecyclePhase::RequestEnd,
        "model-wait-no-events",
    );
    rejected(
        call(&service.client, update(&binding, &original, &candidate)),
        "busy",
    );
    assert!(call(
        &service.client,
        Operation::HookPlan {
            instance_id: binding.instance_id.clone(),
            event: "tool.after".into()
        }
    )
    .is_ok());
    lifecycle(
        &service.client,
        &binding,
        LifecyclePhase::ToolEnd,
        "long-running-tool",
    );
    assert_eq!(revision(&service.client, &binding), original);
    call(&service.client, update(&binding, &original, &candidate)).unwrap();
    assert_ne!(revision(&service.client, &binding), original);
}

#[test]
fn failed_preparation_conflict_and_frozen_edits_preserve_policy_and_context() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Running::start(&fixture, &baseline);
    let binding = bind(&fixture, &service.client, "qoder");
    let original = revision(&service.client, &binding);
    let mut candidate = baseline.clone();
    candidate["spec"]["providers"]["policy"]["config"]["reject"] = json!(true);
    assert!(matches!(
        call(&service.client, update(&binding, &original, &candidate)),
        Err(aw_service::Error::Attempt { .. })
    ));
    assert_eq!(revision(&service.client, &binding), original);
    rejected(
        call(
            &service.client,
            update(&binding, "stale-revision", &baseline),
        ),
        "policy_revision_conflict",
    );
    candidate = baseline.clone();
    candidate["spec"]["agents"]["target"]["argv"] = json!(["/changed-agent"]);
    assert!(call(&service.client, update(&binding, &original, &candidate)).is_err());
    assert_eq!(revision(&service.client, &binding), original);
    let before = fixture.calls("describe").len();
    candidate = baseline.clone();
    candidate["spec"]["execution"]["default_event_budget_ms"] = json!(55001);
    rejected(
        call(&service.client, update(&binding, &original, &candidate)),
        "native_policy_unsupported",
    );
    assert_eq!(fixture.calls("describe").len(), before);
    fixture.assert_reaped();
}

#[test]
fn request_arriving_during_preparation_wins_and_parallel_update_is_busy() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Running::start(&fixture, &baseline);
    let binding = bind(&fixture, &service.client, "qoder");
    let original = revision(&service.client, &binding);
    let mut candidate = baseline.clone();
    candidate["spec"]["providers"]["policy"]["config"]["prepare_gate"] = json!(true);
    let client = service.client.clone();
    let operation = update(&binding, &original, &candidate);
    let worker = thread::spawn(move || call(&client, operation));
    fixture.wait_for_calls("validate_config", 2);
    rejected(
        call(&service.client, update(&binding, &original, &baseline)),
        "busy",
    );
    lifecycle(
        &service.client,
        &binding,
        LifecyclePhase::RequestStart,
        "raced-request",
    );
    fs::write(fixture.0.join("prepare-release"), b"release").unwrap();
    let failure = worker.join().unwrap().unwrap_err();
    let aw_service::Error::Attempt { code, audit_key } = failure else {
        panic!("missing audit key: {failure:?}");
    };
    assert_eq!(code, "busy");
    let audit = common::audit(&service.client, &audit_key);
    let seed = &audit["records"][0]["record"]["plan"];
    assert_eq!(seed["instance_id"], binding.instance_id);
    assert_eq!(
        seed["candidate_revision"],
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&candidate).unwrap())
        )
    );
    let terminal = &audit["records"].as_array().unwrap().last().unwrap()["record"];
    assert_eq!(terminal["status"], "busy");
    assert_eq!(terminal["policy_revision"], original);
    let preparation = common::audit(
        &service.client,
        terminal["preparation_audit"].as_str().unwrap(),
    );
    assert_eq!(preparation["terminal"], true);
    let prepared = &preparation["records"].as_array().unwrap().last().unwrap()["record"];
    assert_eq!(prepared["status"], "ok");
    assert_eq!(prepared["calls"][0]["method"], "describe");
    assert_eq!(prepared["calls"][1]["method"], "validate_config");
    lifecycle(
        &service.client,
        &binding,
        LifecyclePhase::RequestEnd,
        "raced-request",
    );
    assert_eq!(revision(&service.client, &binding), original);
    call(&service.client, update(&binding, &original, &candidate)).unwrap();
    fixture.assert_reaped();
}

#[test]
fn disconnected_update_cancels_preparation_without_later_activation() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let mut service = Running::start(&fixture, &baseline);
    let binding = bind(&fixture, &service.client, "qoder");
    let original = revision(&service.client, &binding);
    let mut candidate = baseline.clone();
    candidate["spec"]["providers"]["policy"]["config"]["prepare_gate"] = json!(true);
    let client = service.client.clone();
    let operation = update(&binding, &original, &candidate);
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation = cancelled.clone();
    let worker = thread::spawn(move || {
        client.call_cancellable(operation, Instant::now() + TIMEOUT, &cancellation)
    });
    fixture.wait_for_calls("validate_config", 2);
    cancelled.store(true, Ordering::Release);
    assert!(worker.join().unwrap().is_err());
    service.finish().unwrap();
    fixture.assert_reaped();
    // Cancellation never published the activation audit transition.
    assert!(!fs::read_dir(fixture.state().join("journal"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .any(
            |entry| fs::read_to_string(entry.path()).is_ok_and(|text| text
                .lines()
                .any(|line| serde_json::from_str::<Value>(line)
                    .is_ok_and(|record| record["record"]["status"] == "active")))
        ));
}

#[test]
fn missing_bridge_or_unconfirmed_idle_is_never_treated_as_idle() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Running::start(&fixture, &baseline);
    let binding = fixture.bind(&service.client);
    rejected(
        call(
            &service.client,
            update(&binding, &binding.identity.config_revision, &baseline),
        ),
        "unsupported",
    );
    rejected(
        call(
            &service.client,
            Operation::EnableReload {
                instance_id: binding.instance_id.clone(),
            },
        ),
        "unsupported",
    );
    let mut operation = fixture.bind_operation();
    if let Operation::Bind { capabilities, .. } = &mut operation {
        capabilities.version = "1.1.73".into();
        capabilities.entrypoint = "tui".into();
    }
    let binding: Binding =
        serde_json::from_value(call(&service.client, operation).unwrap()).unwrap();
    call(
        &service.client,
        Operation::EnableReload {
            instance_id: binding.instance_id.clone(),
        },
    )
    .unwrap();
    call(
        &service.client,
        Operation::AssociateProcess {
            instance_id: binding.instance_id.clone(),
            pid: std::process::id(),
        },
    )
    .unwrap();
    rejected(
        call(
            &service.client,
            update(&binding, &binding.identity.config_revision, &baseline),
        ),
        "busy",
    );
    rejected(
        call(
            &service.client,
            Operation::BridgeLifecycle {
                instance_id: binding.instance_id.clone(),
                pid: 1,
                phase: LifecyclePhase::Ready,
                key: "".into(),
            },
        ),
        "bridge_process_mismatch",
    );
    lifecycle(&service.client, &binding, LifecyclePhase::Ready, "");
    call(
        &service.client,
        update(&binding, &binding.identity.config_revision, &baseline),
    )
    .unwrap();
}

#[test]
fn startup_ack_race_waits_for_launcher_process_corroboration() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    let service = Running::start(&fixture, &baseline);
    let mut operation = fixture.bind_operation();
    if let Operation::Bind { capabilities, .. } = &mut operation {
        capabilities.version = "1.1.73".into();
        capabilities.entrypoint = "tui".into();
    }
    let binding: Binding =
        serde_json::from_value(call(&service.client, operation).unwrap()).unwrap();
    call(
        &service.client,
        Operation::EnableReload {
            instance_id: binding.instance_id.clone(),
        },
    )
    .unwrap();
    lifecycle(&service.client, &binding, LifecyclePhase::Ready, "");
    assert_eq!(
        call(&service.client, Operation::Status).unwrap()["instances"][0]["ready"],
        false
    );
    call(
        &service.client,
        Operation::AssociateProcess {
            instance_id: binding.instance_id.clone(),
            pid: std::process::id(),
        },
    )
    .unwrap();
    assert_eq!(
        call(&service.client, Operation::Status).unwrap()["instances"][0]["ready"],
        true
    );
    call(
        &service.client,
        update(&binding, &binding.identity.config_revision, &baseline),
    )
    .unwrap();
}
