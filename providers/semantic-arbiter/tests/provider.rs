//! Stdio, mapping and loopback model regression inventory.

#[test]
fn paired_examples_validate_and_have_identical_private_config() {
    let document = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!("../examples/aw.yaml"))
        .unwrap();
    let mut config: Value =
        serde_json::from_slice(include_bytes!("../examples/config.json")).unwrap();
    assert_eq!(
        document.as_value()["spec"]["providers"]["semantic"]["config"],
        config
    );
    let fixture = Fixture::new();
    config["record_dir"] = json!(fixture.records);
    let request = json!({"api_version":"aw-provider/v1alpha1","request_id":"example","method":"validate_config","config":config});
    let output = finish(
        spawn(fixture.root.path(), false),
        request.to_string().as_bytes(),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["status"],
        "ok"
    );
    assert!(fixture.records().is_empty());
}

#[test]
fn public_aw_host_admits_and_invokes_observe_only_provider() {
    use aw_host::{Host, ProcessContext, StepOutput};
    use aw_provider::admission::AdapterCapabilities;
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    for probability in ["0.99", "0.01"] {
        let fixture = Fixture::new();
        let server = Server::new(probability, 200, Duration::ZERO);
        let document = json!({
            "apiVersion":"aw/v1alpha1","kind":"AWConfiguration","metadata":{"name":"shadow-fixture"},
            "spec":{
                "daemon":{"startup":"on_demand","endpoint":"auto","state_dir":"auto"},
                "execution":{"guarantee":"native_hook","default_event_budget_ms":3000},
                "audit":{"enabled":true,"payload":"metadata_only"},
                "agents":{"target":{"adapter":"qoder","argv":["/no-agent-is-started"]}},
                "providers":{"semantic":{"protocol":"aw-provider/v1alpha1",
                    "transport":{"type":"stdio","location":"agent","argv":[BINARY]},
                    "timeout_ms":3000,"max_output_bytes":65536,"config":fixture.config(&server.endpoint)}},
                "events":{"tool.before":{"enabled":true,"steps":[{"id":"shadow","provider":"semantic",
                    "operation":"evaluate_task_scope","effects":["observe"],"on_error":"report"}]}}
            }
        });
        let bytes = serde_json::to_vec(&document).unwrap();
        aw_config::Validator::new().unwrap().parse(&bytes).unwrap();
        let cancelled = AtomicBool::new(false);
        let host = Host::prepare(
            &bytes,
            "target",
            AdapterCapabilities {
                adapter: "qoder".into(),
                version: "fixture".into(),
                entrypoint: "fixture".into(),
                failure_actions: Default::default(),
                events: BTreeMap::from([("tool.before".into(), vec!["observe".into()])]),
            },
            ProcessContext {
                cwd: fixture.root.path().to_path_buf(),
                environment: BTreeMap::from([(KEY_ENV.into(), KEY.into())]),
                stderr_bytes: 4096,
            },
            Instant::now() + Duration::from_secs(5),
            &cancelled,
        )
        .unwrap();
        assert_eq!(host.preparation().len(), 2);
        assert!(fixture.records().is_empty());
        let mut event = fixture.request(fixture.config(&server.endpoint))["event"].clone();
        event["agent"]["binding_id"] = json!("target");
        let selected = host
            .event(event, Instant::now() + Duration::from_secs(3), &cancelled)
            .unwrap();
        let invocation = selected.invoke("shadow").unwrap();
        let StepOutput::Provider(outcome) = invocation.result.unwrap() else {
            panic!("expected Provider output");
        };
        assert!(!outcome.requests_block());
        assert_eq!(outcome.as_value()["effects"], json!([]));
        assert_eq!(
            read(&fixture.records()[0].join("40-result.json"))["status"],
            "evaluated"
        );
        assert!(!fixture.root.path().join("greeting.txt").exists());
    }
}

#[path = "support/fixture.rs"]
mod fixture;

use std::fs;
use std::time::Duration;

