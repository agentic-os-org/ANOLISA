//! Producer and replay tests use runtime boundaries, with no observation backend.

use super::*;
use crate::{public_projection, Capabilities};
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

struct Fixture {
    root: tempfile::TempDir,
    runtime: Arc<Runtime>,
    id: String,
    adapter: String,
    bytes: Vec<u8>,
}

impl Fixture {
    fn new(adapter: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut config = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!(
                "../../../../aw-config/examples/aw.minimal.yaml"
            ))
            .unwrap()
            .as_value()
            .clone();
        config["spec"]["agents"]["qoder"]["adapter"] = json!(adapter);
        let bytes = serde_json::to_vec(&config).unwrap();
        let runtime = Runtime::new(
            bytes.clone(),
            &root.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let bound = runtime
            .bind(
                "qoder".into(),
                Capabilities {
                    adapter: adapter.into(),
                    version: "fixture".into(),
                    entrypoint: "fixture".into(),
                    failure_actions: BTreeMap::new(),
                    events: BTreeMap::from([
                        ("tool.before".into(), vec!["observe".into()]),
                        ("tool.after".into(), vec!["observe".into()]),
                    ]),
                },
                aw_host::ProcessContext {
                    cwd: root.path().into(),
                    environment: BTreeMap::new(),
                    stderr_bytes: 1024,
                },
                deadline(),
                &AtomicBool::new(false),
            )
            .unwrap();
        Self {
            root,
            runtime,
            id: bound["instance_id"].as_str().unwrap().into(),
            adapter: adapter.into(),
            bytes,
        }
    }

    fn callback(&self, after: bool, call: &str) -> (Value, Vec<u8>) {
        let input = json!({"command":"private input"});
        let result = if self.adapter == "qwenpaw" {
            json!({"state":"success","content":[{"text":"private result"}]})
        } else {
            json!("private result")
        };
        let event = if after { "tool.after" } else { "tool.before" };
        let native = if self.adapter == "qwenpaw" {
            let mut native = json!({"event":event,"session_id":"session","tool_call":{"name":"bash","id":call,"input":input.to_string()}});
            if after {
                native["tool_response"] = result.clone();
            }
            native
        } else {
            let mut native = json!({"hook_event_name":if after {"PostToolUse"} else {"PreToolUse"},"session_id":"session","tool_use_id":call,"tool_name":"Bash","tool_input":input});
            if after {
                native["tool_response"] = result.clone();
            }
            native
        };
        (
            json!({"name":event,"agent":{"adapter":self.adapter,"binding_id":"qoder","instance_id":self.id},"session_id":"session",
            "tool":{"name":"bash","native_name":if self.adapter=="qwenpaw" {"bash"} else {"Bash"},"call_id":call,"input":input,"result":if after {result} else {Value::Null}},"native":native}),
            serde_json::to_vec(&native).unwrap(),
        )
    }

    fn records(&self) -> Vec<Value> {
        let journal = aw_core::journal::FileJournal::new(self.root.path().join("audit")).unwrap();
        let mut output = Vec::new();
        for entry in fs::read_dir(self.root.path().join("audit")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let chain = journal
                .read(path.file_stem().unwrap().to_str().unwrap())
                .unwrap();
            if let Some(event) = public_projection::execution_event(&chain[0]).unwrap() {
                output.push(event.clone());
                for envelope in chain {
                    if let Some(record) =
                        public_projection::project_record(&envelope, &event).unwrap()
                    {
                        output.push(record);
                    }
                }
            }
        }
        output.sort_by_key(|value| value["metadata"]["uid"].as_str().unwrap().to_owned());
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.maintain(true).unwrap();
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn ordinary_run_start_replay_remains_invalid() {
    let fixture = Fixture::new("qoder");
    let pid = std::process::id();
    fixture
        .runtime
        .report_run(&fixture.id, RunOutcome::Started { pid })
        .unwrap();
    assert!(matches!(
        fixture.runtime.report_run(&fixture.id, RunOutcome::Started { pid }),
        Err(crate::Error::Rejected(code)) if code == "run_transition_invalid"
    ));
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|record| record["data"]["name"] == "instance.started")
            .count(),
        1
    );
}

