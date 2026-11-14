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

#[test]
fn submission_requires_bounded_turn_and_text() {
    let context = NativeContext {
        target: "coding".into(),
        instance_id: "instance".into(),
        cwd: "/workspace".into(),
    };
    let payload = json!({"hook_event_name":"UserPromptSubmit", "cwd":"/workspace",
        "session_id":"session", "turn_id":"turn", "prompt":"check this"});
    let normalized = Codex.normalize(&context, "input.submit", &payload).unwrap();
    assert_eq!(normalized["input"]["text"], "check this");
    assert_eq!(normalized["turn_id"], "turn");
    for (key, value) in [
        ("turn_id", json!(" ")),
        ("session_id", json!("x".repeat(513))),
        ("prompt", json!({"text":"check this"})),
        ("cwd", json!("/other")),
    ] {
        let mut invalid = payload.clone();
        invalid[key] = value;
        assert!(
            Codex.normalize(&context, "input.submit", &invalid).is_err(),
            "{key}"
        );
    }
}

#[test]
fn launch_entrypoints_cannot_dispatch_bare_prompts_as_hidden_commands() {
    for command in [
        "responses-api-proxy",
        "migrate-rollouts",
        "e",
        "a",
        "resume",
        "future-command",
    ] {
        for prefix in [vec![], vec!["exec"]] {
            let mut args = prefix.clone();
            args.push(command);
            assert!(
                preflight::arguments(&args.iter().map(Into::into).collect::<Vec<_>>()).is_err()
            );
            let mut literal = prefix;
            literal.extend(["--", command]);
            assert!(
                preflight::arguments(&literal.iter().map(Into::into).collect::<Vec<_>>()).is_ok()
            );
        }
    }
    for args in [
        vec![],
        vec!["--model", "responses-api-proxy"],
        vec!["exec", "--ephemeral", "--", "a prompt"],
    ] {
        assert!(preflight::arguments(&args.iter().map(Into::into).collect::<Vec<_>>()).is_ok());
    }
    assert!(preflight::arguments(&["--".into(), "one".into(), "two".into()]).is_err());
    let literal =
        preflight::arguments(&["--".into(), "--dangerously-bypass-hook-trust".into()]).unwrap();
    assert!(!literal.bypass_hook_trust);
}
