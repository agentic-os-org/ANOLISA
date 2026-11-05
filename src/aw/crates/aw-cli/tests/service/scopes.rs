//! Task scopes release independent contexts and fail closed across owner death and teardown.

use super::*;
use aw_service::Binding;

fn own(service: &Running, binding: &Binding) {
    service
        .client
        .call(
            Operation::OwnInstance {
                instance_id: binding.instance_id.clone(),
                pid: std::process::id(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
}

fn request(fixture: &Fixture, parent: &Binding, marker: &str) -> Operation {
    Operation::OpenScope {
        parent_id: parent.instance_id.clone(),
        pid: std::process::id(),
        cwd: fixture.0.to_str().unwrap().into(),
        environment: std::collections::BTreeMap::from([(
            "AW_SERVICE_MARKER".into(),
            marker.into(),
        )]),
    }
}

fn scope(fixture: &Fixture, service: &Running, parent: &Binding, marker: &str) -> Binding {
    let binding: Binding = serde_json::from_value(
        service
            .client
            .call(request(fixture, parent, marker), Instant::now() + TIMEOUT)
            .unwrap(),
    )
    .unwrap();
    service
        .client
        .call(
            Operation::AcknowledgeScope {
                instance_id: binding.instance_id.clone(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    binding
}

#[test]
fn task_scopes_isolate_context_reject_nested_and_reclaim_capacity() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let parent = fixture.bind(&service.client);
    assert!(service
        .client
        .call(
            request(&fixture, &parent, "unowned"),
            Instant::now() + TIMEOUT
        )
        .is_err());
    own(&service, &parent);
    let first = scope(&fixture, &service, &parent, "first");
    let second = scope(&fixture, &service, &parent, "second");
    assert_ne!(first.instance_id, second.instance_id);
    assert!(service
        .client
        .call(
            request(&fixture, &first, "nested"),
            Instant::now() + TIMEOUT
        )
        .is_err());
    for (binding, marker) in [(&first, "first"), (&second, "second")] {
        let handle = open(
            &service.client,
            binding,
            event(binding, "tool.before", "allow"),
        );
        service
            .client
            .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
            .unwrap();
        close(&service.client, &handle);
        assert!(fixture
            .calls("invoke")
            .iter()
            .any(|call| call["environment"]["AW_SERVICE_MARKER"] == marker));
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: binding.instance_id.clone(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        assert!(service
            .client
            .call(
                Operation::OpenEvent {
                    instance_id: binding.instance_id.clone(),
                    event: event(binding, "tool.before", "allow")
                },
                Instant::now() + TIMEOUT
            )
            .is_err());
    }
    // More sequential tasks than the service's concurrent binding capacity.
    for _ in 0..20 {
        let child = scope(&fixture, &service, &parent, "sequential");
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: child.instance_id,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
    }
    let _held = scope(&fixture, &service, &parent, "held");
    service
        .client
        .call(
            Operation::ReleaseInstance {
                instance_id: parent.instance_id.clone(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    let status = service
        .client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(status["bindings"], 0);
    assert_eq!(status["events"], 0);
    assert!(service
        .client
        .call(
            request(&fixture, &parent, "stale"),
            Instant::now() + TIMEOUT
        )
        .is_err());
    fixture.assert_reaped();
}

#[test]
fn closing_parent_cancels_inflight_scope_preparation() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let parent = fixture.bind(&service.client);
    own(&service, &parent);
    fs::write(fixture.0.join("describe-gate"), "held").unwrap();
    let count = fixture.calls("describe").len();
    thread::scope(|threads| {
        let pending = threads.spawn(|| {
            service.client.call(
                request(&fixture, &parent, "pending"),
                Instant::now() + TIMEOUT,
            )
        });
        fixture.wait_for_calls("describe", count + 1);
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: parent.instance_id.clone(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        assert!(pending.join().unwrap().is_err());
    });
    assert_eq!(
        service
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap()["bindings"],
        0
    );
    fixture.assert_reaped();
}

#[test]
fn dead_worker_scope_is_reaped_without_releasing_live_parent() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let parent = fixture.bind(&service.client);
    own(&service, &parent);
    let mut worker = Command::new("sleep").arg("30").spawn().unwrap();
    let mut operation = request(&fixture, &parent, "worker");
    if let Operation::OpenScope { pid, .. } = &mut operation {
        *pid = worker.id();
    }
    let result = service.client.call(operation, Instant::now() + TIMEOUT);
    worker.kill().unwrap();
    worker.wait().unwrap();
    let child: Binding = serde_json::from_value(result.unwrap()).unwrap();
    assert!(service
        .client
        .call(
            Operation::OpenEvent {
                instance_id: child.instance_id.clone(),
                event: event(&child, "tool.before", "allow")
            },
            Instant::now() + TIMEOUT
        )
        .is_err());
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let status = service.client.call(Operation::Status, deadline).unwrap();
        if status["bindings"] == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dead worker retained its binding"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let next = scope(&fixture, &service, &parent, "replacement-worker");
    assert_ne!(child.instance_id, next.instance_id);
    service
        .client
        .call(
            Operation::ReleaseInstance {
                instance_id: parent.instance_id,
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    fixture.assert_reaped();
}

#[test]
fn disconnected_scope_preparation_releases_its_reservation() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let parent = fixture.bind(&service.client);
    own(&service, &parent);
    fs::write(fixture.0.join("describe-gate"), "held").unwrap();
    let count = fixture.calls("describe").len();
    let cancelled = AtomicBool::new(false);
    thread::scope(|threads| {
        let pending = threads.spawn(|| {
            service.client.call_cancellable(
                request(&fixture, &parent, "disconnected"),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
        });
        fixture.wait_for_calls("describe", count + 1);
        cancelled.store(true, Ordering::Release);
        assert!(pending.join().unwrap().is_err());
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if service.client.call(Operation::Status, deadline).unwrap()["bindings"] == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "disconnected preparation retained its slot"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fixture.assert_reaped();
}

#[test]
fn scope_capacity_and_invalid_owner_fail_without_extra_bindings() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["daemon"]["limits"] = json!({"bindings": 24});
    let service = Running::start(&fixture, &document);
    let parent = fixture.bind(&service.client);
    own(&service, &parent);
    for operation in [
        Operation::OwnInstance {
            instance_id: parent.instance_id.clone(),
            pid: std::process::id(),
        },
        Operation::EnableReload {
            instance_id: parent.instance_id.clone(),
        },
    ] {
        assert!(service
            .client
            .call(operation, Instant::now() + TIMEOUT)
            .is_err());
    }
    let mut invalid = request(&fixture, &parent, "invalid");
    if let Operation::OpenScope { pid, .. } = &mut invalid {
        *pid = 0;
    }
    assert!(service
        .client
        .call(invalid, Instant::now() + TIMEOUT)
        .is_err());
    for _ in 0..23 {
        scope(&fixture, &service, &parent, "capacity");
    }
    assert!(service
        .client
        .call(
            request(&fixture, &parent, "overflow"),
            Instant::now() + TIMEOUT
        )
        .unwrap_err()
        .to_string()
        .contains("binding_limit"));
    service
        .client
        .call(
            Operation::ReleaseInstance {
                instance_id: parent.instance_id,
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    assert_eq!(
        service
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap()["bindings"],
        0
    );
    fixture.assert_reaped();
}

#[test]
fn parent_close_cancels_active_child_steps_and_rejects_late_calls() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let parent = fixture.bind(&service.client);
    own(&service, &parent);
    let child = scope(&fixture, &service, &parent, "busy");
    let handle = open(
        &service.client,
        &child,
        event(&child, "tool.before", "wait"),
    );
    thread::scope(|threads| {
        let pending = threads.spawn(|| {
            service
                .client
                .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        });
        fixture.wait_for_calls("invoke", 1);
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: parent.instance_id,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        let result = pending.join().unwrap();
        assert!(result.is_err() || result.unwrap()["status"] != "ok");
    });
    assert!(service
        .client
        .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        .is_err());
    let status = service
        .client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(status["bindings"], 0);
    assert_eq!(status["events"], 0);
    fixture.assert_reaped();
}

#[test]
fn launcher_death_drains_all_owned_scopes_and_rejects_old_handles() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let parent = fixture.bind(&service.client);
    let mut launcher = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = launcher.id();
    let owned = service.client.call(
        Operation::OwnInstance {
            instance_id: parent.instance_id.clone(),
            pid,
        },
        Instant::now() + TIMEOUT,
    );
    let mut operation = request(&fixture, &parent, "launcher");
    if let Operation::OpenScope { pid: owner, .. } = &mut operation {
        *owner = pid;
    }
    let opened = service.client.call(operation, Instant::now() + TIMEOUT);
    launcher.kill().unwrap();
    launcher.wait().unwrap();
    owned.unwrap();
    let child: Binding = serde_json::from_value(opened.unwrap()).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if service.client.call(Operation::Status, deadline).unwrap()["bindings"] == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dead launcher retained task scopes"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(service
        .client
        .call(
            Operation::OpenEvent {
                instance_id: child.instance_id.clone(),
                event: event(&child, "tool.before", "allow"),
            },
            Instant::now() + TIMEOUT
        )
        .is_err());
    fixture.assert_reaped();
}

#[test]
fn unacknowledged_handoff_expires_and_cannot_invoke_or_revive() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let parent = fixture.bind(&service.client);
    own(&service, &parent);
    let child: Binding = serde_json::from_value(
        service
            .client
            .call(
                request(&fixture, &parent, "lost-response"),
                Instant::now() + TIMEOUT,
            )
            .unwrap(),
    )
    .unwrap();
    assert!(service
        .client
        .call(
            Operation::OpenEvent {
                instance_id: child.instance_id.clone(),
                event: event(&child, "tool.before", "allow"),
            },
            Instant::now() + TIMEOUT
        )
        .unwrap_err()
        .to_string()
        .contains("scope_unacknowledged"));
    assert!(fixture.calls("invoke").is_empty());
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        if service.client.call(Operation::Status, deadline).unwrap()["bindings"] == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "lost response retained a task in the live host"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(service
        .client
        .call(
            Operation::AcknowledgeScope {
                instance_id: child.instance_id
            },
            Instant::now() + TIMEOUT
        )
        .is_err());
    let active = scope(&fixture, &service, &parent, "acknowledged");
    assert!(service
        .client
        .call(
            Operation::AcknowledgeScope {
                instance_id: active.instance_id
            },
            Instant::now() + TIMEOUT
        )
        .is_err());
    service
        .client
        .call(
            Operation::ReleaseInstance {
                instance_id: parent.instance_id,
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    fixture.assert_reaped();
}
