#![cfg(target_os = "linux")]

use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    path::Path,
    time::{Duration, Instant},
};

#[path = "support/fixture.rs"]
mod support;

fn invocation() -> Value {
    json!({"api_version":VERSION,"method":"invoke","request_id":"skill-1",
        "operation":"check_skill","config_revision":"b".repeat(64),"budget_ms":1000,
        "allowed_effects":["observe","block"],"config":{"version":1,"mode":"block","skill":true},
        "event":{"name":"tool.before","agent":{"adapter":"qwenpaw","binding_id":"target","instance_id":"instance"},
            "session_id":"session","tool":{"name":"Skill","native_name":"Skill","call_id":"call",
                "input":{"skill":"test-skill"},"result":null},
            "native":{"tool_call":{"id":"native-call"},"aw_skill":{"name":"test-skill","directory":"/actual/skill path","source":"toolkit"}}}})
}

fn run(value: Value, cli: &Path) -> Value {
    let protocol = Protocol::new().unwrap();
    let request = protocol.bind_invocation(value).unwrap();
    let mut output = Vec::new();
    aw_provider_sec_core::run(
        &mut Cursor::new(serde_json::to_vec(request.as_value()).unwrap()),
        &mut output,
        cli,
        Path::new("/unused.sock"),
        Duration::from_secs(2),
    )
    .unwrap();
    match protocol.check_response(&request, &output) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => panic!("{error}"),
    }
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn skill_states_preserve_trust_and_explicit_modes() {
    for (status, reason) in [
        ("pass", "skill_pass"),
        ("none", "skill_unsigned"),
        ("drifted", "skill_drifted"),
        ("warn", "skill_warn"),
        ("deny", "skill_deny"),
        ("tampered", "skill_tampered"),
    ] {
        for mode in ["block", "observe"] {
            let fixture =
                support::Directory::new(json!({"stdout":{"status":status,"findings":"PRIVATE"}}));
            let mut request = invocation();
            request["config"]["mode"] = json!(mode);
            let result = run(request, &fixture.cli());
            let effect = if mode == "block" && status != "pass" {
                "block"
            } else {
                "observe"
            };
            assert_eq!(
                result["effects"],
                json!([{"type":effect,"reason_code":reason}])
            );
            assert!(!result.to_string().contains("PRIVATE"));
            let argv = fixture.argv();
            assert_eq!(argv[0], "--trace-context");
            let context: Value = serde_json::from_str(argv[1].as_str().unwrap()).unwrap();
            assert_eq!(context["tool_call_id"], "native-call");
            assert_eq!(argv[2], "--socket");
            assert_eq!(argv[3], "/unused.sock");
            assert_eq!(argv[4], "--timeout-ms");
            assert_eq!(argv[6], "skill-ledger");
            assert_eq!(argv[7], "check");
            assert_eq!(argv[8], "/actual/skill path");
            assert_eq!(argv.as_array().unwrap().len(), 9);
            fixture.assert_stopped();
        }
    }
}

#[test]
fn skill_checks_coexist_with_submission_configuration() {
    let fixture = support::Directory::new(json!({"stdout":{"status":"pass"}}));
    let mut request = invocation();
    request["config"]["prompt"] = json!({"mode":"block","scan_mode":"fast"});
    request["config"]["pii"] = json!({"input_mode":"block","output_mode":"observe"});
    assert_eq!(
        run(request, &fixture.cli())["effects"],
        json!([{"type":"observe","reason_code":"skill_pass"}])
    );
    fixture.assert_stopped();
}

#[test]
fn negative_trust_exit_one_is_a_completed_check() {
    for status in ["none", "drifted", "warn", "deny", "tampered"] {
        let fixture = support::Directory::new(json!({"exit":1,"stdout":{"status":status}}));
        assert_eq!(
            run(invocation(), &fixture.cli())["effects"][0]["type"],
            "block"
        );
    }
}

#[test]
fn skill_faults_are_errors_without_success_effects() {
    for (case, code) in [
        (json!({"raw":""}), "invalid_skill_result"),
        (json!({"raw":"not JSON"}), "invalid_skill_result"),
        (
            json!({"raw":"{\"status\":\"pass\",\"status\":\"none\"}"}),
            "invalid_skill_result",
        ),
        (
            json!({"stdout":{"status":"unknown"}}),
            "invalid_skill_result",
        ),
        (json!({"stdout":{"status":"error"}}), "skill_check_error"),
        (
            json!({"exit":1,"stdout":{"status":"pass"}}),
            "invalid_skill_result",
        ),
        (json!({"exit":2,"stdout":{"status":"none"}}), "cli_failed"),
        (
            json!({"exit":1,"stderr":"PermissionDenied PRIVATE"}),
            "invalid_skill_result",
        ),
        (json!({"stdout_repeat":1048577}), "cli_output_limit"),
    ] {
        let fixture = support::Directory::new(case);
        let reply = run(invocation(), &fixture.cli());
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["error_code"], code);
        assert!(reply.get("effects").is_none());
        assert!(!reply.to_string().contains("PRIVATE"));
    }
    let fixture = support::Directory::new(json!({"sleep":2}));
    let mut request = invocation();
    request["budget_ms"] = json!(100);
    let started = Instant::now();
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "deadline_exceeded"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    fixture.assert_stopped();
    assert_eq!(
        run(invocation(), Path::new("/absent-cli"))["error_code"],
        "cli_transport_error"
    );
}

#[test]
fn missing_or_untrustworthy_resolution_does_not_call_backend() {
    let fixture = support::Directory::new(json!({"stdout":{"status":"pass"}}));
    for resolved in [
        json!(null),
        json!({"error_code":"skill_resolution_error"}),
        json!({"name":"test-skill","source":"toolkit","directory":"relative"}),
        json!({"name":"test-skill","source":"toolkit","directory":"/wrong/../skill"}),
        json!({"name":"test-skill","source":"toolkit","directory":"/"}),
        json!({"name":"wrong-skill","source":"toolkit","directory":"/skill"}),
    ] {
        let mut request = invocation();
        request["event"]["native"]["aw_skill"] = resolved;
        assert_eq!(
            run(request, &fixture.cli())["error_code"],
            "skill_resolution_error"
        );
    }
    let mut request = invocation();
    request["allowed_effects"] = json!(["observe"]);
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "block_not_admitted"
    );
    let mut request = invocation();
    request["config"]["skill"] = json!(false);
    request["config"]["tools"] = json!({"bash":{"language":"bash","input_pointer":"/command"}});
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "skill_check_disabled"
    );
    assert!(!fixture.0.join("cli.pid").exists());
}
