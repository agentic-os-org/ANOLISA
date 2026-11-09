//! Real service persistence and export, using deterministic native callback fixtures.

use aw_service::{audit_export, Capabilities, Client, Operation, RunOutcome, Server};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
struct Stop(Arc<AtomicBool>);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn qoder_and_qwenpaw_facts_export_after_service_shutdown_without_sight() {
    for adapter in ["qoder-sdk", "qwenpaw"] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = directory.path().join("state");
        let mut config = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
            .unwrap()
            .as_value()
            .clone();
        config["spec"]["agents"]["qoder"]["adapter"] = json!(adapter);
        let bytes = serde_json::to_vec(&config).unwrap();
        let server = Server::bind(bytes.clone(), &state).unwrap();
        let socket = server.socket_path();
        let stopped = Arc::new(AtomicBool::new(false));
        let snapshot = thread::scope(|scope| {
            let stop = Arc::clone(&stopped);
            let worker = scope.spawn(move || server.run(&stop));
            let _stop = Stop(Arc::clone(&stopped));
            let client = Client::connect(&socket, deadline()).unwrap();
            let binding = client
                .call(
                    Operation::Bind {
                        target: "qoder".into(),
                        capabilities: Capabilities {
                            adapter: adapter.into(),
                            version: "fixture".into(),
                            entrypoint: "fixture".into(),
                            failure_actions: BTreeMap::new(),
                            events: BTreeMap::from([
                                ("tool.before".into(), vec!["observe".into()]),
                                ("tool.after".into(), vec!["observe".into()]),
                            ]),
                        },
                        cwd: directory.path().to_str().unwrap().into(),
                        environment: BTreeMap::new(),
                    },
                    deadline(),
                )
                .unwrap();
            let id = binding["instance_id"].as_str().unwrap().to_owned();
            client
                .call(
                    Operation::ReportRun {
                        instance_id: id.clone(),
                        outcome: RunOutcome::Started {
                            pid: std::process::id(),
                        },
                    },
                    deadline(),
                )
                .unwrap();
            let input = json!({"command":"private command content"});
            let native = if adapter == "qwenpaw" {
                json!({"event":"tool.before","session_id":"session","tool_call":{"name":"bash","id":"call","input":input.to_string()}})
            } else {
                json!({"hook_event_name":"PreToolUse","session_id":"session","tool_use_id":"call","aw_native_tool_use_id":"native-call","tool_name":"Bash","tool_input":input})
            };
            let event = json!({"name":"tool.before","agent":{"adapter":adapter,"binding_id":"qoder","instance_id":id},
                "session_id":"session","tool":{"name":"bash","native_name":if adapter=="qwenpaw" {"bash"} else {"Bash"},
                "call_id":"call","input":input,"result":null},"native":native});
            client
                .call(
                    Operation::ObserveHookEvent {
                        instance_id: id.clone(),
                        event,
                        native_input: serde_json::to_vec(&native).unwrap(),
                    },
                    deadline(),
                )
                .unwrap();
            client
                .call(
                    Operation::ReportRun {
                        instance_id: id,
                        outcome: RunOutcome::Exited {
                            code: Some(0),
                            signal: None,
                        },
                    },
                    deadline(),
                )
                .unwrap();
            let snapshot = read_all(&state.join("journal"));
            stopped.store(true, Ordering::Release);
            worker.join().unwrap().unwrap();
            snapshot
        });
        let replay = read_all(&state.join("journal"));
        assert_eq!(snapshot, replay);
        let events: Vec<_> = replay
            .iter()
            .map(|r| r["result"]["event"]["data"]["name"].as_str().unwrap())
            .collect();
        for name in ["instance.started", "tool.before", "instance.ended"] {
            assert!(events.contains(&name), "{events:?}");
        }
        let tool = replay
            .iter()
            .map(|page| &page["result"]["event"]["data"])
            .find(|event| event["name"] == "tool.before")
            .unwrap();
        assert_eq!(tool["subject"]["toolCallId"], "call");
        assert_eq!(
            tool["nativeRefs"]["toolCallId"],
            if adapter == "qoder-sdk" {
                "native-call"
            } else {
                "call"
            }
        );
        assert!(!serde_json::to_string(&replay)
            .unwrap()
            .contains("private command content"));
        let restarted = Server::bind(bytes, &state).unwrap();
        assert_eq!(read_all(&state.join("journal")), replay);
        drop(restarted);
    }
}

fn read_all(directory: &std::path::Path) -> Vec<Value> {
    let request = |operation: Value| {
        audit_export::query(
            directory,
            &serde_json::to_vec(
                &json!({"api_version":audit_export::VERSION,"operation":operation}),
            )
            .unwrap(),
            deadline(),
            &AtomicBool::new(false),
        )
    };
    let inventory = request(json!({"type":"enumerate","cursor":null,"limit":64}));
    assert!(inventory["error"].is_null(), "{inventory}");
    assert!(inventory["result"]["next_cursor"].is_null());
    let mut output = Vec::new();
    for key in inventory["result"]["keys"].as_array().unwrap() {
        let page = request(json!({"type":"read","chain_id":key,"cursor":null,"limit":64}));
        if page["error"] == "unsupported_history" {
            continue;
        }
        assert!(page["error"].is_null(), "{page}");
        assert_eq!(page["result"]["caught_up"], true);
        assert_eq!(page["result"]["terminal"], true);
        output.push(page);
    }
    output
}
