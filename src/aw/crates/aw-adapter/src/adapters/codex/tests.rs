//! Native field validation needs only launch identity, never a service binding.

use super::*;

#[test]
fn normalizes_native_identity_and_preserves_failed_tool_results() {
    let context = NativeContext {
        target: "coding".into(),
        instance_id: "instance".into(),
        cwd: "/workspace".into(),
    };
    let payload = json!({
        "hook_event_name":"PostToolUse", "cwd":"/workspace",
        "session_id":"session", "tool_use_id":"call", "tool_name":"Bash",
        "tool_input":{"command":"false"},
        "tool_response":{"stdout":"", "stderr":"failure", "exit_code":1}
    });
    let result = Codex.normalize(&context, "tool.after", &payload).unwrap();
    assert_eq!(
        result["agent"],
        json!({"adapter":"codex", "binding_id":"coding", "instance_id":"instance"})
    );
    assert_eq!(result["session_id"], "session");
    assert_eq!(result["tool"]["call_id"], "call");
    assert_eq!(result["tool"]["result"], payload["tool_response"]);
    assert_eq!(result["native"], payload);

    for (key, value) in [
        ("hook_event_name", json!("PreToolUse")),
        ("cwd", json!("/other")),
        ("session_id", json!("")),
        ("tool_use_id", json!(" ")),
        ("tool_name", json!("x".repeat(513))),
        ("tool_input", json!("false")),
    ] {
        let mut invalid = payload.clone();
        invalid[key] = value;
        assert!(
            Codex.normalize(&context, "tool.after", &invalid).is_err(),
            "{key}"
        );
    }
    let mut missing = payload.clone();
    missing.as_object_mut().unwrap().remove("tool_response");
    assert!(Codex.normalize(&context, "tool.after", &missing).is_err());
    assert!(Codex.normalize(&context, "unsupported", &payload).is_err());
}
