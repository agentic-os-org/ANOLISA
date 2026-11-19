//! SDK extension negotiation, owner fencing and canonical native callback execution.

use super::*;
use crate::Availability;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../../sdk/tests/fixtures/contract.json"
    ))
    .unwrap()
}

fn command(f: &Fixture, key: &str, mode: Availability) -> RegistrationCommand {
    let mut value = serde_json::to_value(f.command(key)).unwrap();
    value["action"] = json!("register_sdk");
    value["sdk_version"] = fixture()["sdk_protocol"].clone();
    value["availability"] = json!(mode);
    // Public callbacks are validated by event contract, without an exact SDK version list.
    value["capabilities"]["version"] = json!("application-hooks/v1");
    serde_json::from_value(value).unwrap()
}

pub(super) fn task(f: &Fixture, key: &str) -> (RegistrationHandle, String) {
    let snapshot = f.call(command(f, key, Availability::Required)).unwrap();
    let h = handle(&snapshot);
    code(f.open(&h.binding.instance_id), "registration_not_ready");
    f.call(RegistrationCommand::Ready {
        handle: h.clone(),
        policy_revision: snapshot["policy_revision"].as_str().unwrap().into(),
    })
    .unwrap();
    let scope = f.open(&h.binding.instance_id).unwrap();
    let id = scope["instance_id"].as_str().unwrap().to_owned();
    f.runtime.scope_policy(&id).unwrap();
    f.runtime
        .acknowledge_scope(&id, Instant::now() + Duration::from_secs(5))
        .unwrap();
    (h, id)
}

fn callback(f: &Fixture, id: &str) -> Operation {
    let mut native = fixture()["native_before"].clone();
    native["cwd"] = json!(f.directory.path());
    Operation::SdkCallback {
        instance_id: id.into(),
        event: "tool.before".into(),
        step_id: "observe".into(),
        native_input: native,
        close: false,
    }
}

#[test]
fn sdk_negotiates_without_relaxing_the_old_registration_shape() {
    let f = Fixture::new();
    let mut value =
        serde_json::to_value(command(&f, "negotiation", Availability::Required)).unwrap();
    value["sdk_version"] = json!("aw-sdk/v0");
    code(
        f.call(serde_json::from_value(value.clone()).unwrap()),
        "sdk_protocol_version",
    );
    value["action"] = json!("register");
    assert!(serde_json::from_value::<RegistrationCommand>(value).is_err());
    let snapshot = f
        .call(command(&f, "negotiation", Availability::Required))
        .unwrap();
    assert_eq!(snapshot["sdk_protocol"], fixture()["sdk_protocol"]);
    assert_eq!(snapshot["availability"], "required");
    let recovered = f.query("negotiation");
    assert_eq!(snapshot["handle"], recovered["handle"]);
    code(
        f.call(command(&f, "negotiation", Availability::NativeFallback)),
        "registration_request_conflict",
    );
}

#[test]
fn fallback_rejects_required_events_and_accepts_explicit_optional_policy() {
    let f = Fixture::new();
    code(
        f.call(command(&f, "conflict", Availability::NativeFallback)),
        "sdk_required_policy_conflict",
    );
    let mut document: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    for event in document["spec"]["events"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        event["required"] = json!(false);
    }
    let mut cmd = command(&f, "optional", Availability::NativeFallback);
    if let RegistrationCommand::RegisterSdk { config, .. } = &mut cmd {
        *config = Some(serde_json::to_string(&document).unwrap());
    }
    assert_eq!(f.call(cmd).unwrap()["availability"], "native_fallback");
}

