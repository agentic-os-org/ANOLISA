//! Exercise source labels and credentials through real HTTP and service boundaries.

use super::{common, support};
use aw_contracts::{canonical, evidence::validate_context, Registry};
use aw_service::{Binding, Operation, Server};
use common::{close, Fixture, Running, TIMEOUT};
use serde_json::{json, Value};
use std::time::Instant;

fn event(binding: &Binding, adapter: &str) -> Value {
    let mut event = support::native_event(binding, "tool.after");
    event["agent"]["adapter"] = json!(adapter);
    let tool = &event["tool"];
    event["native"] = match adapter {
        "qoder" => event["native"].clone(),
        "qwenpaw" => json!({"event":"tool.after","session_id":event["session_id"],
            "tool_call":{"id":tool["call_id"],"name":tool["name"],"input":tool["input"].to_string()},
            "tool_response":tool["result"]}),
        "hermes" => json!({"hook_event_name":"post_tool_call","session_id":event["session_id"],
            "tool_name":tool["name"],"tool_input":tool["input"],
            "extra":{"api_request_id":"request","tool_call_id":"native-call","result":tool["result"]}}),
        "openclaw" => json!({"hook":"after_tool_call","context":{"sessionId":event["session_id"]},
            "event":{"toolName":tool["name"],"toolCallId":"native-call","runId":"run",
            "params":tool["input"],"result":tool["result"]}}),
        _ => unreachable!(),
    };
    if adapter == "hermes" {
        event["tool"]["call_id"] = json!(format!(
            "hermes:{}",
            canonical::digest(json!(["request", "native-call"]).to_string().as_bytes())
        ));
    } else if adapter == "openclaw" {
        event["tool"]["call_id"] = json!(canonical::digest(
            json!(["run", "native-call"]).to_string().as_bytes()
        ));
    }
    event
}

#[test]
fn producer_names_match_only_the_explicit_adapter_contract() {
    // Qoder local discovery emits Qoder; the system collector emits qoder.
    // Hermes/OpenClaw use display names in AgentSight's default configuration.
    for (adapter, name, accepted) in [
        ("qoder", "Qoder", true),
        ("qoder", "qoder", true),
        ("hermes", "Hermes", true),
        ("hermes", "hermes", true),
        ("openclaw", "OpenClaw", true),
        ("openclaw", "openclaw", true),
        ("qwenpaw", "qwenpaw", true),
        ("qoder", "QoderWork", false),
        ("qoder", "Hermes", false),
        ("hermes", "unknown", false),
        ("qwenpaw", "QwenCode", false),
        ("qwenpaw", "AgentScope", false),
    ] {
        let fixture = Fixture::new();
        let mut history = support::trajectory(2);
        history["agent"]["name"] = json!(name);
        let source = support::Source::new(&fixture, history);
        let mut document = fixture.document();
        source.configure(&mut document);
        document["spec"]["agents"]["target"]["adapter"] = json!(adapter);
        let service = Running::start(&fixture, &document);
        let mut bind = fixture.bind_operation();
        if let Operation::Bind { capabilities, .. } = &mut bind {
            capabilities.adapter = adapter.into();
        }
        let binding: Binding =
            serde_json::from_value(service.client.call(bind, Instant::now() + TIMEOUT).unwrap())
                .unwrap();
        let handle = support::hook(&service.client, &binding, event(&binding, adapter));
        let context = support::snapshot(&service.client, &handle);
        assert_eq!(context["evidence"][0]["association"]["status"], "verified");
        assert_eq!(
            context["source_status"],
            if accepted { "available" } else { "unknown" },
            "{adapter}: {name}"
        );
        assert_eq!(
            context["evidence"].as_array().unwrap().len(),
            if accepted { 3 } else { 1 }
        );
        assert_eq!(support::gap(&context, "source_adapter_mismatch"), !accepted);
        for entry in &context["evidence"].as_array().unwrap()[1..] {
            assert_eq!(entry["source"]["version"], Value::Null);
            assert_eq!(entry["source"]["instance_id"], Value::Null);
            assert_eq!(entry["trust"], "source_reported");
            assert_eq!(entry["execution_ref"], Value::Null);
        }
        validate_context(
            &Registry::new().unwrap(),
            &context,
            &context["subject"],
            &handle.candidate_revision,
        )
        .unwrap();
        close(&service.client, &handle);
    }
}

#[test]
fn https_requires_a_token_path_but_loopback_never_opens_one() {
    let fixture = Fixture::new();
    let source = support::Source::new(&fixture, support::trajectory(1));
    for path in [None, Some("relative-token")] {
        let mut document = fixture.document();
        source.configure(&mut document);
        let evidence = &mut document["spec"]["agents"]["target"]["evidence"];
        evidence["endpoint"] = json!("https://agentsight.invalid");
        if let Some(path) = path {
            evidence["token_file"] = json!(path);
        }
        assert!(Server::bind(serde_json::to_vec(&document).unwrap(), fixture.state()).is_err());
        assert!(!fixture.state().join("aw.sock").exists());
    }
    let mut document = fixture.document();
    source.configure(&mut document);
    // An existing directory is deliberately not a credential file. Old configs
    // may still supply the field; loopback must neither open it nor send a token.
    document["spec"]["agents"]["target"]["evidence"]["token_file"] = json!(fixture.0);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = support::hook(
        &service.client,
        &binding,
        support::native_event(&binding, "tool.before"),
    );
    let context = support::snapshot(&service.client, &handle);
    assert_eq!(context["source_status"], "available");
    assert_eq!(
        source.paths.lock().unwrap().as_slice(),
        ["/api/auth/status", "/api/trajectories/native-session"]
    );
    close(&service.client, &handle);
}