use fixture::*;
use serde_json::{json, Value};

#[test]
fn describe_and_offline_closed_config() {
    let fixture = Fixture::new();
    let request =
        json!({"api_version":"aw-provider/v1alpha1","request_id":"describe","method":"describe"});
    let output = finish(
        spawn(fixture.root.path(), false),
        request.to_string().as_bytes(),
    );
    let protocol = aw_provider::Protocol::new().unwrap();
    protocol
        .check_response(
            &protocol
                .parse_request(request.to_string().as_bytes())
                .unwrap(),
            &output.stdout,
        )
        .unwrap();
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        response["operations"],
        json!([{"name":"evaluate_task_scope","events":["tool.before"],"effects":["observe"]}])
    );
    let config = fixture.config("http://127.0.0.1:1/no-network");
    let request = json!({"api_version":"aw-provider/v1alpha1","request_id":"config","method":"validate_config","config":config});
    let output = finish(
        spawn(fixture.root.path(), false),
        request.to_string().as_bytes(),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["status"],
        "ok"
    );
    assert!(fixture.records().is_empty());
}

#[test]
fn invalid_config_and_duplicate_fields_are_sanitized() {
    let fixture = Fixture::new();
    for (pointer, value) in [
        ("/version", json!(2)),
        ("/task/unknown", json!(true)),
        (
            "/tools/Bash",
            json!({"operation":"replace","path_pointer":"/command"}),
        ),
        ("/tools/Write/path_pointer", json!("/paths/0")),
        ("/model/credential", json!("private-config-marker")),
        ("/model/config_revision", json!("unrelated")),
        ("/max_storage_bytes", json!(1)),
        (
            "/model/backend/endpoint_url",
            json!("http://not-loopback.example/"),
        ),
    ] {
        let mut config = fixture.config("http://127.0.0.1:1/no-network");
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        config
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        let request = json!({"api_version":"aw-provider/v1alpha1","request_id":"config","method":"validate_config","config":config});
        let output = finish(
            spawn(fixture.root.path(), false),
            request.to_string().as_bytes(),
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["status"], "error", "{pointer}");
        assert_eq!(response["error_code"], "invalid_config");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-config-marker"));
    }
    let output = finish(spawn(fixture.root.path(), false), br#"{"api_version":"aw-provider/v1alpha1","request_id":"dup","method":"describe","method":"describe"}"#);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error_code"],
        "invalid_request"
    );
    assert!(fixture.records().is_empty());
}

#[test]
fn full_candidate_normal_and_denial_never_emit_effects() {
    for (probability, expected) in [("0.99", "allow"), ("0.01", "block")] {
        let fixture = Fixture::new();
        let server = Server::new(probability, 200, Duration::ZERO);
        let request = fixture.request(fixture.config(&server.endpoint));
        let (_, diagnostic) = invoke(&fixture, request.clone(), true);
        assert_eq!(diagnostic["code"], "shadow_evaluated");
        let received = server
            .received
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            received["state"]["candidate"]["native_tool"],
            json!({"name":"Write","input":request["event"]["tool"]["input"]})
        );
        assert_eq!(
            received["state"]["authoritative_task"]["origin"],
            "operator"
        );
        let dir = &fixture.records()[0];
        let input = read(&dir.join("00-input.json"));
        assert_eq!(input["aw_request"]["event"], request["event"]);
        assert_eq!(input["filesystem_attested"], false);
        let result = read(&dir.join("40-result.json"));
        assert_eq!(
            result["evaluation"]["outcome"]["control"]["candidate"],
            expected
        );
        let projection = read(&dir.join("20-model-request.json"));
        assert_eq!(projection["state"], received["state"]);
        assert_eq!(projection["questions"], received["questions"]);
        assert_eq!(projection["backend"]["mode"], "loopback_test");
        assert_eq!(read(&dir.join("30-model-result.json"))["status"], "parsed");
        assert!(!fixture.root.path().join("greeting.txt").exists());
        for entry in fs::read_dir(dir).unwrap() {
            let bytes = fs::read(entry.unwrap().path()).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(KEY));
        }
    }
}