fn multi_target_fixture() -> Fixture {
    let mut f = Fixture::new();
    let mut document: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    document["spec"]["agents"]["other"] = json!({"adapter":"qwenpaw","argv":["/bin/true"]});
    document["spec"]["events"]["tool.before"]["match"] =
        json!({"tools":["native:qwenpaw:execute_shell_command"]});
    document["spec"]["events"]["tool.after"]["required"] = json!(false);
    f.runtime = Runtime::new(
        serde_json::to_vec(&document).unwrap(),
        &f.directory.path().join("targets-audit"),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    f
}

#[test]
fn fallback_registration_uses_the_candidate_and_actual_target() {
    let f = multi_target_fixture();
    let snapshot = f
        .call(command(
            &f,
            "foreign-required",
            Availability::NativeFallback,
        ))
        .unwrap();
    assert_eq!(snapshot["availability"], "native_fallback");
    for mode in [Availability::NativeFallback, Availability::Required] {
        let mut cmd = command(&f, &format!("other-{mode:?}"), mode);
        if let RegistrationCommand::RegisterSdk {
            target,
            capabilities,
            ..
        } = &mut cmd
        {
            *target = "other".into();
            capabilities.adapter = "qwenpaw".into();
            capabilities.entrypoint = "app".into();
        }
        if mode == Availability::NativeFallback {
            code(f.call(cmd), "sdk_required_policy_conflict");
        } else {
            f.call(cmd).unwrap();
        }
    }
    let mut candidate: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    candidate["spec"]["events"]["tool.before"]["match"] =
        json!({"tools":["native:qoder-sdk:Bash"]});
    let mut cmd = command(&f, "candidate-required", Availability::NativeFallback);
    if let RegistrationCommand::RegisterSdk { config, .. } = &mut cmd {
        *config = Some(serde_json::to_string(&candidate).unwrap());
    }
    code(f.call(cmd), "sdk_required_policy_conflict");
}

#[test]
fn sdk_callbacks_require_ready_owned_scopes_and_execute_once() {
    let f = Fixture::new();
    let (root, id) = task(&f, "callback");
    let peer = Process::read(std::process::id()).unwrap();
    code(
        f.runtime
            .authorize_registration_peer(&callback(&f, &root.binding.instance_id), Some(&peer))
            .map(|()| json!({})),
        "registration_requires_scope",
    );
    assert!(f
        .runtime
        .authorize_registration_peer(&callback(&f, &id), None)
        .is_err());
    f.runtime
        .authorize_registration_peer(&callback(&f, &id), Some(&peer))
        .unwrap();
    let result = f
        .runtime
        .handle(
            callback(&f, &id),
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result, fixture()["neutral_callback"]);
    assert!(f.directory.path().join("invoked").exists());
    fs::remove_file(f.directory.path().join("invoked")).unwrap();
    assert!(f
        .runtime
        .handle(
            callback(&f, &id),
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false)
        )
        .is_err());
    assert!(!f.directory.path().join("invoked").exists());
    f.call(RegistrationCommand::Release { handle: root })
        .unwrap();
}

