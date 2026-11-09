//! Managed identities remain corroborated against the original native envelope.

use super::*;
use crate::adapter::{select, NativeContext};

#[test]
fn qwenpaw_dispatch_identity_keeps_native_call_and_exact_input() {
    let context = NativeContext {
        target: "target".into(),
        instance_id: "instance".into(),
        cwd: "/tmp".into(),
    };
    for managed in [false, true] {
        for name in ["tool.before", "tool.after"] {
            let mut native = json!({"event":name,"session_id":"session",
                "tool_call":{"id":"native-call","name":"custom",
                    "input":"{\"value\":9007199254740993}"}});
            if name == "tool.after" {
                native["tool_response"] = json!({"content":[],"state":"success"});
            }
            if managed {
                native["aw_invocation_id"] = json!("f930e5bb-810c-470e-b472-58b7b2039865");
            }
            let event = select("qwenpaw")
                .unwrap()
                .normalize(&context, name, &native)
                .unwrap();
            let raw = serde_json::to_vec(&native).unwrap();
            let check = |event: &Value, raw: &[u8]| {
                association("qwenpaw", event, Some(raw), |bytes| {
                    serde_json::from_slice(bytes).ok()
                })["status"]
                    .clone()
            };
            assert_eq!(check(&event, &raw), "verified");
            assert_eq!(event["native"]["tool_call"]["id"], "native-call");
            let mut changed = event.clone();
            changed["tool"]["input"]["value"] = json!(9007199254740992_u64);
            assert_eq!(check(&changed, &raw), "unverified");
            if managed {
                changed = event.clone();
                changed["tool"]["call_id"] = json!("native-call");
                assert_eq!(check(&changed, &raw), "unverified");
                native["aw_invocation_id"] = json!("0e98ee65-87b8-4c90-a8d3-5d564e53908e");
                assert_eq!(
                    check(&event, &serde_json::to_vec(&native).unwrap()),
                    "unverified"
                );
            }
        }
    }
}

#[test]
fn qoder_sdk_references_preserve_original_tool_identity() {
    let context = NativeContext {
        target: "target".into(),
        instance_id: "instance".into(),
        cwd: "/tmp".into(),
    };
    let invocation = "f930e5bb-810c-470e-b472-58b7b2039865";
    let cases = [
        (None, json!(invocation)),
        (Some(json!("native-tool-id")), json!("native-tool-id")),
        (Some(Value::Null), Value::Null),
        (Some(json!("")), Value::Null),
        (Some(json!(" ")), Value::Null),
        (Some(json!("native\ncall")), Value::Null),
        (Some(json!("x".repeat(513))), Value::Null),
        (Some(json!(42)), Value::Null),
    ];
    for name in ["tool.before", "tool.after"] {
        for (original, expected) in &cases {
            let mut native = json!({"session_id":"session","tool_use_id":invocation,"cwd":"/tmp",
                "tool_name":"Bash","tool_input":{"command":"true"},
                "hook_event_name":if name == "tool.before" {"PreToolUse"} else {"PostToolUse"}});
            if let Some(original) = original {
                native["aw_native_tool_use_id"] = original.clone();
            }
            if name == "tool.after" {
                native["tool_response"] = json!({"stdout":"done","is_error":false});
            }
            let event = select("qoder-sdk")
                .unwrap()
                .normalize(&context, name, &native)
                .unwrap();
            assert_eq!(event["tool"]["call_id"], invocation);
            assert_eq!(
                native_references("qoder-sdk", &event)["toolCallId"],
                *expected,
                "{name}: {original:?}"
            );
            for adapter in ["qoder", "claude-sdk"] {
                assert_eq!(native_references(adapter, &event)["toolCallId"], invocation);
            }
        }
    }
}
