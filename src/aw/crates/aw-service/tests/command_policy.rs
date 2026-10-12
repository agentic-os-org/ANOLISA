//! Boolean commands exercise the real Provider bridge without native Agents or sec-core.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

#[allow(dead_code)]
#[path = "../../aw-exec/tests/support/fixture.rs"]
mod execution_fixture;

use aw_exec::{CommandSpec, Limits};
use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use support::Fixture;

#[test]
fn host_cancellation_and_outer_deadlines_stop_nested_policy_descendants() {
    for (mode, provider_timeout, event_timeout) in [
        ("cancel", 5000, 5000),
        ("provider deadline", 1000, 5000),
        ("event deadline", 5000, 1000),
    ] {
        let fixture = Fixture::new();
        let directory = execution_fixture::Directory::new();
        let mut private = config("");
        private["argv"] = json!([
            fixture.python,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../aw-exec/tests/fixtures/child.py"),
            directory.0,
            "descendant",
            "closed",
            "wait"
        ]);
        private["timeout_ms"] = json!(10000);
        let mut document = fixture.document();
        document["spec"]["providers"] = json!({"policy":{
            "protocol":"aw-provider/v1alpha1",
            "transport":{"type":"stdio","location":"agent","argv":[env!("CARGO_BIN_EXE_aw"),"policy"]},
            "timeout_ms":provider_timeout,"max_output_bytes":4096,"config":private}});
        document["spec"]["events"] = json!({"tool.before":{"enabled":true,"required":true,
            "steps":[{"id":"check","provider":"policy","operation":"check","effects":["block"],"on_error":"block"}]}});
        let cancelled = AtomicBool::new(false);
        let host = aw_host::Host::prepare(
            &serde_json::to_vec(&document).unwrap(),
            "qoder",
            aw_provider::admission::AdapterCapabilities {
                adapter: "qoder".into(),
                version: "1.1.64".into(),
                entrypoint: "interactive".into(),
                events: BTreeMap::from([("tool.before".into(), vec!["block".into()])]),
            },
            aw_host::ProcessContext {
                cwd: directory.0.clone(),
                environment: BTreeMap::from([("LC_ALL".into(), "C".into())]),
                stderr_bytes: 65536,
            },
            Instant::now() + Duration::from_secs(5),
            &cancelled,
        )
        .unwrap();
        let mut value = invoke(private)["event"].clone();
        value["agent"]["binding_id"] = json!("qoder");
        let event = host
            .event(
                value,
                Instant::now() + Duration::from_millis(event_timeout),
                &cancelled,
            )
            .unwrap();
        let started = Instant::now();
        let (ready, invocation) = std::thread::scope(|scope| {
            let call = scope.spawn(|| event.invoke("check").unwrap());
            let ready = execution_fixture::wait_until_exists(
                &directory.0.join("descendant.pid"),
                Instant::now() + Duration::from_secs(3),
            );
            if mode == "cancel" {
                cancelled.store(true, Ordering::Release);
            }
            (ready, call.join().unwrap())
        });
        assert!(ready, "{mode}: script never became ready");
        assert!(invocation.result.is_err(), "{mode}");
        assert_eq!(
            invocation.failure_action,
            Some(aw_host::FailureAction::Block)
        );
        assert!(started.elapsed() < Duration::from_secs(4), "{mode}");
        assert!(
            !directory.process("leader").is_live(),
            "{mode}: script survived"
        );
        assert!(
            !directory.process("descendant").is_live(),
            "{mode}: descendant survived"
        );
    }
}

fn config(script: &str) -> Value {
    json!({"version":1,"argv":["/bin/sh","-c",script],"timeout_ms":1000,
        "on_true":{"type":"block","reason_code":"rule_matched"}})
}

fn invoke(config: Value) -> Value {
    json!({"api_version":VERSION,"method":"invoke","request_id":"invoke-1",
        "operation":"check","config_revision":"b".repeat(64),"budget_ms":2500,
        "allowed_effects":["block"],"config":config,
        "event":{"name":"tool.before","agent":{"adapter":"qoder","binding_id":"target","instance_id":"instance"},
        "session_id":"session","tool":{"name":"custom/tool","native_name":"custom/tool","call_id":"call",
        "input":{"nested":["12345",{"value":"literal ; $HOME"}]},"result":null},"native":{}}})
}

fn run(fixture: &Fixture, request: Value) -> Value {
    let protocol = Protocol::new().unwrap();
    let request = if request["method"] == "invoke" {
        protocol.bind_invocation(request).unwrap()
    } else {
        protocol
            .parse_request(&serde_json::to_vec(&request).unwrap())
            .unwrap()
    };
    let command = CommandSpec {
        program: env!("CARGO_BIN_EXE_aw").into(),
        args: vec!["policy".into()],
        cwd: fixture.root.clone(),
        environment: std::env::vars_os().collect(),
    };
    let output = aw_exec::run(
        &command,
        &serde_json::to_vec(request.as_value()).unwrap(),
        Limits {
            input_bytes: 1024 * 1024,
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        },
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    match protocol.check_response(&request, &output.stdout) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => panic!("invalid bridge reply: {error}"),
    }
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["request_id"], request.as_value()["request_id"]);
    reply
}

