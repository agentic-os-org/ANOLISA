//! Installed Provider files remain protected through prepared snapshot ownership.

use crate::{capabilities, event, Fixture, Host, Method, TIMEOUT};
use aw_core::admission::AdmissionEngine;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Instant,
};

fn installed_provider(fixture: &Fixture) -> (Value, PathBuf) {
    let prefix = fixture.0.join("installed");
    fs::create_dir_all(prefix.join(".local-packages")).unwrap();
    for path in [&prefix, &prefix.join(".local-packages")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(prefix.join(".local-install.lock"), []).unwrap();
    fs::set_permissions(
        prefix.join(".local-install.lock"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let script = prefix.join("provider.py");
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider.py"),
        &script,
    )
    .unwrap();
    let mut document = fixture.document();
    document["spec"]["providers"]["policy"]["transport"]["argv"][2] = json!(script);
    (document, prefix)
}

#[test]
fn prepared_hosts_retain_managed_files_until_the_last_snapshot_drops() {
    let fixture = Fixture::private();
    let (document, prefix) = installed_provider(&fixture);
    let host = Arc::new(fixture.prepare(&document));
    let in_flight_snapshot = Arc::clone(&host);
    let writer = File::open(prefix.join(".local-install.lock")).unwrap();
    assert!(writer.try_lock().is_err());
    drop(host);
    assert!(writer.try_lock().is_err());
    drop(in_flight_snapshot);
    writer.try_lock().unwrap();
    fixture.assert_reaped();
}

#[test]
fn failed_and_cancelled_preparation_release_managed_files() {
    for cancel_during_discovery in [false, true] {
        let fixture = Fixture::private();
        let (mut document, prefix) = installed_provider(&fixture);
        document["spec"]["providers"]["policy"]["transport"]["argv"][4] =
            json!(if cancel_during_discovery {
                "slow-preparation"
            } else {
                "describe-nonzero"
            });
        let cancelled = AtomicBool::new(false);
        let bytes = serde_json::to_vec(&document).unwrap();
        let result = thread::scope(|scope| {
            let worker = scope.spawn(|| {
                Host::prepare(
                    &bytes,
                    "target",
                    capabilities(),
                    fixture.context(),
                    Instant::now() + TIMEOUT,
                    &cancelled,
                )
            });
            if cancel_during_discovery {
                let ready = fixture.wait_for_calls("describe", 1, Instant::now() + TIMEOUT);
                let writer = File::open(prefix.join(".local-install.lock")).unwrap();
                let held = writer.try_lock().is_err();
                cancelled.store(true, Ordering::Release);
                let result = worker.join().unwrap();
                assert!(ready && held);
                result
            } else {
                worker.join().unwrap()
            }
        });
        assert!(result.is_err());
        File::open(prefix.join(".local-install.lock"))
            .unwrap()
            .try_lock()
            .unwrap();
        fixture.assert_reaped();
    }
}

#[test]
fn mutation_lock_prevents_any_provider_discovery_process() {
    let fixture = Fixture::private();
    let (document, prefix) = installed_provider(&fixture);
    let writer = File::open(prefix.join(".local-install.lock")).unwrap();
    writer.try_lock().unwrap();
    let result = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(aw_host::Error::PackageLease(aw_exec::LeaseError::Busy(_)))
    ));
    assert!(fixture.calls("describe").is_empty());
    fixture.assert_reaped();
}

