//! Selector terminals retain service ordering, ownership, durable audit and closure.

use crate::{runtime::Runtime, Capabilities, Operation};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{atomic::AtomicBool, mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    runtime: Arc<Runtime>,
    instance: String,
    directory: tempfile::TempDir,
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

impl Fixture {
    fn new(before: Value, after: Value) -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("tool-matching-")
            .tempdir_in(target)
            .unwrap();
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("python3"))
            .find(|path| path.is_file())
            .unwrap();
        let python = fs::canonicalize(python).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aw-host/tests/fixtures");
        let mut document = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!(
                "../../../aw-config/examples/aw.minimal.yaml"
            ))
            .unwrap()
            .as_value()
            .clone();
        document["spec"]["providers"] = json!({
            "policy":{"protocol":aw_provider::VERSION, "config":{}, "timeout_ms":3000,
                "max_output_bytes":65536, "transport":{"type":"stdio", "location":"agent",
                "argv":[python, "-I", fixtures.join("provider.py"), directory.path(), "normal"]}},
            "hook":{"protocol":aw_config::HOOK_PROTOCOL, "config":{}, "timeout_ms":3000,
                "max_output_bytes":65536, "transport":{"type":"stdio", "location":"agent",
                "argv":[python, "-I", fixtures.join("boolean_hook.py"), directory.path()]}},
            "raw":{"protocol":"native-hook/v1alpha1", "config":{}, "timeout_ms":3000,
                "max_output_bytes":65536, "transport":{"type":"stdio", "location":"agent",
                "argv":["/bin/sh", "-c", "printf invoked > native-marker"]}}
        });
        document["spec"]["events"]["tool.before"]["steps"] = before;
        document["spec"]["events"]["tool.after"]["steps"] = after;
        let runtime = Runtime::new(
            serde_json::to_vec(&document).unwrap(),
            &directory.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let binding = runtime
            .handle(
                Operation::Bind {
                    target: "qoder".into(),
                    capabilities: Capabilities {
                        failure_actions: Default::default(),
                        adapter: "qoder".into(),
                        version: "fixture".into(),
                        entrypoint: "fixture".into(),
                        events: BTreeMap::from([
                            ("tool.before".into(), vec!["observe".into(), "block".into()]),
                            (
                                "tool.after".into(),
                                vec!["observe".into(), "replace_result".into()],
                            ),
                        ]),
                    },
                    cwd: directory.path().to_str().unwrap().into(),
                    environment: BTreeMap::new(),
                },
                deadline(),
                &AtomicBool::new(false),
            )
            .unwrap();
        Self {
            runtime,
            instance: binding["instance_id"].as_str().unwrap().into(),
            directory,
        }
    }

    fn open(&self, name: &str, scenario: &str) -> Value {
        self.runtime
            .handle(
                self.open_operation(name, scenario),
                deadline(),
                &AtomicBool::new(false),
            )
            .unwrap()
    }

    fn open_operation(&self, name: &str, scenario: &str) -> Operation {
        let mut event = json!({"name":name, "session_id":"session",
            "agent":{"adapter":"qoder", "binding_id":"qoder", "instance_id":self.instance},
            "tool":{"name":"file.read", "native_name":"read_file", "call_id":"call",
                "input":{}, "result":null}, "native":{"scenario":scenario}});
        if name == "tool.after" {
            event["tool"]["result"] = json!("original result");
            event["tool"]["result_projection"] = json!({"text":"original result",
                "status":"error", "content_origin":"file_content"});
        }
        Operation::OpenHookEvent {
            instance_id: self.instance.clone(),
            event,
            native_input: b"{}".to_vec(),
        }
    }

    fn invoke(&self, event: &Value, step: &str, instance: &str) -> crate::Result<Value> {
        self.runtime.handle(
            Operation::InvokeStep {
                event_id: event["event_id"].as_str().unwrap().into(),
                instance_id: instance.into(),
                step_id: step.into(),
                native_environment: None,
            },
            deadline(),
            &AtomicBool::new(false),
        )
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_dir(self.directory.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .map(|entry| serde_json::from_slice::<Value>(&fs::read(entry.path()).unwrap()).unwrap())
            .filter(|entry| entry["request"]["method"] == "invoke")
            .collect()
    }

    fn closed_audit(&self, event: &Value) -> Value {
        let until = deadline();
        loop {
            let audit = self
                .runtime
                .handle(
                    Operation::Audit {
                        key: event["event_id"].as_str().unwrap().into(),
                    },
                    deadline(),
                    &AtomicBool::new(false),
                )
                .unwrap();
            if audit["terminal"] == true {
                return audit;
            }
            assert!(Instant::now() < until, "event did not close");
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.maintain(true).unwrap();
        let status = self.runtime.audit.healthy();
        assert!(status, "audit failed during selector test");
    }
}

fn skipped_provider(id: &str) -> Value {
    json!({"id":id, "provider":"policy", "operation":"check", "effects":["observe"],
        "on_error":"block", "match":{"tools":["bash"]}})
}

#[test]
fn all_protocol_skips_close_automatically_and_cannot_replay() {
    let fixture = Fixture::new(
        json!([
            skipped_provider("structured"),
            {"id":"boolean", "provider":"hook", "effects":["block"], "on_error":"block", "match":{"tools":["bash"]}},
            {"id":"native", "provider":"raw", "native":{}, "on_error":"block", "match":{"tools":["bash"]}}
        ]),
        json!([]),
    );
    let event = fixture.open("tool.before", "block");
    assert_eq!(event["steps"], json!(["structured", "boolean", "native"]));
    assert!(fixture
        .invoke(&event, "structured", "wrong-instance")
        .is_err());
    for step in ["structured", "boolean", "native"] {
        let result = fixture.invoke(&event, step, &fixture.instance).unwrap();
        assert_eq!(result["kind"], "skipped");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["effects"], json!([]));
        assert!(result["call"]["process"].is_null());
        assert!(result["failure_action"].is_null());
        assert!(fixture.invoke(&event, step, &fixture.instance).is_err());
    }
    let audit = fixture.closed_audit(&event);
    let records = audit["records"].as_array().unwrap();
    let completed: Vec<_> = records
        .iter()
        .map(|record| &record["record"])
        .filter(|record| record["phase"] == "completed")
        .collect();
    assert_eq!(completed.len(), 3);
    assert!(completed.iter().all(|record| record["kind"] == "skipped"
        && record["status"] == "ok"
        && record["call"]["process"].is_null()
        && record["policy_block"] == false));
    let recent = fixture
        .runtime
        .handle(
            Operation::RecentExecutions,
            deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(recent["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|record| record["kind"] == "skipped"));
    assert_eq!(records.last().unwrap()["record"]["status"], "completed");
    assert_eq!(records.last().unwrap()["record"]["attempted_steps"], 3);
    assert!(fixture
        .runtime
        .handle(
            fixture.open_operation("tool.before", "block"),
            deadline(),
            &AtomicBool::new(false)
        )
        .is_err());
    assert!(fixture.calls().is_empty());
    assert!(!fixture.directory.path().join("hook-event.json").exists());
    assert!(!fixture.directory.path().join("native-marker").exists());
}

#[test]
fn skipped_step_keeps_a_later_deny_and_both_durable_completions() {
    let fixture = Fixture::new(
        json!([skipped_provider("skip"),
            {"id":"deny", "provider":"policy", "operation":"check", "effects":["block"], "on_error":"block"}
        ]),
        json!([]),
    );
    let event = fixture.open("tool.before", "block");
    assert_eq!(
        fixture.invoke(&event, "skip", &fixture.instance).unwrap()["kind"],
        "skipped"
    );
    let deny = fixture.invoke(&event, "deny", &fixture.instance).unwrap();
    assert_eq!(deny["kind"], "provider");
    assert_eq!(deny["effects"][0]["type"], "block");
    assert_eq!(fixture.calls().len(), 1);
    let audit = fixture.closed_audit(&event);
    let completed: Vec<_> = audit["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["record"])
        .filter(|record| record["phase"] == "completed")
        .collect();
    assert_eq!(completed[0]["kind"], "skipped");
    assert_eq!(completed[1]["policy_block"], true);
}

#[test]
fn ordered_replacement_waits_for_a_skipped_predecessor_and_preserves_projection() {
    let mut skip = skipped_provider("skip");
    skip["on_error"] = json!("report");
    let fixture = Fixture::new(
        json!([]),
        json!([skip,
            {"id":"replace", "provider":"policy", "operation":"replace", "effects":["replace_result"], "on_error":"report"}
        ]),
    );
    let event = fixture.open("tool.after", "replace");
    let (reply, result) = mpsc::sync_channel(1);
    thread::scope(|threads| {
        threads.spawn(|| {
            reply
                .send(fixture.invoke(&event, "replace", &fixture.instance))
                .unwrap()
        });
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(40)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(fixture.calls().is_empty());
        assert_eq!(
            fixture.invoke(&event, "skip", &fixture.instance).unwrap()["kind"],
            "skipped"
        );
        let replaced = result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(
            replaced["effects"],
            json!([{"type":"replace_result", "text":"short result"}])
        );
    });
    let calls = fixture.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0]["request"]["event"]["tool"]["result_projection"]["status"],
        "error"
    );
    let audit = fixture.closed_audit(&event);
    let completed: Vec<_> = audit["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["record"])
        .filter(|record| record["phase"] == "completed")
        .collect();
    assert_eq!(completed[0]["kind"], "skipped");
    assert_eq!(completed[0]["call"]["step_id"], "skip");
    assert_eq!(completed[1]["call"]["step_id"], "replace");
}