#[test]
fn explicit_single_path_delete_is_advisory_and_rejects_extra_targets() {
    for (name, pointer, preserve, extra) in [
        ("Delete", "/file_path", false, false),
        ("delete", "/path", true, false),
        ("Delete", "/path", false, true),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let file = fixture.root.path().join("greeting.txt");
        fs::write(&file, "retained synthetic data").unwrap();
        let mut config = fixture.config(&server.endpoint);
        config["tools"][name] = json!({"operation":"delete","path_pointer":pointer});
        if preserve {
            config["task"]["preserve_resource_ids"] = json!(["greeting"]);
        }
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!(name);
        request["event"]["tool"]["input"] = json!({&pointer[1..]:"greeting.txt"});
        if extra {
            request["event"]["tool"]["input"]["paths"] = json!(["private.txt"]);
        }
        let (_, diagnostic) = invoke(&fixture, request, true);
        let dir = &fixture.records()[0];
        if extra {
            assert_eq!(diagnostic["code"], "unsupported_input");
            assert!(server.received.try_recv().is_err());
        } else {
            assert_eq!(diagnostic["code"], "shadow_evaluated");
            assert_eq!(
                read(&dir.join("10-evaluation.json"))["candidate"]["operation"],
                "delete"
            );
            assert_eq!(
                read(&dir.join("40-result.json"))["evaluation"]["outcome"]["control"]["candidate"],
                if preserve { "block" } else { "allow" }
            );
        }
        assert_eq!(fs::read_to_string(file).unwrap(), "retained synthetic data");
    }
}

#[test]
fn integral_decimal_and_exponent_budgets_follow_aw_protocol() {
    for budget in ["2000.0", "2e3", "9007199254740991.0"] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let mut request = fixture.request(fixture.config(&server.endpoint));
        request["budget_ms"] = serde_json::from_str(budget).unwrap();
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{budget}");
        let evaluation = read(&fixture.records()[0].join("10-evaluation.json"));
        assert_eq!(
            evaluation["budget"]["deadline_ms"].as_u64().unwrap()
                - evaluation["budget"]["opened_at_ms"].as_u64().unwrap(),
            2000
        );
    }
}

#[test]
fn explicit_permissions_are_not_candidate_derived() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let mut request = fixture.request(fixture.config(&server.endpoint));
    request["event"]["tool"]["input"]["file_path"] = json!("private.txt");
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    let result = read(&fixture.records()[0].join("40-result.json"));
    assert_eq!(
        result["evaluation"]["answers"][0]["reason"],
        "outside_scope"
    );
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "block"
    );
    assert!(server.received.try_recv().is_err());
}

#[test]
fn preserve_overrides_allowance_without_model_call() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let mut config = fixture.config(&server.endpoint);
    config["task"]["preserve_resource_ids"] = json!(["greeting"]);
    invoke(&fixture, fixture.request(config), true);
    let result = read(&fixture.records()[0].join("40-result.json"));
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "block"
    );
    assert!(server.received.try_recv().is_err());
}

