//! Input identity is independent of the managed tool invocation ledger.

use super::tests::{deadline, Fixture};
use super::*;
use aw_host::{Host, ProcessContext};
use aw_provider::admission::AdapterCapabilities;
use std::collections::BTreeMap;

fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let mut document = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!(
            "../../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap()
        .as_value()
        .clone();
    document["spec"]["providers"]["submit"] = json!({
        "transport":{"type":"stdio","location":"agent", "argv":["/usr/bin/python3","-c","import sys; sys.stdin.read(); print('false')"]},
        "timeout_ms":1000,"max_output_bytes":32});
    document["spec"]["events"]["input.submit"] = json!({"enabled":true,"steps":[
        {"id":"submit","provider":"submit","effects":["block"],"on_error":"block"}]});
    let instances = fixture.runtime.instances.lock().unwrap();
    let existing = instances[&fixture.id].as_ref().unwrap();
    let mut capabilities: AdapterCapabilities = existing.host.capabilities().clone();
    capabilities.events.insert(
        "input.submit".into(),
        vec!["observe".into(), "block".into()],
    );
    let context = ProcessContext {
        cwd: existing.context.cwd.clone(),
        environment: BTreeMap::new(),
        stderr_bytes: 1024,
    };
    drop(instances);
    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "qoder",
        capabilities,
        context,
        deadline(),
        &AtomicBool::new(false),
    )
    .unwrap();
    fixture
        .runtime
        .instances
        .lock()
        .unwrap()
        .get_mut(&fixture.id)
        .unwrap()
        .as_mut()
        .unwrap()
        .host = Arc::new(host);
    fixture
}

fn event(fixture: &Fixture, session: &str, turn: &str) -> Value {
    json!({"name":"input.submit","agent":{"adapter":"qoder","binding_id":"qoder","instance_id":fixture.id},
        "session_id":session,"turn_id":turn,"input":{"text":"fresh input"},"native":{}})
}

#[test]
fn submission_correlates_turns_without_registering_tools_and_cancels_replays() {
    let fixture = fixture();
    fixture.runtime.enable_invocations(&fixture.id).unwrap();
    let value = event(&fixture, "session", "turn");
    let first = fixture
        .runtime
        .open_hook(
            fixture.id.clone(),
            value.clone(),
            b"first".to_vec(),
            deadline(),
        )
        .unwrap();
    let duplicate = fixture
        .runtime
        .open_hook(
            fixture.id.clone(),
            value.clone(),
            b"first".to_vec(),
            deadline(),
        )
        .unwrap();
    assert_eq!(first, duplicate);
    let mut changed = value.clone();
    changed["input"]["text"] = json!("changed");
    assert!(fixture
        .runtime
        .open_hook(fixture.id.clone(), changed, b"first".to_vec(), deadline())
        .is_err());
    let other = fixture
        .runtime
        .open_hook(
            fixture.id.clone(),
            event(&fixture, "session", "turn-2"),
            b"second".to_vec(),
            deadline(),
        )
        .unwrap();
    assert_ne!(first["event_id"], other["event_id"]);
    fixture
        .runtime
        .handle(
            crate::Operation::CloseInputEvent {
                instance_id: fixture.id.clone(),
                session_id: "session".into(),
                turn_id: "turn".into(),
            },
            deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(fixture
        .runtime
        .open_hook(fixture.id.clone(), value, b"first".to_vec(), deadline())
        .is_err());
    // Closing a turn in another session does not cancel the active session's turn.
    fixture
        .runtime
        .close_hook(&fixture.id, "input.submit", "other", "turn-2", deadline())
        .unwrap();
    assert!(fixture
        .runtime
        .event_sender(other["event_id"].as_str().unwrap(), &fixture.id)
        .is_ok());
    fixture
        .runtime
        .close_hook(&fixture.id, "input.submit", "session", "turn-2", deadline())
        .unwrap();
    fixture
        .runtime
        .register_invocation(&fixture.id, 1, "tool-call", "Bash", json!({}))
        .unwrap();
    fixture
        .runtime
        .finish_invocation(&fixture.id, 1, "tool-call", deadline())
        .unwrap();
    fixture
        .runtime
        .release_instance(&fixture.id, deadline())
        .unwrap();
    assert!(fixture.runtime.events.lock().unwrap().is_empty());
}

#[test]
fn submission_close_claims_are_bounded_with_a_managed_tool_ledger() {
    let fixture = fixture();
    fixture.runtime.enable_invocations(&fixture.id).unwrap();
    let hooks = Arc::clone(
        &fixture.runtime.instances.lock().unwrap()[&fixture.id]
            .as_ref()
            .unwrap()
            .hooks,
    );
    for index in 0..MAX_HOOK_EVENTS {
        hooks.lock().unwrap().insert(
            index.to_string(),
            Lease {
                digest: [0; 32],
                handle: None,
                completion_digest: None,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
        );
    }
    assert!(
        matches!(fixture.runtime.close_hook(&fixture.id,"input.submit","session","overflow",deadline()),
        Err(crate::Error::Rejected(code)) if code == "native_event_limit")
    );
    assert_eq!(hooks.lock().unwrap().len(), MAX_HOOK_EVENTS);
    fixture
        .runtime
        .release_instance(&fixture.id, deadline())
        .unwrap();
}
