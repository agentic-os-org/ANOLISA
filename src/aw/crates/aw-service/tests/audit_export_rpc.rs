//! Manual RPC completion reports survive Provider execution and public audit replay.

use aw_service::{audit_export, Capabilities, Client, Operation, ResultStatus, Server};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

struct Fixture {
    root: tempfile::TempDir,
    state: PathBuf,
    config: Vec<u8>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), aw_service::Error>>>,
    client: Client,
    instance: String,
    adapter: String,
}

impl Fixture {
    fn new(adapter: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("state");
        let mut config = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
            .unwrap()
            .as_value()
            .clone();
        config["spec"]["agents"]["qoder"]["adapter"] = json!(adapter);
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("python3"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        config["spec"]["providers"]["tokenless"] = json!({
            "protocol":"aw-provider/v1alpha1",
            "transport":{"type":"stdio","location":"agent","argv":[python,
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/audit_provider.py"), root.path()]},
            "timeout_ms":1000,"max_output_bytes":4096,"config":{}});
        for (name, operation, effect) in [
            ("tool.before", "observe", "observe"),
            ("tool.after", "post_tool", "replace_result"),
        ] {
            config["spec"]["events"][name]["steps"] = json!([
                {"id":"policy","provider":"tokenless","operation":operation,
                    "effects":[effect],"on_error":"report"}
            ]);
        }
        let mut events = BTreeMap::from([
            ("tool.before".into(), vec!["observe".into()]),
            ("tool.after".into(), vec!["replace_result".into()]),
        ]);
        if adapter == "openclaw" {
            config["spec"]["events"]["security.violation"] = json!({
                "enabled":true,"steps":[{"id":"policy","provider":"tokenless",
                    "operation":"observe","effects":["observe"],"on_error":"block"}]
            });
            events.insert(
                "security.violation".into(),
                vec!["observe".into(), "block".into()],
            );
        }
        let config = serde_json::to_vec(&config).unwrap();
        let server = Server::bind(config.clone(), &state).unwrap();
        let socket = server.socket_path();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::spawn(move || server.run(&stop));
        let client = Client::connect(socket, deadline()).unwrap();
        let mut fixture = Self {
            root,
            state,
            config,
            stopped,
            worker: Some(worker),
            client,
            instance: String::new(),
            adapter: adapter.into(),
        };
        let binding = fixture.call(Operation::Bind {
            target: "qoder".into(),
            capabilities: Capabilities {
                post_permission: false,
                adapter: adapter.into(),
                version: "fixture".into(),
                entrypoint: "manual-rpc".into(),
                failure_actions: BTreeMap::new(),
                events,
            },
            cwd: fixture.root.path().to_str().unwrap().into(),
            environment: BTreeMap::new(),
        });
        fixture.instance = binding["instance_id"].as_str().unwrap().into();
        fixture
    }

    fn call(&self, operation: Operation) -> Value {
        self.client.call(operation, deadline()).unwrap()
    }

    fn event(&self, name: &str, call: &str, result: Value) -> Value {
        json!({"name":name,"agent":{"adapter":self.adapter,"binding_id":"qoder","instance_id":self.instance},
            "session_id":"session","tool":{"name":"bash","native_name":"bash","call_id":call,
                "input":{"command":"private command"},"result":result},"native":{}})
    }

    fn provider_event(&self, mut event: Value) -> Value {
        event["cwd"] = json!(self.root.path());
        event
    }

    fn finish(&self, event: Value, status: Option<ResultStatus>) -> (String, Value) {
        let handle = self.call(Operation::OpenEvent {
            instance_id: self.instance.clone(),
            event,
            result_status: status,
        });
        let key = handle["event_id"].as_str().unwrap().to_owned();
        let result = self.call(Operation::InvokeStep {
            instance_id: self.instance.clone(),
            event_id: key.clone(),
            step_id: "policy".into(),
            native_environment: None,
        });
        self.call(Operation::CloseEvent {
            instance_id: self.instance.clone(),
            event_id: key.clone(),
        });
        (key, result)
    }

    fn query(&self, operation: Value) -> Value {
        audit_export::query(
            &self.state.join("journal"),
            &serde_json::to_vec(
                &json!({"api_version":audit_export::VERSION,"operation":operation}),
            )
            .unwrap(),
            deadline(),
            &AtomicBool::new(false),
        )
    }

    fn read(&self, key: &str) -> Value {
        self.query(json!({"type":"read","chain_id":key,"cursor":null,"limit":64}))
    }

    fn stop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap().unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn manual_completion_exports_provider_records_and_replays() {
    for adapter in ["openclaw", "qoder-sdk"] {
        let mut fixture = Fixture::new(adapter);
        let mut snapshots = Vec::new();
        let (key, _) = fixture.finish(fixture.event("tool.before", "before", Value::Null), None);
        assert!(fixture.read(&key)["error"].is_null());
        for (status, value) in [
            (
                ResultStatus::Success,
                json!({"status":"business-state","body":"private result"}),
            ),
            (ResultStatus::Error, json!("private error")),
            (ResultStatus::Interrupted, Value::Null),
            (ResultStatus::Denied, json!(["private denial"])),
        ] {
            let expected_status = serde_json::to_value(status).unwrap();
            let digest = aw_contracts::canonical::document_digest(&value).unwrap();
            for mode in ["declared", "projected", "both"] {
                let mut event = fixture.event("tool.after", mode, value.clone());
                if mode != "declared" {
                    event["tool"]["result_projection"] = json!({
                        "text":"private result", "status":expected_status, "content_origin":"command_output"});
                }
                let original = event.clone();
                let (key, reply) = fixture.finish(event, (mode != "projected").then_some(status));
                if mode != "declared" && status == ResultStatus::Success {
                    assert_eq!(reply["effects"][0]["type"], "replace_result", "{reply}");
                }
                let page = fixture.read(&key);
                assert!(page["error"].is_null(), "{page}");
                assert_eq!(page["result"]["terminal"], true);
                let fact = &page["result"]["event"]["data"];
                assert_eq!(fact["details"]["resultStatus"], expected_status);
                assert_eq!(
                    fact["details"]["resultDigest"],
                    json!({"algorithm":"sha256","value":digest})
                );
                assert_eq!(fact["association"]["status"], "unverified");
                let records = page["result"]["records"].as_array().unwrap();
                let completed = records
                    .iter()
                    .find(|r| r["data"]["kind"] == "provider.completed")
                    .unwrap();
                assert_eq!(completed["data"]["details"]["providerId"], "tokenless");
                assert_eq!(completed["data"]["details"]["status"], "ok");
                assert!(!page.to_string().contains("private"));
                let invoked =
                    fs::read_to_string(fixture.root.path().join("invocations.jsonl")).unwrap();
                let forwarded: Value =
                    serde_json::from_str(invoked.lines().last().unwrap()).unwrap();
                assert_eq!(forwarded, fixture.provider_event(original));
                snapshots.push((key, page));
            }
        }
        fixture.stop();
        let server = Server::bind(fixture.config.clone(), &fixture.state).unwrap();
        for (key, snapshot) in snapshots {
            assert_eq!(fixture.read(&key), snapshot);
        }
        drop(server);
    }
}

#[test]
fn legacy_completion_without_call_id_invokes_provider_and_keeps_audit_gap() {
    let fixture = Fixture::new("openclaw");
    let mut event = fixture.event("tool.after", "legacy", json!("tool output"));
    event["tool"]["call_id"] = Value::Null;
    event["tool"]["result_projection"] =
        json!({"text":"tool output","status":"success","content_origin":"command_output"});

    let (key, reply) = fixture.finish(event.clone(), None);
    assert_eq!(reply["effects"][0]["type"], "replace_result");
    assert_eq!(fixture.read(&key)["error"], "projection_unavailable");
    let invoked = fs::read_to_string(fixture.root.path().join("invocations.jsonl")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&invoked).unwrap(),
        fixture.provider_event(event)
    );
}

#[test]
fn codex_posttool_projection_agrees_with_manual_completion() {
    let fixture = Fixture::new("codex");
    let mut shell = fixture.event("tool.after", "shell", json!("tool output"));
    shell["native"] = json!({"hook_event_name":"PostToolUse","tool_name":"bash",
        "tool_input":shell["tool"]["input"],"tool_response":"tool output"});
    assert_eq!(aw_adapter::completion::result_status("codex", &shell), None);
    let (key, _) = fixture.finish(shell.clone(), None);
    assert_eq!(fixture.read(&key)["error"], "projection_unavailable");
    for declared in [ResultStatus::Error, ResultStatus::Success] {
        let (key, _) = fixture.finish(shell.clone(), Some(declared));
        let page = fixture.read(&key);
        assert!(page["error"].is_null(), "{page}");
        assert_eq!(
            page["result"]["event"]["data"]["details"]["resultStatus"],
            json!(declared)
        );
    }

    let mcp = |flag: &str, value: Value| json!({"content":[{"type":"text","text":"tool output"}],flag:value});
    for (index, (result, status)) in [
        (
            json!({"content":[{"type":"text","text":"tool output"}]}),
            ResultStatus::Success,
        ),
        (mcp("isError", json!(false)), ResultStatus::Success),
        (mcp("isError", json!(true)), ResultStatus::Error),
        (mcp("is_error", json!(true)), ResultStatus::Error),
        (mcp("exit_code", json!(1)), ResultStatus::Error),
        (mcp("exitCode", json!(1)), ResultStatus::Error),
        (mcp("interrupted", json!(true)), ResultStatus::Interrupted),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = fixture.event("tool.after", &format!("call-{index}"), result.clone());
        let tool = "mcp__fixture__echo";
        event["tool"]["name"] = json!(tool);
        event["tool"]["native_name"] = json!(tool);
        event["native"] = json!({"hook_event_name":"PostToolUse","tool_name":tool,
            "tool_input":event["tool"]["input"],"tool_response":result});
        aw_adapter::projection::add_result_projection(&mut event).unwrap();
        let expected = serde_json::to_value(status).unwrap();
        let native_status = aw_adapter::completion::result_status("codex", &event);
        assert_eq!(native_status, expected.as_str(), "{event}");
        if status == ResultStatus::Success {
            assert_eq!(
                event["tool"]["result_projection"]["status"].as_str(),
                native_status,
                "{event}"
            );
        } else {
            assert!(event["tool"].get("result_projection").is_none(), "{event}");
            assert!(aw_adapter::projection::reply("codex", &event["native"], "safe").is_err());
        }
        for declared in [None, Some(status)] {
            let (key, _) = fixture.finish(event.clone(), declared);
            let page = fixture.read(&key);
            assert!(page["error"].is_null(), "{page}");
            assert_eq!(
                page["result"]["event"]["data"]["details"]["resultStatus"],
                expected
            );
        }
    }
}

#[test]
fn codex_manual_completion_uses_native_exit_code_over_normalized_text() {
    let fixture = Fixture::new("codex");
    let mut event = fixture.event("tool.after", "call", json!("plain output"));
    event["native"] = json!({"hook_event_name":"PostToolUse","tool_name":"bash",
        "tool_input":event["tool"]["input"],
        "tool_response":{"stdout":"","stderr":"boom","exit_code":7}});

    let rejected = fixture.client.call(
        Operation::OpenEvent {
            instance_id: fixture.instance.clone(),
            event: event.clone(),
            result_status: Some(ResultStatus::Success),
        },
        deadline(),
    );
    assert!(
        matches!(rejected, Err(aw_service::Error::Rejected(ref code)) if code == "result_status_conflict"),
        "{rejected:?}"
    );
    for declared in [Some(ResultStatus::Error), None] {
        let (key, _) = fixture.finish(event.clone(), declared);
        let page = fixture.read(&key);
        assert!(page["error"].is_null(), "{page}");
        assert_eq!(
            page["result"]["event"]["data"]["details"]["resultStatus"],
            "error"
        );
    }
}

#[test]
fn manual_result_fields_only_affect_status_at_native_completion_callbacks() {
    for (adapter, business_field) in [
        ("pi", json!({"isError":true,"body":"tool output"})),
        (
            "deepseek-harness",
            json!({"isError":true,"body":"tool output"}),
        ),
        ("qwenpaw", json!({"state":"error","body":"tool output"})),
        ("agentscope", json!({"state":"error","body":"tool output"})),
    ] {
        let fixture = Fixture::new(adapter);
        let call_id = "03b8c201-1157-4ea5-977d-38173dfc160b";
        let event = fixture.event("tool.after", call_id, business_field.clone());
        let (key, _) = fixture.finish(event.clone(), Some(ResultStatus::Success));
        let page = fixture.read(&key);
        assert!(page["error"].is_null(), "{adapter}: {page}");
        let details = &page["result"]["event"]["data"]["details"];
        assert_eq!(details["resultStatus"], "success", "{adapter}");
        let digest = aw_contracts::canonical::document_digest(&business_field).unwrap();
        assert_eq!(details["resultDigest"]["value"], digest, "{adapter}");
        let invoked = fs::read_to_string(fixture.root.path().join("invocations.jsonl")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&invoked).unwrap(),
            fixture.provider_event(event.clone())
        );

        let mut native = event;
        let input = native["tool"]["input"].clone();
        let callback_input = if matches!(adapter, "qwenpaw" | "agentscope") {
            json!(input.to_string())
        } else {
            input.clone()
        };
        native["native"] = json!({
            "event":"tool.after","session_id":"session","invocation_id":call_id,
            "tool_call":{"id":call_id,"name":"bash","input":callback_input},
            "effective_input":input,"tool_response":business_field
        });
        let rejected = fixture.client.call(
            Operation::OpenEvent {
                instance_id: fixture.instance.clone(),
                event: native,
                result_status: Some(ResultStatus::Success),
            },
            deadline(),
        );
        assert!(
            matches!(rejected, Err(aw_service::Error::Rejected(ref code)) if code == "result_status_conflict"),
            "{adapter}: {rejected:?}"
        );
    }
}

#[test]
fn reported_completion_rejects_invalid_metadata_before_journaling() {
    for adapter in ["openclaw", "qoder-sdk"] {
        let fixture = Fixture::new(adapter);
        let inventory = fixture.query(json!({"type":"enumerate","cursor":null,"limit":64}));
        let after = fixture.event("tool.after", "call", json!("result"));
        let mut projected = after.clone();
        projected["tool"]["result_projection"] =
            json!({"text":"result","status":"error","content_origin":"command_output"});
        let mut native = after.clone();
        native["native"] = if adapter == "openclaw" {
            json!({"hook":"after_tool_call","event":{"error":"failed"}})
        } else {
            json!({"hook_event_name":"PostToolUseFailure"})
        };
        let mut native_success = after.clone();
        native_success["tool"]["result"] = json!({"isError":false});
        native_success["native"] = if adapter == "openclaw" {
            json!({"hook":"after_tool_call","event":{"result":{"isError":false}}})
        } else {
            json!({"hook_event_name":"PostToolUse"})
        };
        let mut missing_id = after.clone();
        missing_id["tool"]["call_id"] = Value::Null;
        let mut invalid_projection = projected.clone();
        invalid_projection["tool"]["result_projection"]["status"] = json!("unknown");
        for (event, status, code) in [
            (
                projected,
                Some(ResultStatus::Success),
                "result_status_conflict",
            ),
            (
                native,
                Some(ResultStatus::Success),
                "result_status_conflict",
            ),
            (
                native_success,
                Some(ResultStatus::Error),
                "result_status_conflict",
            ),
            (
                fixture.event("tool.before", "before", Value::Null),
                Some(ResultStatus::Success),
                "result_status_requires_tool_after",
            ),
            (
                missing_id,
                Some(ResultStatus::Success),
                "public_event_invalid",
            ),
            (invalid_projection, None, "event_invalid"),
        ] {
            let result = fixture.client.call(
                Operation::OpenEvent {
                    instance_id: fixture.instance.clone(),
                    event,
                    result_status: status,
                },
                deadline(),
            );
            assert!(
                matches!(result, Err(aw_service::Error::Rejected(ref actual)) if actual == code),
                "{code}: {result:?}"
            );
        }
        for value in [json!("unknown"), json!(true), json!(42), json!({})] {
            let request = json!({"method":"open_event","instance_id":fixture.instance,"event":after,"result_status":value});
            assert!(Operation::from_json(&serde_json::to_vec(&request).unwrap()).is_err());
        }
        assert_eq!(
            fixture.query(json!({"type":"enumerate","cursor":null,"limit":64})),
            inventory
        );
        assert!(!fixture.root.path().join("invocations.jsonl").exists());
    }
}

#[test]
fn legacy_security_violation_keeps_projection_and_provider_behavior() {
    let fixture = Fixture::new("openclaw");
    let result = json!({"content":[{"type":"text","text":"tool output"}]});
    let mut event = fixture.event("security.violation", "legacy", result.clone());
    event["tool"]["result_projection"] =
        json!({"text":"tool output","status":"success","content_origin":"command_output"});
    event["native"] = json!({"hook":"final_tool_result","event":{"result":result}});

    let rejected = fixture.client.call(
        Operation::OpenEvent {
            instance_id: fixture.instance.clone(),
            event: event.clone(),
            result_status: Some(ResultStatus::Success),
        },
        deadline(),
    );
    assert!(
        matches!(rejected, Err(aw_service::Error::Rejected(ref code)) if code == "result_status_requires_tool_after"),
        "{rejected:?}"
    );

    let (key, reply) = fixture.finish(event.clone(), None);
    assert_eq!(reply["effects"], json!([]));
    assert_eq!(fixture.read(&key)["error"], "projection_unavailable");
    // The security gate forwards the raw result without the text projection.
    event["tool"]
        .as_object_mut()
        .unwrap()
        .remove("result_projection");
    let invoked = fs::read_to_string(fixture.root.path().join("invocations.jsonl")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&invoked).unwrap(),
        fixture.provider_event(event)
    );
}

#[test]
fn rejected_first_completion_leaves_managed_protocol_available() {
    let fixture = Fixture::new("openclaw");
    let mut event = fixture.event("tool.after", "missing", json!("tool output"));
    event["tool"]["call_id"] = Value::Null;
    let rejected = fixture.client.call(
        Operation::OpenEvent {
            instance_id: fixture.instance.clone(),
            event,
            result_status: Some(ResultStatus::Success),
        },
        deadline(),
    );
    assert!(
        matches!(rejected, Err(aw_service::Error::Rejected(ref code)) if code == "public_event_invalid"),
        "{rejected:?}"
    );

    fixture.call(Operation::EnableInvocations {
        instance_id: fixture.instance.clone(),
    });
    fixture.call(Operation::RegisterInvocation {
        instance_id: fixture.instance.clone(),
        sequence: 1,
        call_id: "managed-call".into(),
        tool_name: "bash".into(),
        input: json!({"command":"true"}),
    });
}

#[test]
fn provider_failure_and_missing_tool_status_remain_distinct() {
    let fixture = Fixture::new("openclaw");
    let event = fixture.event("tool.after", "unknown", json!({"status":"success"}));
    let (key, _) = fixture.finish(event, None);
    assert_eq!(fixture.read(&key)["error"], "projection_unavailable");
    fs::write(fixture.root.path().join("fail-provider"), "").unwrap();
    let (key, _) = fixture.finish(
        fixture.event(
            "tool.after",
            "tool-ok-provider-failed",
            json!("private result"),
        ),
        Some(ResultStatus::Success),
    );
    let page = fixture.read(&key);
    assert!(page["error"].is_null(), "{page}");
    assert_eq!(
        page["result"]["event"]["data"]["details"]["resultStatus"],
        "success"
    );
    let completed = page["result"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["data"]["kind"] == "provider.completed")
        .unwrap();
    assert_eq!(completed["data"]["details"]["status"], "failed");
}

#[test]
fn minimal_posttool_reports_export_consistently_across_sdk_adapters() {
    let mut reports = Vec::new();
    let result = json!({"isError":false});
    for adapter in ["qoder-sdk", "claude-sdk", "qoder"] {
        let mut fixture = Fixture::new(adapter);
        let mut event = fixture.event("tool.after", "minimal-result", result.clone());
        event["native"] = json!({"hook_event_name":"PostToolUse"});
        let (key, _) = fixture.finish(event, None);
        let page = fixture.read(&key);
        fixture.stop();
        let server = Server::bind(fixture.config.clone(), &fixture.state).unwrap();
        assert_eq!(fixture.read(&key), page);
        drop(server);
        reports.push((adapter, page));
    }
    let outcomes: Vec<_> = reports
        .iter()
        .map(|(adapter, page)| (*adapter, page["error"].clone()))
        .collect();
    assert!(
        outcomes.iter().all(|(_, error)| error.is_null()),
        "{outcomes:?}"
    );
    let digest = aw_contracts::canonical::document_digest(&result).unwrap();
    for (adapter, page) in reports {
        let event = &page["result"]["event"]["data"];
        assert_eq!(event["details"]["resultStatus"], "success", "{adapter}");
        assert_eq!(
            event["details"]["resultDigest"],
            json!({"algorithm":"sha256","value":digest})
        );
        let completed = page["result"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["data"]["kind"] == "provider.completed")
            .unwrap();
        assert_eq!(completed["data"]["details"]["status"], "ok");
        assert_eq!(completed["data"]["details"]["providerId"], "tokenless");
    }
}