#[test]
fn mismatches_missing_identity_and_unmapped_tools_are_uncovered() {
    for (pointer, value, expected) in [
        ("/event/session_id", json!("other"), "context_mismatch"),
        ("/event/agent/adapter", json!("pi"), "context_mismatch"),
        ("/event/native/cwd", json!("/other"), "context_mismatch"),
        ("/event/agent/instance_id", Value::Null, "missing_identity"),
        ("/event/agent/binding_id", json!(" "), "missing_identity"),
        ("/event/tool/call_id", Value::Null, "missing_identity"),
        ("/event/tool/native_name", json!("Bash"), "unmapped_tool"),
        (
            "/event/tool/native_name",
            json!("MultiEdit"),
            "unmapped_tool",
        ),
        (
            "/event/tool/input",
            json!({"file_path":"greeting.txt","content":"ok","other_path":"private.txt"}),
            "unsupported_input",
        ),
        (
            "/event/tool/input/file_path",
            json!("other.txt"),
            "unmapped_resource",
        ),
        (
            "/event/tool/input/file_path",
            json!("../greeting.txt"),
            "unmapped_resource",
        ),
        (
            "/event/tool/input/file_path",
            json!("./greeting.txt"),
            "unmapped_resource",
        ),
        (
            "/event/tool/input/file_path",
            json!("dir//greeting.txt"),
            "unmapped_resource",
        ),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let mut request = fixture.request(fixture.config(&server.endpoint));
        *request.pointer_mut(pointer).unwrap() = value;
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], expected, "{pointer}");
        assert_eq!(
            read(&fixture.records()[0].join("40-result.json"))["status"],
            "uncovered"
        );
        assert!(server.received.try_recv().is_err());
    }
}

#[test]
fn oversized_candidate_is_saved_without_truncation_or_inference() {
    for size in [70_000, 300_000, 800_000] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let mut request = fixture.request(fixture.config(&server.endpoint));
        request["event"]["tool"]["input"]["content"] = json!("x".repeat(size));
        let (_, diagnostic) = invoke(&fixture, request.clone(), true);
        assert_eq!(diagnostic["code"], "oversized_candidate");
        let dir = &fixture.records()[0];
        assert_eq!(
            read(&dir.join("00-input.json"))["aw_request"]["event"],
            request["event"]
        );
        assert_eq!(read(&dir.join("40-result.json"))["status"], "uncovered");
        assert!(!dir.join("20-model-request.json").exists());
        assert!(server.received.try_recv().is_err());
    }
}

#[test]
fn credentials_and_remote_failures_remain_sanitized_shadow_results() {
    for key in [false, true] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 500, Duration::ZERO);
        let (_, diagnostic) = invoke(
            &fixture,
            fixture.request(fixture.config(&server.endpoint)),
            key,
        );
        let dir = &fixture.records()[0];
        if key {
            assert_eq!(diagnostic["code"], "shadow_evaluated");
            assert_eq!(read(&dir.join("30-model-result.json"))["error"], "http");
            assert_eq!(
                read(&dir.join("40-result.json"))["evaluation"]["judgments"][0]["error"],
                "http"
            );
        } else {
            assert_eq!(diagnostic["code"], "missing_credentials");
            assert!(server.received.try_recv().is_err());
        }
        for entry in fs::read_dir(dir).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains("private-remote-error-body") && !text.contains(KEY));
        }
    }
}

#[test]
fn changed_content_and_complete_native_ids_change_bindings() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let mut revisions = Vec::new();
    let mut identities = Vec::new();
    for index in 0..2 {
        let mut request = fixture.request(fixture.config(&server.endpoint));
        request["event"]["agent"]["instance_id"] = json!(format!("{}+{index}", "a".repeat(300)));
        request["event"]["tool"]["input"]["content"] = json!(format!("new content {index}"));
        invoke(&fixture, request, true);
        let dir = fixture.records().pop().unwrap();
        let evaluation = read(&dir.join("10-evaluation.json"));
        revisions.push(evaluation["binding"]["candidate_revision"].clone());
        identities.push(evaluation["binding"]["instance_id"].clone());
    }
    assert_ne!(revisions[0], revisions[1]);
    assert_ne!(identities[0], identities[1]);
}

#[test]
fn edit_and_absolute_path_keep_full_replacement_text() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let mut request = fixture.request(fixture.config(&server.endpoint));
    request["event"]["tool"]["native_name"] = json!("Edit");
    request["event"]["tool"]["input"] = json!({"file_path":fixture.root.path().join("greeting.txt"),"old_string":"old","new_string":"new","replace_all":false});
    invoke(&fixture, request.clone(), true);
    let received = server
        .received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        received["state"]["candidate"]["native_tool"]["input"],
        request["event"]["tool"]["input"]
    );
}
