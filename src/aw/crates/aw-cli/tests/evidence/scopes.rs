//! Observation admission and task ownership exclude each other under the shared runtime locks.

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
fn owned_roots_allow_observation_and_reject_task_admission() {
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
        assert!(
            matches!(service.client.call(task(&fixture, &root), Instant::now() + TIMEOUT),
            Err(aw_service::Error::Rejected(code)) if code == "scope_parent_unavailable")
        );
        assert_eq!(
            service
                .client
                .call(Operation::Status, Instant::now() + TIMEOUT)
                .unwrap()["bindings"],
            1
        );
        release(&service, &root.instance_id);
        assert_empty(&service);
        fixture.assert_reaped();
    }
}

#[test]
fn preparing_and_active_task_scopes_exclude_observation() {
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
    for id in [&root.instance_id, &child.instance_id] {
        assert!(matches!(observe(&service, id),
            Err(aw_service::Error::Rejected(code)) if code == "scope_observation_unsupported"));
    }
    assert_eq!(
        source.paths.lock().unwrap().len(),
        paths,
        "rejected observation contacted the source"
    );
    release(&service, &child.instance_id);
    assert!(observe(&service, &root.instance_id).unwrap()["audit_key"].is_string());
    release(&service, &root.instance_id);
    assert_empty(&service);
    fixture.assert_reaped();
}