#[test]
fn bridge_owns_handshake_and_never_runs_a_command_during_admission() {
    let fixture = Fixture::new();
    let private = config("touch MUST_NOT_RUN; printf true");
    let description = run(
        &fixture,
        json!({"api_version":VERSION,"method":"describe","request_id":"description"}),
    );
    assert_eq!(description["operations"][0]["name"], "check");
    let validation = run(
        &fixture,
        json!({"api_version":VERSION,"method":"validate_config","request_id":"validation","config":private}),
    );
    assert_eq!(validation["status"], "ok");
    assert!(!fixture.root.join("MUST_NOT_RUN").exists());
}

#[test]
fn only_event_reaches_the_script_and_configuration_owns_effects() {
    let fixture = Fixture::new();
    for (matched, effect) in [(false, "block"), (true, "block"), (true, "observe")] {
        let mut request = invoke(config(&format!("cat > event.json; printf {matched}")));
        request["config"]["on_true"]["type"] = json!(effect);
        request["allowed_effects"] = json!([effect]);
        let reply = run(&fixture, request.clone());
        assert_eq!(reply["status"], "ok");
        assert_eq!(
            fs::read(fixture.root.join("event.json")).unwrap(),
            serde_json::to_vec(&request["event"]).unwrap()
        );
        assert_eq!(
            reply["effects"],
            if matched {
                json!([{"type":effect,"reason_code":"rule_matched"}])
            } else {
                json!([])
            }
        );
        assert!(reply["input_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
    }
}

#[test]
fn incomplete_event_delivery_rejects_boolean_results() {
    let fixture = Fixture::new();
    for matched in [false, true] {
        for complete in [false, true] {
            let script = if complete {
                format!("cat >/dev/null; printf {matched}")
            } else {
                format!("exec 0<&-; printf {matched}")
            };
            let mut request = invoke(config(&script));
            request["event"]["tool"]["input"] = json!({"payload":"I".repeat(512 * 1024)});
            let reply = run(&fixture, request);
            if complete {
                assert_eq!(reply["status"], "ok");
                assert_eq!(
                    reply["effects"].as_array().unwrap().len(),
                    usize::from(matched)
                );
            } else {
                assert_eq!(reply["status"], "error", "{matched}");
                assert_eq!(reply["error_code"], "command_incomplete_input");
                assert!(reply.get("effects").is_none());
            }
        }
    }
}

#[test]
fn failures_are_provider_errors_and_do_not_expose_command_output() {
    let fixture = Fixture::new();
    for script in [
        "cat >/dev/null; printf null",
        "cat >/dev/null; printf '\"true\"'",
        "cat >/dev/null; printf true; exit 7",
        "cat >/dev/null; printf 'PRIVATE_INVALID_OUTPUT'",
        "cat >/dev/null; printf '%040d' 0",
    ] {
        let reply = run(&fixture, invoke(config(script)));
        assert_eq!(reply["status"], "error", "{script}");
        assert!(reply.get("effects").is_none());
        assert!(!reply.to_string().contains("PRIVATE_INVALID_OUTPUT"));
    }
    let mut request = invoke(config("cat >/dev/null; exec sleep 10"));
    request["config"]["timeout_ms"] = json!(50);
    let started = Instant::now();
    assert_eq!(run(&fixture, request)["status"], "error");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn invalid_configuration_and_unadmitted_effects_fail_without_running_scripts() {
    let fixture = Fixture::new();
    let original = config("touch MUST_NOT_RUN; printf false");
    let non_executable = fixture.root.join("not executable");
    fs::write(&non_executable, "fixture").unwrap();
    fs::set_permissions(&non_executable, fs::Permissions::from_mode(0o600)).unwrap();
    for (field, value) in [
        ("version", json!(2)),
        ("argv", json!([])),
        ("argv", json!(["relative"])),
        ("argv", json!(["/missing-command"])),
        ("argv", json!([non_executable])),
        ("timeout_ms", json!(0)),
        ("unexpected", json!(true)),
        ("on_true", json!({"type":"ask","reason_code":"invalid"})),
        (
            "on_true",
            json!({"type":"block","reason_code":"private reason"}),
        ),
    ] {
        let mut private = original.clone();
        private[field] = value;
        assert_eq!(
            run(
                &fixture,
                json!({"api_version":VERSION,"method":"validate_config","request_id":"validation","config":private})
            )["status"],
            "error"
        );
    }
    let mut request = invoke(original);
    request["allowed_effects"] = json!(["observe"]);
    assert_eq!(run(&fixture, request)["error_code"], "effect_not_admitted");
    assert!(!fixture.root.join("MUST_NOT_RUN").exists());
}

#[test]
fn literal_arguments_are_not_interpreted_as_shell_commands() {
    let fixture = Fixture::new();
    let mut request = invoke(config(
        "cat >/dev/null; test \"$1\" = '$(touch MUST_NOT_RUN); spaces'; printf true",
    ));
    request["config"]["argv"] = json!([
        "/bin/sh",
        "-c",
        "cat >/dev/null; test \"$1\" = '$(touch MUST_NOT_RUN); spaces' || exit 1; printf true",
        "check",
        "$(touch MUST_NOT_RUN); spaces"
    ]);
    assert_eq!(run(&fixture, request)["status"], "ok");
    assert!(!fixture.root.join("MUST_NOT_RUN").exists());
}
