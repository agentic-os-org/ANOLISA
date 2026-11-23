//! Task-owned observation preserves source claims and drains independently of its resident root.

use super::*;
use std::{collections::BTreeMap, fs, thread};

fn own(service: &Running, root: &aw_service::Binding) {
    service
        .client
        .call(
            Operation::OwnInstance {
                instance_id: root.instance_id.clone(),
                pid: std::process::id(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
}

fn task(fixture: &Fixture, root: &aw_service::Binding) -> Operation {
    Operation::OpenScope {
        parent_id: root.instance_id.clone(),
        pid: std::process::id(),
        cwd: fixture.0.to_str().unwrap().into(),
        environment: BTreeMap::new(),
    }
}

fn observe(service: &Running, id: &str) -> Result<Value, aw_service::Error> {
    service.client.call(
        Operation::StartObservation {
            instance_id: id.into(),
        },
        Instant::now() + TIMEOUT,
    )
}

fn release(service: &Running, id: &str) {
    service
        .client
        .call(
            Operation::ReleaseInstance {
                instance_id: id.into(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
}

fn assert_empty(service: &Running) {
    let status = service
        .client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(status["bindings"], 0);
    assert_eq!(status["events"], 0);
    assert_eq!(status["observations"], json!([]));
}

#[test]
fn owned_roots_keep_their_run_metadata_while_tasks_observe_independently() {
    for own_first in [false, true] {
        let fixture = Fixture::new();
        let source = Source::new(&fixture, trajectory(0));
        let mut document = fixture.document();
        source.configure(&mut document);
        let service = Running::start(&fixture, &document);
        let root = fixture.bind(&service.client);
        if own_first {
            own(&service, &root);
        }
        let observation = observe(&service, &root.instance_id).unwrap();
        assert!(observation["audit_key"].is_string());
        if !own_first {
            own(&service, &root);
        }
        assert_eq!(
            observe(&service, &root.instance_id).unwrap()["audit_key"],
            observation["audit_key"]
        );
        let child: aw_service::Binding = serde_json::from_value(
            service
                .client
                .call(task(&fixture, &root), Instant::now() + TIMEOUT)
                .unwrap(),
        )
        .unwrap();
        assert!(
            observe(&service, &child.instance_id).is_err(),
            "unacknowledged scope observed"
        );
        service
            .client
            .call(
                Operation::AcknowledgeScope {
                    instance_id: child.instance_id.clone(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        let child_observation = observe(&service, &child.instance_id).unwrap();
        assert!(child_observation["audit_key"].is_string());
        assert_ne!(child_observation["audit_key"], observation["audit_key"]);
        release(&service, &root.instance_id);
        assert_empty(&service);
        fixture.assert_reaped();
    }
}

#[test]
fn preparing_scopes_are_closed_to_observation_until_acknowledgement() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(0));
    let mut document = fixture.document();
    source.configure(&mut document);
    let service = Running::start(&fixture, &document);
    let root = fixture.bind(&service.client);
    own(&service, &root);
    fs::write(fixture.0.join("describe-gate"), "held").unwrap();
    let count = fixture.calls("describe").len();
    let paths = source.paths.lock().unwrap().len();
    let child: aw_service::Binding = thread::scope(|threads| {
        let pending = threads.spawn(|| {
            service
                .client
                .call(task(&fixture, &root), Instant::now() + TIMEOUT)
        });
        fixture.wait_for_calls("describe", count + 1);
        assert!(matches!(observe(&service, &root.instance_id),
            Err(aw_service::Error::Rejected(code)) if code == "scope_observation_unsupported"));
        fs::write(fixture.0.join("prepare-release"), "ready").unwrap();
        serde_json::from_value(pending.join().unwrap().unwrap()).unwrap()
    });
    service
        .client
        .call(
            Operation::AcknowledgeScope {
                instance_id: child.instance_id.clone(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    assert!(matches!(observe(&service, &root.instance_id),
        Err(aw_service::Error::Rejected(code)) if code == "scope_observation_unsupported"));
    assert!(observe(&service, &child.instance_id).unwrap()["audit_key"].is_string());
    assert_eq!(
        source.paths.lock().unwrap().len(),
        paths + 1,
        "only the acknowledged child may probe its source"
    );
    release(&service, &child.instance_id);
    assert!(observe(&service, &root.instance_id).unwrap()["audit_key"].is_string());
    release(&service, &root.instance_id);
    assert_empty(&service);
    fixture.assert_reaped();
}

#[test]
fn parallel_task_observations_do_not_share_source_sessions_and_release_claims() {
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(1));
    let mut document = fixture.document();
    source.configure(&mut document);
    let service = Running::start(&fixture, &document);
    let root = fixture.bind(&service.client);
    own(&service, &root);
    let children: Vec<aw_service::Binding> = (0..2)
        .map(|_| {
            let child: aw_service::Binding = serde_json::from_value(
                service
                    .client
                    .call(task(&fixture, &root), Instant::now() + TIMEOUT)
                    .unwrap(),
            )
            .unwrap();
            service
                .client
                .call(
                    Operation::AcknowledgeScope {
                        instance_id: child.instance_id.clone(),
                    },
                    Instant::now() + TIMEOUT,
                )
                .unwrap();
            child
        })
        .collect();
    let first = hook(
        &service.client,
        &children[0],
        native_event(&children[0], "tool.after"),
    );
    let first_snapshot = snapshot(&service.client, &first);
    assert!(!gap(&first_snapshot, "source_session_conflict"));
    assert!(first_snapshot["evidence"].as_array().unwrap().len() > 1);
    assert_eq!(
        first_snapshot["subject"]["instance_id"],
        children[0].instance_id
    );
    let second = hook(
        &service.client,
        &children[1],
        native_event(&children[1], "tool.after"),
    );
    let second_snapshot = snapshot(&service.client, &second);
    assert!(gap(&second_snapshot, "source_session_conflict"));
    assert!(!second_snapshot.to_string().contains(HISTORY));
    close(&service.client, &first);
    close(&service.client, &second);
    release(&service, &children[0].instance_id);
    let mut resumed = native_event(&children[1], "tool.before");
    resumed["tool"]["call_id"] = json!("resumed-call");
    resumed["native"]["tool_use_id"] = json!("resumed-call");
    let resumed = hook(&service.client, &children[1], resumed);
    let resumed_snapshot = snapshot(&service.client, &resumed);
    assert!(!gap(&resumed_snapshot, "source_session_conflict"));
    assert!(resumed_snapshot["evidence"].as_array().unwrap().len() > 1);
    assert!(gap(&resumed_snapshot, "process_identity_unavailable"));
    close(&service.client, &resumed);
    release(&service, &root.instance_id);
    assert_empty(&service);
    fixture.assert_reaped();
}

#[test]
fn worker_exit_drains_task_observation_and_reclaims_its_scope() {
    struct Worker(std::process::Child);
    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new();
    let source = Source::new(&fixture, trajectory(1));
    *source.delay.lock().unwrap() = std::time::Duration::from_secs(2);
    let mut document = fixture.document();
    source.configure(&mut document);
    let service = Running::start(&fixture, &document);
    let root = fixture.bind(&service.client);
    own(&service, &root);
    let mut worker = Worker(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap(),
    );
    let mut opening = task(&fixture, &root);
    if let Operation::OpenScope { pid, .. } = &mut opening {
        *pid = worker.0.id();
    }
    let child: aw_service::Binding = serde_json::from_value(
        service
            .client
            .call(opening, Instant::now() + TIMEOUT)
            .unwrap(),
    )
    .unwrap();
    service
        .client
        .call(
            Operation::AcknowledgeScope {
                instance_id: child.instance_id.clone(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    let metadata = observe(&service, &child.instance_id).unwrap();
    let _event = hook(&service.client, &child, native_event(&child, "tool.before"));
    source.wait_for(2);
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state = service.client.call(Operation::Status, deadline).unwrap();
        if state["bindings"] == 1 && state["observations"] == json!([]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dead worker retained observation: {state}"
        );
        thread::sleep(std::time::Duration::from_millis(10));
    }
    let closed = audit(&service.client, metadata["audit_key"].as_str().unwrap());
    assert_eq!(closed["terminal"], true);
    assert!(!closed.to_string().contains(HISTORY));
    assert!(observe(&service, &child.instance_id).is_err());
    release(&service, &root.instance_id);
    assert_empty(&service);
    fixture.assert_reaped();
}
