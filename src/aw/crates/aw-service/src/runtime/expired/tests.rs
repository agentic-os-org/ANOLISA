//! A queued RPC must survive receiver loss while an active entry becomes an expiry handle.

use super::*;
use crate::{
    runtime::{evidence, EventEntry},
    Capabilities, Operation,
};
use std::{thread, time::Duration};

#[test]
fn queued_requests_survive_expiry_retirement() {
    for invoking in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(
            include_bytes!("../../../../aw-config/examples/aw.minimal.yaml").to_vec(),
            &directory.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let deadline = || Instant::now() + Duration::from_secs(5);
        let binding = runtime
            .bind(
                "qoder".into(),
                Capabilities {
                    adapter: "qoder".into(),
                    version: "fixture".into(),
                    entrypoint: "fixture".into(),
                    events: BTreeMap::from([
                        ("tool.before".into(), vec!["observe".into()]),
                        ("tool.after".into(), vec!["observe".into()]),
                    ]),
                    failure_actions: Default::default(),
                },
                aw_host::ProcessContext {
                    cwd: directory.path().into(),
                    environment: BTreeMap::new(),
                    stderr_bytes: 1024,
                },
                deadline(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let instance = binding["instance_id"].as_str().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        runtime.events.lock().unwrap().insert(
            "event".into(),
            EventEntry {
                instance_id: instance.into(),
                sender,
                cancelled: Arc::clone(&cancelled),
                context: Arc::new(evidence::Cell::default()),
            },
        );
        thread::scope(|threads| {
            let calling = threads.spawn(|| {
                runtime.handle(
                    if invoking {
                        Operation::InvokeStep {
                            event_id: "event".into(),
                            instance_id: instance.into(),
                            step_id: "check".into(),
                            native_environment: None,
                        }
                    } else {
                        Operation::CloseEvent {
                            event_id: "event".into(),
                            instance_id: instance.into(),
                        }
                    },
                    deadline(),
                    &AtomicBool::new(false),
                )
            });
            let job = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
            // Hold the request's reply until the registry handoff is complete, then
            // drop it just as an event worker exiting without reading the queue does.
            {
                let mut events = runtime.events.lock().unwrap();
                let mut expired = runtime.expired.lock().unwrap();
                expired.insert(
                    "event".into(),
                    ExpiredEvent {
                        instance_id: instance.into(),
                        cancelled,
                        automatic: false,
                        execution: None,
                        replies: BTreeMap::from([(
                            "check".into(),
                            json!({"status":"error","failure_action":"block","effects":[]}),
                        )]),
                        evidence: json!({"status":"expired"}),
                    },
                );
                events.remove("event");
            }
            drop(job);
            let result = calling.join().unwrap().unwrap();
            assert_eq!(
                result[if invoking { "failure_action" } else { "closed" }],
                if invoking {
                    json!("block")
                } else {
                    json!(true)
                }
            );
        });
        if invoking {
            runtime.close_event("event", instance, deadline()).unwrap();
        }
        assert!(runtime.expired.lock().unwrap().is_empty());
        runtime.maintain(true).unwrap();
    }
}