#[test]
fn core_preflight_discover_admit_and_from_admitted_match_prepare_execution() {
    let fixture = Fixture::private();
    let (mut document, prefix) = installed_provider(&fixture);
    document["spec"]["providers"]["raw"] = json!({
        "protocol": "native-hook/v1alpha1", "config": {}, "timeout_ms": 1000,
        "max_output_bytes": 1024,
        "transport": {"type": "stdio", "location": "agent", "argv": ["/no-discovery-for-native"]}
    });
    document["spec"]["providers"]["boolean"] = json!({
        "protocol": aw_config::HOOK_PROTOCOL, "config": {}, "timeout_ms": 1000,
        "max_output_bytes": 32,
        "transport": {"type": "stdio", "location": "agent", "argv": ["/no-discovery-for-boolean"]}
    });
    document["spec"]["events"]["tool.after"]["steps"] = json!([
        {"id": "raw", "provider": "raw", "native": {}, "on_error": "report"},
        {"id": "bool", "provider": "boolean", "effects": ["observe"], "on_error": "report"},
        {"id": "record", "provider": "policy", "operation": "record", "effects": ["observe"], "on_error": "report"}
    ]);
    let bytes = serde_json::to_vec(&document).unwrap();
    let cancelled = AtomicBool::new(false);
    let engine = AdmissionEngine::new().unwrap();
    let candidate = engine.preflight(&bytes, "target", capabilities()).unwrap();
    let prepared = Host::discover(
        &candidate,
        fixture.context(),
        Instant::now() + TIMEOUT,
        &cancelled,
    )
    .unwrap();
    assert_eq!(prepared.revision(), candidate.revision());
    assert_eq!(prepared.target(), "target");
    assert_eq!(prepared.capabilities().adapter, "qoder");
    assert_eq!(prepared.evidence().len(), 1);
    assert!(prepared.evidence().contains_key("policy"));
    assert_eq!(prepared.preparation().len(), 2);
    assert_eq!(prepared.preparation()[0].method, Method::Describe);
    assert_eq!(prepared.preparation()[1].method, Method::ValidateConfig);
    let writer = File::open(prefix.join(".local-install.lock")).unwrap();
    assert!(writer.try_lock().is_err());

    let policy = engine.admit(candidate, prepared.evidence()).unwrap();
    let host = Host::from_admitted(policy, prepared).unwrap();
    assert_eq!(host.policy().revision(), host.revision());
    assert_eq!(host.steps().len(), 4);
    let selected = host
        .event(
            event("tool.before", "block"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let invocation = selected.invoke("check").unwrap();
    let aw_host::StepOutput::Provider(outcome) = invocation.result.unwrap() else {
        panic!("expected structured Provider outcome")
    };
    assert!(outcome.requests_block());
    assert!(writer.try_lock().is_err());
    drop(host);
    writer.try_lock().unwrap();
    fixture.assert_reaped();
}

#[test]
fn from_admitted_and_post_discovery_admission_failures_release_managed_files() {
    let fixture = Fixture::private();
    let (mut invalid_op_doc, prefix) = installed_provider(&fixture);
    invalid_op_doc["spec"]["events"]["tool.before"]["steps"][0]["operation"] =
        json!("not-declared");
    let cancelled = AtomicBool::new(false);
    let engine = AdmissionEngine::new().unwrap();
    let candidate = engine
        .preflight(
            &serde_json::to_vec(&invalid_op_doc).unwrap(),
            "target",
            capabilities(),
        )
        .unwrap();
    let prepared = Host::discover(
        &candidate,
        fixture.context(),
        Instant::now() + TIMEOUT,
        &cancelled,
    )
    .unwrap();
    assert_eq!(prepared.preparation().len(), 2);
    assert!(engine.admit(candidate, prepared.evidence()).is_err());
    drop(prepared);
    File::open(prefix.join(".local-install.lock"))
        .unwrap()
        .try_lock()
        .unwrap();

    let (valid_doc, _) = installed_provider(&fixture);
    let valid_bytes = serde_json::to_vec(&valid_doc).unwrap();
    let mut padded_bytes = valid_bytes.clone();
    padded_bytes.push(b'\n');
    let mut other_target_doc = valid_doc.clone();
    other_target_doc["spec"]["agents"]["other"] =
        other_target_doc["spec"]["agents"]["target"].clone();
    let mut other_caps = capabilities();
    other_caps.version = "other-version".into();

    for (policy_bytes, policy_target, policy_caps) in [
        (padded_bytes, "target", capabilities()),
        (
            serde_json::to_vec(&other_target_doc).unwrap(),
            "other",
            capabilities(),
        ),
        (valid_bytes.clone(), "target", other_caps),
    ] {
        let base_candidate = engine
            .preflight(&valid_bytes, "target", capabilities())
            .unwrap();
        let prepared = Host::discover(
            &base_candidate,
            fixture.context(),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
        let mismatched_candidate = engine
            .preflight(&policy_bytes, policy_target, policy_caps)
            .unwrap();
        let mismatched_policy = engine
            .admit(mismatched_candidate, prepared.evidence())
            .unwrap();
        let result = Host::from_admitted(mismatched_policy, prepared);
        assert!(matches!(
            result,
            Err(aw_host::Error::Invalid(
                "admitted policy does not match prepared Providers"
            ))
        ));
        File::open(prefix.join(".local-install.lock"))
            .unwrap()
            .try_lock()
            .unwrap();
    }

    // Same configuration bytes in two execution contexts: A declares check, B only declares other.
    let context_a = fixture.context();
    let mut context_b = fixture.context();
    context_b
        .environment
        .insert("AW_HOST_OPERATIONS".into(), "other".into());
    assert!(matches!(
        Host::prepare(
            &valid_bytes,
            "target",
            capabilities(),
            context_b.clone(),
            Instant::now() + TIMEOUT,
            &cancelled,
        ),
        Err(aw_host::Error::Preparation(_))
    ));
    let candidate_b = engine
        .preflight(&valid_bytes, "target", capabilities())
        .unwrap();
    let prepared_b = Host::discover(
        &candidate_b,
        context_b,
        Instant::now() + TIMEOUT,
        &cancelled,
    )
    .unwrap();
    assert!(engine.admit(candidate_b, prepared_b.evidence()).is_err());
    let candidate_a = engine
        .preflight(&valid_bytes, "target", capabilities())
        .unwrap();
    let prepared_a = Host::discover(
        &candidate_a,
        context_a,
        Instant::now() + TIMEOUT,
        &cancelled,
    )
    .unwrap();
    let policy_a = engine.admit(candidate_a, prepared_a.evidence()).unwrap();
    drop(prepared_a);
    assert!(matches!(
        Host::from_admitted(policy_a, prepared_b),
        Err(aw_host::Error::Invalid(
            "admitted policy does not match prepared Providers"
        ))
    ));
    File::open(prefix.join(".local-install.lock"))
        .unwrap()
        .try_lock()
        .unwrap();
    fixture.assert_reaped();
}
