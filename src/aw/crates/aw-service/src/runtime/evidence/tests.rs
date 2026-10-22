//! Exercise evidence trust and publication at the service's existing audit boundary.

use super::*;
use crate::{Capabilities, Operation};
use std::collections::BTreeMap;

#[test]
fn delayed_query_cannot_reclaim_a_released_instance_session() {
    let mut contexts = Contexts::new(include_bytes!(
        "../../../../aw-config/examples/aw.minimal.yaml"
    ))
    .unwrap();
    contexts.sources.insert(
        "qoder".into(),
        Settings {
            mode: "auto".into(),
            source_id: "fixture".into(),
            endpoint: "https://agentsight.invalid/".into(),
            token_file: Some("/missing-token".into()),
            query_timeout_ms: 1000,
            query_bytes: 4096,
            max_items: 4,
            max_bytes: 4096,
        },
    );
    contexts.activate("first").unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let seed = contexts
        .seed(
            json!({"binding_id":"qoder","instance_id":"first","adapter":"qoder"}),
            &json!({"name":"tool.before","tool":{}}),
            json!({"status":"verified","session_id":"resumed"}),
            deadline,
            aw_config::Validator::new()
                .unwrap()
                .parse(include_bytes!(
                    "../../../../aw-config/examples/aw.minimal.yaml"
                ))
                .unwrap()
                .as_value(),
        )
        .unwrap();
    contexts.sessions.lock().unwrap().active.remove("first");
    assert_eq!(contexts.query(&seed, deadline).2, "source_instance_closed");
    assert!(contexts.sessions.lock().unwrap().owners.is_empty());
    contexts.activate("second").unwrap();
    let mut resumed = seed;
    resumed.subject["instance_id"] = json!("second");
    assert_eq!(
        contexts.query(&resumed, deadline).2,
        "source_authentication"
    );
    assert_eq!(
        contexts
            .sessions
            .lock()
            .unwrap()
            .owners
            .values()
            .next()
            .unwrap(),
        "second"
    );
}