#[test]
fn sdk_can_close_an_existing_event_after_bridge_expiry() {
    let mut f = Fixture::new();
    let mut document: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    let steps = document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap();
    let mut later = steps[0].clone();
    later["id"] = json!("later");
    steps.push(later);
    f.runtime = Runtime::new(
        serde_json::to_vec(&document).unwrap(),
        &f.directory.path().join("sdk-cleanup-audit"),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let (root, id) = task(&f, "cleanup");
    let root_id = &root.binding.instance_id;
    let peer = Process::read(std::process::id()).unwrap();
    let close = |id: &str| {
        let mut operation = callback(&f, id);
        if let Operation::SdkCallback { close, .. } = &mut operation {
            *close = true;
        }
        operation
    };
    let handle = |operation| {
        f.runtime.handle(
            operation,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
    };
    let unacknowledged = f.open(root_id).unwrap();
    code(
        handle(close(unacknowledged["instance_id"].as_str().unwrap())),
        "scope_unacknowledged",
    );
    code(handle(close("unknown")), "scope_unavailable");
    code(
        f.runtime
            .authorize_registration_peer(&close(root_id), Some(&peer))
            .map(|()| json!({})),
        "registration_requires_scope",
    );
    code(
        f.runtime
            .authorize_registration_peer(&close(&id), None)
            .map(|()| json!({})),
        "registration_peer_mismatch",
    );

    f.runtime.watch_scope(&id, std::process::id()).unwrap();
    handle(callback(&f, &id)).unwrap();
    assert_eq!(f.runtime.events.lock().unwrap().len(), 1);
    f.runtime.expire_scope_bridge(&id);
    code(handle(callback(&f, &id)), "scope_bridge_expired");
    f.runtime
        .authorize_registration_peer(&close(&id), Some(&peer))
        .unwrap();
    assert_eq!(
        handle(close(&id)).unwrap(),
        json!({"code":0,"stdout":[],"stderr":[]})
    );
    assert!(f.runtime.events.lock().unwrap().is_empty());

    f.runtime
        .owners
        .lock()
        .unwrap()
        .get(root_id)
        .unwrap()
        .cancelled
        .store(true, Ordering::Release);
    code(handle(close(&id)), "scope_closing");
}

#[test]
fn sdk_callback_cannot_replace_the_server_selected_step_or_native_identity() {
    let f = Fixture::new();
    let (_, id) = task(&f, "input");
    for invalid in ["step", "identity"] {
        let mut op = callback(&f, &id);
        if let Operation::SdkCallback {
            step_id,
            native_input,
            ..
        } = &mut op
        {
            if invalid == "step" {
                *step_id = "unadmitted".into();
            } else {
                native_input["cwd"] = json!("/not-the-owner-directory");
            }
        }
        assert!(f
            .runtime
            .handle(
                op,
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false)
            )
            .is_err());
    }
    assert!(!f.directory.path().join("invoked").exists());
}

#[test]
fn sdk_denial_and_provider_failure_remain_distinct_from_native_fallback() {
    for fail in [false, true] {
        let mut f = Fixture::new();
        let provider = f.directory.path().join("policy.py");
        fs::write(&provider, r#"import json, sys
request = json.load(sys.stdin)
reply = {"api_version": "aw-provider/v1alpha1", "request_id": request["request_id"], "status": "ok"}
if request["method"] == "describe":
    reply["operations"] = [{"name":"observe", "events":["tool.before"], "effects":["block"]}]
elif request["method"] == "invoke":
    if sys.argv[2] == "fail":
        sys.exit(42)
    reply.update(input_digest=request["input_digest"], effects=[{"type":"block", "reason_code":"sdk_policy"}])
json.dump(reply, sys.stdout)
"#).unwrap();
        let mut doc: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
        doc["spec"]["providers"]["policy"]["transport"]["argv"][2] = json!(provider);
        doc["spec"]["providers"]["policy"]["transport"]["argv"]
            .as_array_mut()
            .unwrap()
            .push(json!(if fail { "fail" } else { "deny" }));
        doc["spec"]["events"]["tool.before"]["steps"][0]["effects"] = json!(["block"]);
        f.runtime = Runtime::new(
            serde_json::to_vec(&doc).unwrap(),
            &f.directory.path().join("sdk-audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut cmd = command(&f, "decision", Availability::Required);
        if let RegistrationCommand::RegisterSdk { capabilities, .. } = &mut cmd {
            capabilities
                .events
                .insert("tool.before".into(), vec!["observe".into(), "block".into()]);
        }
        let registered = f.call(cmd).unwrap();
        let h = handle(&registered);
        f.call(RegistrationCommand::Ready {
            handle: h.clone(),
            policy_revision: registered["policy_revision"].as_str().unwrap().into(),
        })
        .unwrap();
        let scope = f.open(&h.binding.instance_id).unwrap();
        let id = scope["instance_id"].as_str().unwrap();
        f.runtime.scope_policy(id).unwrap();
        f.runtime
            .acknowledge_scope(id, Instant::now() + Duration::from_secs(5))
            .unwrap();
        let result = f.runtime.handle(
            callback(&f, id),
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        );
        if fail {
            code(result, "sdk_step_failed");
        } else {
            assert_eq!(result.unwrap()["code"], 2);
        }
    }
}

#[test]
fn fallback_updates_reject_required_policy_and_busy_scopes() {
    let f = Fixture::new();
    let mut doc: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    for event in doc["spec"]["events"].as_object_mut().unwrap().values_mut() {
        event["required"] = json!(false);
    }
    let mut cmd = command(&f, "update", Availability::NativeFallback);
    if let RegistrationCommand::RegisterSdk { config, .. } = &mut cmd {
        *config = Some(serde_json::to_string(&doc).unwrap());
    }
    let snapshot = f.call(cmd).unwrap();
    let h = handle(&snapshot);
    let id = &h.binding.instance_id;
    let pid = std::process::id();
    f.runtime.enable_reload(id).unwrap();
    f.call(RegistrationCommand::Ready {
        handle: h.clone(),
        policy_revision: snapshot["policy_revision"].as_str().unwrap().into(),
    })
    .unwrap();
    f.runtime.associate_process(id, pid).unwrap();
    f.runtime
        .lifecycle(id, pid, crate::LifecyclePhase::Ready, "")
        .unwrap();
    let update = |document: &Value| {
        let request = crate::Request {
            api_version: crate::VERSION.into(),
            identity: Some(f.runtime.identity.clone()),
            deadline_ns: crate::deadline::encode(Instant::now() + Duration::from_secs(5)).unwrap(),
            operation: Operation::UpdatePolicy {
                instance_id: id.clone(),
                expected_revision: snapshot["policy_revision"].as_str().unwrap().into(),
                config: serde_json::to_string(document).unwrap(),
            },
        };
        // SAFETY: geteuid only reads this test process's effective UID.
        f.runtime.update_policy(
            &request,
            (unsafe { libc::geteuid() }, pid as i32),
            &Arc::new(AtomicBool::new(false)),
        )
    };
    f.runtime
        .lifecycle(id, pid, crate::LifecyclePhase::RequestStart, "task")
        .unwrap();
    let scope = f.open(id).unwrap();
    assert!(matches!(update(&doc), Err(Error::Attempt { code, .. }) if code == "busy"));
    f.runtime
        .release_instance(
            scope["instance_id"].as_str().unwrap(),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    f.runtime
        .lifecycle(id, pid, crate::LifecyclePhase::RequestEnd, "task")
        .unwrap();
    doc["spec"]["events"]["tool.before"]["required"] = json!(true);
    assert!(
        matches!(update(&doc), Err(Error::Attempt { code, .. }) if code == "sdk_required_policy_conflict")
    );
    assert_eq!(
        f.query("update")["policy_revision"],
        snapshot["policy_revision"]
    );
}

#[test]
fn fallback_updates_project_the_candidate_and_preserve_rejected_policy() {
    let f = multi_target_fixture();
    let snapshot = f
        .call(command(&f, "target-update", Availability::NativeFallback))
        .unwrap();
    let h = handle(&snapshot);
    let id = &h.binding.instance_id;
    let pid = std::process::id();
    f.runtime.enable_reload(id).unwrap();
    f.call(RegistrationCommand::Ready {
        handle: h.clone(),
        policy_revision: snapshot["policy_revision"].as_str().unwrap().into(),
    })
    .unwrap();
    f.runtime.associate_process(id, pid).unwrap();
    f.runtime
        .lifecycle(id, pid, crate::LifecyclePhase::Ready, "")
        .unwrap();
    let update = |candidate: &Value, revision: &str| {
        let request = crate::Request {
            api_version: crate::VERSION.into(),
            identity: Some(f.runtime.identity.clone()),
            deadline_ns: crate::deadline::encode(Instant::now() + Duration::from_secs(5)).unwrap(),
            operation: Operation::UpdatePolicy {
                instance_id: id.clone(),
                expected_revision: revision.into(),
                config: serde_json::to_string(candidate).unwrap(),
            },
        };
        // SAFETY: geteuid only reads this test process's effective UID.
        f.runtime.update_policy(
            &request,
            (unsafe { libc::geteuid() }, pid as i32),
            &Arc::new(AtomicBool::new(false)),
        )
    };
    let mut accepted: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    accepted["spec"]["events"]["tool.before"]["budget_ms"] = json!(4000);
    let result = update(&accepted, snapshot["policy_revision"].as_str().unwrap()).unwrap();
    let revision = result["policy_revision"].as_str().unwrap();
    assert_ne!(result["policy_revision"], snapshot["policy_revision"]);

    let mut rejected = accepted.clone();
    rejected["spec"]["events"]["tool.before"]["match"] = json!({"tools":["native:qoder-sdk:Bash"]});
    assert!(
        matches!(update(&rejected, revision), Err(Error::Attempt { code, .. }) if code == "sdk_required_policy_conflict")
    );
    {
        let instances = f.runtime.instances.lock().unwrap();
        let active = instances.get(id).unwrap().as_ref().unwrap();
        assert_eq!(active.host.revision(), revision);
        assert_eq!(active.host.configuration().as_value(), &accepted);
        assert_eq!(active.bytes, serde_json::to_vec(&accepted).unwrap());
    }
    rejected["spec"]["events"]["tool.before"]["required"] = json!(false);
    let optional = update(&rejected, revision).unwrap();
    assert_ne!(optional["policy_revision"], revision);
    let instances = f.runtime.instances.lock().unwrap();
    let active = instances.get(id).unwrap().as_ref().unwrap();
    assert_eq!(
        active.host.revision(),
        optional["policy_revision"].as_str().unwrap()
    );
    assert_eq!(active.host.configuration().as_value(), &rejected);
}

#[test]
fn sdk_callbacks_reuse_cached_routes_and_adopt_resident_policy_updates() {
    let f = Fixture::new();
    let (root, id) = task(&f, "cached-routes");
    let root_id = &root.binding.instance_id;
    let initial_routes = {
        let instances = f.runtime.instances.lock().unwrap();
        Arc::clone(instances[&id].as_ref().unwrap().routes.as_ref().unwrap())
    };
    // Unknown event and step codes come directly from the cached PreparedRoutes.
    let mut missing_event = callback(&f, &id);
    if let Operation::SdkCallback { event, .. } = &mut missing_event {
        *event = "input.submit".into();
    }
    code(
        f.runtime.handle(
            missing_event,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        ),
        "sdk_event_unavailable",
    );
    let mut missing_step = callback(&f, &id);
    if let Operation::SdkCallback { step_id, .. } = &mut missing_step {
        *step_id = "retired".into();
    }
    code(
        f.runtime.handle(
            missing_step,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        ),
        "sdk_step_unavailable",
    );
    f.runtime
        .handle(
            callback(&f, &id),
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
    {
        let mut instances = f.runtime.instances.lock().unwrap();
        let scope = instances.get_mut(&id).unwrap().as_mut().unwrap();
        assert!(Arc::ptr_eq(scope.routes.as_ref().unwrap(), &initial_routes));
        scope.routes = None;
    }
    code(f.runtime.scope_policy(&id), "native_policy_unsupported");
    code(
        f.runtime.handle(
            callback(&f, &id),
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        ),
        "native_policy_unsupported",
    );
    f.runtime
        .release_instance(&id, Instant::now() + Duration::from_secs(5))
        .unwrap();

    // Hot policy activation refreshes cached routes on the root and subsequent Scopes.
    let pid = std::process::id();
    f.runtime.enable_reload(root_id).unwrap();
    f.runtime.associate_process(root_id, pid).unwrap();
    f.runtime
        .lifecycle(root_id, pid, crate::LifecyclePhase::Ready, "")
        .unwrap();
    let expected = f.query("cached-routes")["policy_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut updated_doc: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    updated_doc["spec"]["events"]["tool.before"]["steps"][0]["id"] = json!("observe-v2");
    let request = crate::Request {
        api_version: crate::VERSION.into(),
        identity: Some(f.runtime.identity.clone()),
        deadline_ns: crate::deadline::encode(Instant::now() + Duration::from_secs(5)).unwrap(),
        operation: Operation::UpdatePolicy {
            instance_id: root_id.clone(),
            expected_revision: expected,
            config: serde_json::to_string(&updated_doc).unwrap(),
        },
    };
    // SAFETY: geteuid only reads this test process's effective UID.
    f.runtime
        .update_policy(
            &request,
            (unsafe { libc::geteuid() }, pid as i32),
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    f.runtime
        .lifecycle(root_id, pid, crate::LifecyclePhase::RequestStart, "turn-2")
        .unwrap();
    let next = f.open(root_id).unwrap();
    let next_id = next["instance_id"].as_str().unwrap();
    f.runtime.scope_policy(next_id).unwrap();
    f.runtime
        .acknowledge_scope(next_id, Instant::now() + Duration::from_secs(5))
        .unwrap();
    code(
        f.runtime.handle(
            callback(&f, next_id),
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        ),
        "sdk_step_unavailable",
    );
    let mut updated_call = callback(&f, next_id);
    if let Operation::SdkCallback { step_id, .. } = &mut updated_call {
        *step_id = "observe-v2".into();
    }
    assert_eq!(
        f.runtime
            .handle(
                updated_call,
                Instant::now() + Duration::from_secs(5),
                &AtomicBool::new(false),
            )
            .unwrap(),
        fixture()["neutral_callback"]
    );
    f.runtime
        .release_instance(next_id, Instant::now() + Duration::from_secs(5))
        .unwrap();
    f.runtime
        .lifecycle(root_id, pid, crate::LifecyclePhase::RequestEnd, "turn-2")
        .unwrap();
    f.call(RegistrationCommand::Release { handle: root })
        .unwrap();
}
