//! Event-only checks reuse the invocation schema without consuming envelope quotas.

use aw_provider::{Protocol, MAX_DEPTH, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};

fn event() -> Value {
    json!({"name":"tool.before", "agent":{"adapter":"qoder", "binding_id":"target",
        "instance_id":null}, "session_id":null,
        "tool":{"name":"bash", "native_name":"Bash", "call_id":null,
        "input":{"command":"echo hello"}, "result":null}, "native":{}})
}

#[test]
fn standalone_event_checks_all_fields_and_before_result_semantics() {
    let protocol = Protocol::new().unwrap();
    let original = event();
    protocol.validate_event(&original).unwrap();
    for pointer in [
        "/name",
        "/agent/adapter",
        "/agent/binding_id",
        "/agent/instance_id",
        "/session_id",
        "/tool/name",
        "/tool/native_name",
        "/tool/call_id",
        "/native",
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = json!(42);
        assert!(protocol.validate_event(&value).is_err(), "{pointer}");
    }
    for pointer in ["/agent", "/tool", ""] {
        let mut value = original.clone();
        value.pointer_mut(pointer).unwrap()["unknown"] = json!(true);
        assert!(protocol.validate_event(&value).is_err(), "{pointer}");
    }
    let mut value = original.clone();
    value["tool"].as_object_mut().unwrap().remove("native_name");
    assert!(protocol.validate_event(&value).is_err());
    value = original.clone();
    value["tool"]["result"] = json!({"premature":true});
    assert!(protocol.validate_event(&value).is_err());
    value["name"] = json!("tool.after");
    protocol.validate_event(&value).unwrap();
    value = original;
    value["tool"]["result_projection"] = json!({"text":"result", "status":"success",
        "content_origin":"command_output"});
    assert!(protocol.validate_event(&value).is_err());
}

#[test]
fn standalone_event_preserves_bare_size_and_depth_boundaries() {
    let protocol = Protocol::new().unwrap();
    let mut value = event();
    value["tool"]["input"] = json!("");
    let overhead = serde_json::to_vec(&value).unwrap().len();
    value["tool"]["input"] = json!("I".repeat(MAX_MESSAGE_BYTES - overhead));
    protocol.validate_event(&value).unwrap();
    value["tool"]["input"] = json!("I".repeat(MAX_MESSAGE_BYTES - overhead + 1));
    assert!(protocol.validate_event(&value).is_err());
    let mut nested = json!(true);
    for _ in 0..MAX_DEPTH - 2 {
        nested = json!([nested]);
    }
    value["tool"]["input"] = nested;
    protocol.validate_event(&value).unwrap();
    value["tool"]["input"] = json!([value["tool"]["input"].clone()]);
    assert!(protocol.validate_event(&value).is_err());
}