#[test]
fn poisoned_audit_prevents_context_publication() {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/evidence-unit-tests");
    std::fs::create_dir_all(&parent).unwrap();
    let directory = tempfile::tempdir_in(parent).unwrap();
    let runtime = Runtime::new(
        include_bytes!("../../../../aw-config/examples/aw.minimal.yaml").to_vec(),
        &directory.path().join("journal"),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    struct Cleanup(Arc<Runtime>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.maintain(true).unwrap();
        }
    }
    let _cleanup = Cleanup(runtime.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    let cancelled = AtomicBool::new(false);
    let binding = runtime
        .handle(
            Operation::Bind {
                target: "qoder".into(),
                capabilities: Capabilities {
                    adapter: "qoder".into(),
                    version: "fixture".into(),
                    entrypoint: "fixture".into(),
                    events: BTreeMap::from([
                        ("tool.before".into(), vec!["observe".into(), "block".into()]),
                        ("tool.after".into(), vec!["observe".into()]),
                    ]),
                },
                cwd: directory.path().to_str().unwrap().into(),
                environment: BTreeMap::new(),
            },
            deadline,
            &cancelled,
        )
        .unwrap();
    let instance = binding["instance_id"].as_str().unwrap();
    let handle = runtime.handle(Operation::OpenEvent {
        instance_id:instance.into(), event:json!({"name":"tool.after", "agent":{"adapter":"qoder","binding_id":"qoder","instance_id":instance},
            "tool":{"input":{},"result":"private candidate"},"native":{}}),
    }, deadline, &cancelled).unwrap();
    let key = handle["event_id"].as_str().unwrap();
    // Invalid append poisons the same FileJournal claim used by real I/O failures.
    assert!(runtime.audit.append(key, Value::Null).is_err());
    assert!(runtime
        .freeze_context(
            key,
            instance,
            handle["candidate_revision"].as_str().unwrap(),
            deadline
        )
        .is_err());
    assert!(!runtime.audit.healthy());
    let stored = std::fs::read_dir(directory.path().join("journal"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(!stored.contains("context_frozen"));
    assert!(!stored.contains("private candidate"));
}

#[test]
fn native_run_scoping_and_missing_evidence_are_preserved_for_every_adapter() {
    for adapter in ["qoder", "qwenpaw", "hermes", "openclaw"] {
        let call = match adapter {
            "hermes" => format!(
                "hermes:{}",
                canonical::digest(json!(["request", "call"]).to_string().as_bytes())
            ),
            "openclaw" => canonical::digest(json!(["run", "call"]).to_string().as_bytes()),
            _ => "call".into(),
        };
        let native = match adapter {
            "qoder" => {
                json!({"hook_event_name":"PostToolUse","session_id":"session","tool_use_id":"call","tool_name":"exec","tool_input":{},"tool_response":"original"})
            }
            "qwenpaw" => {
                json!({"event":"tool.after","session_id":"session","tool_call":{"id":"call","name":"exec","input":"{}"},"tool_response":"original"})
            }
            "hermes" => {
                json!({"hook_event_name":"transform_tool_result","session_id":"session","tool_name":"exec","tool_input":{},"extra":{"api_request_id":"request","tool_call_id":"call","result":"original"}})
            }
            "openclaw" => {
                json!({"hook":"after_tool_call","event":{"toolName":"bash","toolCallId":"call","runId":"run","params":{},"result":"original"},"context":{"sessionId":"session"}})
            }
            _ => unreachable!(),
        };
        let event = json!({"name":"tool.after","session_id":"session","tool":{"name":"exec","native_name":"exec","call_id":call,"input":{},"result":"original"},"native":native});
        let bytes = serde_json::to_vec(&native).unwrap();
        assert_eq!(
            association(adapter, &event, Some(&bytes))["status"],
            "verified",
            "{adapter}"
        );
        assert_eq!(association(adapter, &event, None)["status"], "unverified");
        for pointer in ["/session_id", "/tool/call_id", "/tool/result"] {
            let mut forged = event.clone();
            *forged.pointer_mut(pointer).unwrap() = json!("forged");
            assert_eq!(
                association(adapter, &forged, Some(&bytes))["status"],
                "unverified",
                "{adapter}: {pointer}"
            );
        }
        if matches!(adapter, "hermes" | "openclaw") {
            let mut next = event;
            let pointer = if adapter == "hermes" {
                "/native/extra/api_request_id"
            } else {
                "/native/event/runId"
            };
            *next.pointer_mut(pointer).unwrap() = json!("next-run");
            let bytes = serde_json::to_vec(&next["native"]).unwrap();
            assert_eq!(
                association(adapter, &next, Some(&bytes))["status"],
                "unverified"
            );
        }
    }
}

#[test]
fn evidence_limits_accept_schema_integer_spellings_without_rewriting_config() {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/evidence-unit-tests");
    std::fs::create_dir_all(&parent).unwrap();
    let base = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!(
            "../../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap()
        .as_value()
        .clone();
    for limits in [
        [1, 1, 1, 4096],
        [1000, 4096, 32, 262144],
        [60000, 4194304, 128, 2097152],
    ] {
        for suffix in ["", ".0", "e0"] {
            let mut document = base.clone();
            document["spec"]["agents"]["qoder"]["evidence"] = json!({
                "source_id":"fixture", "endpoint":"http://127.0.0.1:1",
            });
            for (field, value) in ["query_timeout_ms", "query_bytes", "max_items", "max_bytes"]
                .into_iter()
                .zip(limits)
            {
                document["spec"]["agents"]["qoder"]["evidence"][field] =
                    serde_json::from_str(&format!("{value}{suffix}")).unwrap();
            }
            let bytes = serde_json::to_vec(&document).unwrap();
            let directory = tempfile::tempdir_in(&parent).unwrap();
            let runtime = Runtime::new(
                bytes.clone(),
                &directory.path().join("journal"),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            let settings = &runtime.contexts.sources["qoder"];
            assert_eq!(settings.query_timeout_ms, limits[0]);
            assert_eq!(settings.query_bytes, limits[1] as usize);
            assert_eq!(settings.max_items, limits[2] as usize);
            assert_eq!(settings.max_bytes, limits[3] as usize);
            assert_eq!(runtime.bytes, bytes);
            assert_eq!(
                serde_json::from_slice::<Value>(&runtime.bytes).unwrap(),
                document
            );
        }
    }
}
