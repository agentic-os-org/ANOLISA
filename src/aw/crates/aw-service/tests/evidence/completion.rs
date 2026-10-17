//! Preserve #7411's original candidate across OpenClaw completion views.

use super::{common, support};
use aw_service::{Binding, EventHandle, Operation};
use common::{close, Fixture, Running, TIMEOUT};
use serde_json::{json, Value};
use std::{path::Path, time::Instant};
use support::snapshot;

#[test]
fn openclaw_completion_keeps_the_original_frozen_candidate_and_budget() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["agents"]["target"]["adapter"] = json!("openclaw");
    let provider = &mut document["spec"]["providers"]["policy"];
    let python = provider["transport"]["argv"][0].clone();
    provider["transport"]["argv"] = json!([
        python,
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/action.py"),
        "--root",
        fixture.0,
        "--protocol",
        "provider",
        "--behavior",
        "replace"
    ]);
    document["spec"]["events"] = json!({"tool.after":{"enabled":true,"steps":[{
        "id":"compress","provider":"policy","operation":"record","effects":["replace_result"],"on_error":"report"
    }]}});
    let service = Running::start(&fixture, &document);
    let mut bind = fixture.bind_operation();
    if let Operation::Bind { capabilities, .. } = &mut bind {
        capabilities.adapter = "openclaw".into();
        capabilities.events.insert(
            "tool.after".into(),
            vec!["observe".into(), "replace_result".into()],
        );
    }
    let binding: Binding =
        serde_json::from_value(service.client.call(bind, Instant::now() + TIMEOUT).unwrap())
            .unwrap();
    let native = json!({"hook":"after_tool_call", "event":{"toolName":"exec","toolCallId":"call","runId":"run", "params":{},"result":"original"},"context":{"sessionId":"session"}});
    let original = json!({
        "name":"tool.after", "agent":{"adapter":"openclaw","binding_id":"target","instance_id":binding.instance_id},
        "session_id":"session", "tool":{"name":"exec","native_name":"exec",
            "call_id":aw_contracts::canonical::digest(json!(["run","call"]).to_string().as_bytes()),"input":{},"result":"original"},
        "native":native,
    });
    let lease = |mut value: Value, phase: &str| -> EventHandle {
        let bytes = serde_json::to_vec(&value["native"]).unwrap();
        value["native"]["aw_completion"] =
            json!({"phase":phase,"snapshot":original,"input":native.to_string()});
        serde_json::from_value(
            service
                .client
                .call(
                    Operation::OpenHookEvent {
                        instance_id: binding.instance_id.clone(),
                        event: value,
                        native_input: bytes,
                    },
                    Instant::now() + TIMEOUT,
                )
                .unwrap(),
        )
        .unwrap()
    };
    let first = lease(original.clone(), "replacement");
    let frozen = snapshot(&service.client, &first);
    assert_eq!(frozen["evidence"][0]["association"]["status"], "verified");
    assert_eq!(frozen["execution_refs"].as_array().unwrap().len(), 1);
    let mut completion = original.clone();
    completion["tool"]["result"] = json!("model-visible compact view");
    completion["native"]["event"]["result"] = json!("model-visible compact view");
    completion["native"]["event"]["durationMs"] = json!(25);
    let later = lease(completion, "observation");
    assert_eq!(first.event_id, later.event_id);
    assert_eq!(first.candidate_revision, later.candidate_revision);
    assert_eq!(snapshot(&service.client, &later), frozen);
    let decoded: Value =
        serde_json::from_str(frozen["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(decoded["tool"]["result"], "original");
    close(&service.client, &first);
}