#[test]
fn native_metadata_without_sight_is_stable_across_reader_restart() {
    for adapter in ["qoder-sdk", "qwenpaw"] {
        let fixture = Fixture::new(adapter);
        fixture
            .runtime
            .report_run(
                &fixture.id,
                RunOutcome::Started {
                    pid: std::process::id(),
                },
            )
            .unwrap();
        for after in [false, true] {
            let (event, native) = fixture.callback(after, "call");
            let observed = fixture
                .runtime
                .observe_hook(&fixture.id, event.clone(), native.clone(), deadline())
                .unwrap();
            assert_eq!(observed["duplicate"], false);
            assert_eq!(
                fixture
                    .runtime
                    .observe_hook(&fixture.id, event, native, deadline())
                    .unwrap()["duplicate"],
                true
            );
        }
        fixture
            .runtime
            .report_run(
                &fixture.id,
                RunOutcome::Exited {
                    code: Some(7),
                    signal: None,
                },
            )
            .unwrap();
        let records = fixture.records();
        assert_eq!(records.len(), 12);
        let serialized = serde_json::to_string(&records).unwrap();
        for private in ["private input", "private result", "command"] {
            assert!(!serialized.contains(private));
        }
        let events: Vec<_> = records
            .iter()
            .filter(|value| value["kind"] == "ExecutionEvent")
            .collect();
        assert_eq!(events.len(), 4);
        let after = events
            .iter()
            .find(|value| value["data"]["name"] == "tool.after")
            .unwrap();
        assert_eq!(after["data"]["details"]["resultStatus"], "success");
        assert_eq!(
            after["data"]["association"]["verifiedFields"],
            json!(["sessionId", "toolCallId"])
        );
        assert_eq!(after["data"]["subject"]["turnId"], Value::Null);
        let restarted = Runtime::new(
            fixture.bytes.clone(),
            &fixture.root.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_ne!(
            restarted.identity.generation,
            fixture.runtime.identity.generation
        );
        assert_eq!(restarted.audit.source_id, fixture.runtime.audit.source_id);
        assert_eq!(records, fixture.records());
        restarted.maintain(true).unwrap();
    }
}

#[test]
fn provider_result_never_claims_native_adoption() {
    let fixture = Fixture::new("qwenpaw");
    let (event, native) = fixture.callback(false, "call");
    let association = super::super::evidence::association("qwenpaw", &event, Some(&native));
    let key = fixture.runtime.identifier().unwrap();
    let fact = {
        let instances = fixture.runtime.instances.lock().unwrap();
        fixture
            .runtime
            .hook_fact(
                &key,
                &instances[&fixture.id].as_ref().unwrap().host,
                &fixture.id,
                &event,
                &association,
            )
            .unwrap()
            .unwrap()
    };
    fixture
        .runtime
        .audit
        .claim(&key, json!({"executionEvent":fact}))
        .unwrap();
    fixture
        .runtime
        .audit
        .append(
            &key,
            json!({"kind":"provider","phase":"completed","status":"ok","policy_block":true,
        "call":{"provider":"policy","step_id":"check","request_id":"request"}}),
        )
        .unwrap();
    fixture
        .runtime
        .audit
        .append(&key, json!({"phase":"closed","status":"completed"}))
        .unwrap();
    fixture.runtime.audit.release(&key);
    let records = fixture.records();
    let completed = records
        .iter()
        .find(|r| r["data"]["kind"] == "provider.completed")
        .unwrap();
    assert_eq!(completed["data"]["details"]["policyBlock"], true);
    let closed = records
        .iter()
        .find(|r| r["data"]["kind"] == "event.closed")
        .unwrap();
    assert_eq!(closed["data"]["details"]["outcome"], "incomplete");
    assert_eq!(
        closed["data"]["details"]["reasonCode"],
        "aw.native_adoption_unconfirmed"
    );
}

#[test]
fn retired_native_metadata_does_not_exhaust_a_long_task() {
    let fixture = Fixture::new("qwenpaw");
    fixture.runtime.enable_invocations(&fixture.id).unwrap();
    for sequence in 1..=300 {
        let call = format!("call-{sequence}");
        let (event, native) = fixture.callback(false, &call);
        fixture
            .runtime
            .register_invocation(
                &fixture.id,
                sequence,
                &call,
                "bash",
                event["tool"]["input"].clone(),
            )
            .unwrap();
        fixture
            .runtime
            .observe_hook(&fixture.id, event.clone(), native.clone(), deadline())
            .unwrap();
        fixture
            .runtime
            .finish_invocation(&fixture.id, sequence, &call, deadline())
            .unwrap();
        assert!(fixture
            .runtime
            .observe_hook(&fixture.id, event, native, deadline())
            .is_err());
    }
    let instances = fixture.runtime.instances.lock().unwrap();
    assert!(instances[&fixture.id]
        .as_ref()
        .unwrap()
        .native_facts
        .lock()
        .unwrap()
        .claims
        .is_empty());
}

#[test]
fn unrecognized_result_preserves_gap_and_no_success_fact() {
    let fixture = Fixture::new("qwenpaw");
    let (mut event, _) = fixture.callback(true, "unknown");
    event["tool"]["result"]["state"] = json!("future_status");
    event["native"]["tool_response"] = event["tool"]["result"].clone();
    fixture
        .runtime
        .observe_hook(
            &fixture.id,
            event.clone(),
            serde_json::to_vec(&event["native"]).unwrap(),
            deadline(),
        )
        .unwrap();
    assert!(fixture.records().is_empty());
    let text = fs::read_dir(fixture.root.path().join("audit"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(text.contains("aw.native_metadata_unavailable"));
    assert!(!text.contains("private result"));
}

#[test]
fn unmanaged_callback_limit_records_one_gap_without_growing_claims() {
    let fixture = Fixture::new("qwenpaw");
    for sequence in 0..256 {
        let (event, native) = fixture.callback(false, &format!("call-{sequence}"));
        fixture
            .runtime
            .observe_hook(&fixture.id, event, native, deadline())
            .unwrap();
    }
    for call in ["over-one", "over-two"] {
        let (event, native) = fixture.callback(false, call);
        assert!(
            matches!(fixture.runtime.observe_hook(&fixture.id,event,native,deadline()),Err(crate::Error::Rejected(code)) if code=="observation_callback_limit")
        );
    }
    let text = fs::read_dir(fixture.root.path().join("audit"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert_eq!(text.matches("aw.observation_callback_limit").count(), 1);
    assert_eq!(
        fixture.runtime.instances.lock().unwrap()[&fixture.id]
            .as_ref()
            .unwrap()
            .native_facts
            .lock()
            .unwrap()
            .claims
            .len(),
        256
    );
}

#[test]
fn durable_failure_does_not_publish_run_state_or_retry_native_facts() {
    let fixture = Fixture::new("qwenpaw");
    let directory = fixture.root.path().join("audit");
    fs::rename(&directory, fixture.root.path().join("retained")).unwrap();
    fs::write(&directory, b"unavailable").unwrap();
    assert!(fixture
        .runtime
        .report_run(
            &fixture.id,
            RunOutcome::Started {
                pid: std::process::id()
            }
        )
        .is_err());
    assert!(fixture.runtime.instances.lock().unwrap()[&fixture.id]
        .as_ref()
        .unwrap()
        .run
        .is_none());
    assert!(!fixture.runtime.audit.healthy());
    // Restore only the fixture storage so normal owned cleanup remains possible.
    fs::remove_file(&directory).unwrap();
    fs::rename(fixture.root.path().join("retained"), &directory).unwrap();
}

#[test]
fn explicit_native_failures_keep_status_and_distinct_invocation_ids() {
    for adapter in ["qwenpaw", "qoder-sdk"] {
        let fixture = Fixture::new(adapter);
        for status in ["error", "interrupted"] {
            let (mut event, _) = fixture.callback(true, status);
            event["tool"]["input"] = json!({"ratio": 1.25});
            if adapter == "qwenpaw" {
                event["native"]["tool_call"]["input"] = json!(event["tool"]["input"].to_string());
                event["native"]["tool_call"]["id"] = json!(format!("raw-{status}"));
                event["native"]["aw_invocation_id"] = json!(status);
                event["tool"]["result"]["state"] = json!(status);
                event["native"]["tool_response"] = event["tool"]["result"].clone();
            } else {
                event["native"]["tool_input"] = event["tool"]["input"].clone();
                event["native"]["hook_event_name"] = json!("PostToolUseFailure");
                event["native"]
                    .as_object_mut()
                    .unwrap()
                    .remove("tool_response");
                event["native"]["error"] = json!("private failure");
                event["native"]["is_interrupt"] = json!(status == "interrupted");
                event["tool"]["result"] = json!({"is_error":true,"error":"private failure","is_interrupt":status=="interrupted"});
            }
            fixture
                .runtime
                .observe_hook(
                    &fixture.id,
                    event.clone(),
                    serde_json::to_vec(&event["native"]).unwrap(),
                    deadline(),
                )
                .unwrap();
        }
        let objects = fixture.records();
        let events: Vec<_> = objects
            .iter()
            .filter(|value| value["kind"] == "ExecutionEvent")
            .collect();
        assert_eq!(events.len(), 2);
        for event in events {
            let status = event["data"]["details"]["resultStatus"].as_str().unwrap();
            assert!(matches!(status, "error" | "interrupted"));
            assert_eq!(event["data"]["subject"]["toolCallId"], status);
            assert_eq!(
                event["data"]["nativeRefs"]["toolCallId"],
                if adapter == "qwenpaw" {
                    format!("raw-{status}")
                } else {
                    status.to_owned()
                }
            );
        }
        assert!(!serde_json::to_string(&objects)
            .unwrap()
            .contains("private failure"));
    }
}

#[test]
fn native_claim_failure_rejects_waiting_and_repeated_callbacks() {
    let fixture = Fixture::new("qwenpaw");
    let (event, native) = fixture.callback(false, "claim-failure");
    let facts = fixture.runtime.instances.lock().unwrap()[&fixture.id]
        .as_ref()
        .unwrap()
        .native_facts
        .clone();
    let journal = fixture.runtime.audit.hold_journal();
    std::thread::scope(|scope| {
        let observe = || {
            fixture
                .runtime
                .observe_hook(&fixture.id, event.clone(), native.clone(), deadline())
        };
        let first = scope.spawn(observe);
        let until = deadline();
        while facts.try_lock().is_ok() && Instant::now() < until {
            std::thread::yield_now();
        }
        let first_reserved = facts.try_lock().is_err();
        let second = scope.spawn(observe);
        let until = deadline();
        while fixture
            .runtime
            .audit
            .observation_attempts
            .load(Ordering::Acquire)
            < 2
            && Instant::now() < until
        {
            std::thread::yield_now();
        }
        let second_arrived = fixture
            .runtime
            .audit
            .observation_attempts
            .load(Ordering::Acquire)
            == 2;
        let directory = fixture.root.path().join("audit");
        fs::rename(&directory, fixture.root.path().join("retained")).unwrap();
        fs::write(&directory, b"unavailable").unwrap();
        // Both callers have reached admission while the first owns the fact lock.
        drop(journal);
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        fs::remove_file(&directory).unwrap();
        fs::rename(fixture.root.path().join("retained"), &directory).unwrap();
        assert!(first_reserved && second_arrived);
        assert_eq!(
            first.unwrap_err().to_string(),
            "AW service: audit_unavailable"
        );
        assert_eq!(
            second.unwrap_err().to_string(),
            "AW service: audit_unavailable"
        );
    });
    assert!(!fixture.runtime.audit.healthy());
    assert_eq!(
        fixture
            .runtime
            .observe_hook(&fixture.id, event, native, deadline())
            .unwrap_err()
            .to_string(),
        "AW service: audit_unavailable"
    );
    assert!(fixture.records().is_empty());
}

#[test]
fn native_close_failure_never_acknowledges_a_duplicate() {
    for failure in [1, 2] {
        let fixture = Fixture::new("qwenpaw");
        let (event, native) = fixture.callback(false, "close-failure");
        fixture
            .runtime
            .audit
            .fact_close_failure
            .store(failure, Ordering::Release);
        assert_eq!(
            fixture
                .runtime
                .observe_hook(&fixture.id, event.clone(), native.clone(), deadline())
                .unwrap_err()
                .to_string(),
            "AW service: audit_unavailable"
        );
        assert!(!fixture.runtime.audit.healthy());
        let records = fixture.records();
        assert_eq!(
            records
                .iter()
                .filter(|record| record["kind"] == "ExecutionEvent")
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .any(|record| record["data"]["kind"] == "event.closed"),
            failure == 2
        );
        assert_eq!(
            fixture
                .runtime
                .observe_hook(&fixture.id, event, native, deadline())
                .unwrap_err()
                .to_string(),
            "AW service: audit_unavailable"
        );
        assert_eq!(fixture.records(), records);
    }
}

#[test]
fn incomplete_native_reservation_is_rejected_while_audit_remains_healthy() {
    let fixture = Fixture::new("qwenpaw");
    let (event, native) = fixture.callback(false, "id-failure");
    let next = fixture.runtime.next.swap(u64::MAX, Ordering::Relaxed);
    let first =
        fixture
            .runtime
            .observe_hook(&fixture.id, event.clone(), native.clone(), deadline());
    fixture.runtime.next.store(next, Ordering::Relaxed);
    assert_eq!(first.unwrap_err().to_string(), "AW service: id_exhausted");
    assert!(fixture.runtime.audit.healthy());
    assert_eq!(
        fixture
            .runtime
            .observe_hook(&fixture.id, event, native, deadline())
            .unwrap_err()
            .to_string(),
        "AW service: audit_unavailable"
    );
    assert!(fixture.runtime.audit.healthy());
    assert!(fixture.records().is_empty());
}

#[test]
fn completed_native_duplicate_requires_healthy_audit() {
    let fixture = Fixture::new("qwenpaw");
    let (event, native) = fixture.callback(false, "completed");
    fixture
        .runtime
        .observe_hook(&fixture.id, event.clone(), native.clone(), deadline())
        .unwrap();
    let records = fixture.records();
    assert!(fixture
        .runtime
        .audit
        .append(&"a".repeat(64), json!({"phase":"closed"}))
        .is_err());
    assert!(!fixture.runtime.audit.healthy());
    assert_eq!(
        fixture
            .runtime
            .observe_hook(&fixture.id, event, native, deadline())
            .unwrap_err()
            .to_string(),
        "AW service: audit_unavailable"
    );
    assert_eq!(fixture.records(), records);
}
