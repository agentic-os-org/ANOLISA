//! Foreground launcher and HTTP fixtures verify automatic observation and owned cleanup.

use super::{common, support};
use aw_service::{Operation, RunOutcome};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::process::ExitStatusExt,
    time::{Duration, Instant},
};

// The shared launcher harness also supports installation and Provider scenarios.
#[allow(dead_code)]
#[path = "../launcher/support.rs"]
mod launch;

#[path = "lifecycle/wait.rs"]
mod wait;

fn configured(source: &support::Source, fixture: &launch::Fixture) -> Value {
    let mut source_document = json!({"spec":{"agents":{"target":{}}}});
    source.configure(&mut source_document);
    let mut document = fixture.document();
    document["spec"]["agents"]["qoder"]["evidence"] =
        source_document["spec"]["agents"]["target"]["evidence"].clone();
    document
}

fn launch_source(fixture: &common::Fixture) -> support::Source {
    let mut trajectory = support::trajectory(3);
    trajectory["session_id"] = json!("fixture-session");
    support::Source::new(fixture, trajectory)
}

#[test]
fn run_observes_without_providers_and_resumed_history_belongs_to_new_instance() {
    let source_files = common::Fixture::new();
    let source = launch_source(&source_files);
    let fixture = launch::Fixture::new();
    let document = configured(&source, &fixture);
    let service = launch::Service::start(&fixture, &document);
    let mut instances = Vec::new();
    for id in ["first", "resumed"] {
        let mut process =
            launch::Process::spawn(&fixture, fixture.launch(&["--run-id", id, "--hold"], None));
        wait::ready(&mut process, &fixture, id);
        wait::observation(&mut process, &service, "available", |state| {
            state["observations"][0]["source_state"] == "available"
        });
        let state = service.status()["observations"][0].clone();
        assert_eq!(state["callbacks"], 2);
        assert_eq!(state["run"]["state"], "started");
        assert_eq!(state["association"]["status"], "verified");
        assert_eq!(state["association"]["session_id"], "fixture-session");
        assert_eq!(state["coverage"], "unknown");
        assert_eq!(state["ebpf"], "unknown");
        assert_eq!(state["collection"], "unknown");
        assert!(state["evidence_gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap == "source_time_unverified"));
        assert!(state["source_revision"].is_string());
        instances.push(state["instance_id"].clone());
        fs::write(fixture.root.join(format!("release-{id}")), "exit").unwrap();
        let output = process.finish();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["tool_executed"], true);
        for event in report["events"].as_array().unwrap() {
            assert_eq!(event["hooks"].as_array().unwrap().len(), 1);
            assert_eq!(event["hooks"][0]["step"], "$aw.observe");
            assert_eq!(event["hooks"][0]["status"], 0);
        }
        let records = common::audit(&service.client, state["audit_key"].as_str().unwrap());
        assert_eq!(records["terminal"], true);
        let last = &records["records"].as_array().unwrap().last().unwrap()["record"];
        assert_eq!(last["outcome"]["state"], "exited");
        assert_eq!(last["outcome"]["code"], 0);
        let text = records.to_string();
        for private in [
            support::SECRET,
            support::HISTORY,
            "echo fixture",
            "native-output",
        ] {
            assert!(!text.contains(private));
        }
        assert_eq!(service.status()["bindings"], 0);
        assert_eq!(service.status()["observations"], json!([]));
        fixture.clean_launches();
    }
    assert_ne!(instances[0], instances[1]);
    assert!(source
        .paths
        .lock()
        .unwrap()
        .iter()
        .any(|path| path == "/api/trajectories/fixture-session"));
}

#[test]
fn off_registers_no_extra_callbacks_and_never_contacts_source() {
    let source_files = common::Fixture::new();
    let source = launch_source(&source_files);
    let fixture = launch::Fixture::new();
    let mut document = configured(&source, &fixture);
    document["spec"]["agents"]["qoder"]["evidence"]["mode"] = json!("off");
    let service = launch::Service::start(&fixture, &document);
    let report = fixture.successful(fixture.launch(&[], None));
    for event in report["events"].as_array().unwrap() {
        assert_eq!(event["hooks"], json!([]));
    }
    assert!(source.paths.lock().unwrap().is_empty());
    assert_eq!(service.status()["bindings"], 0);
    fixture.clean_launches();
    document["spec"]["agents"]["qoder"]["evidence"]["mode"] = json!("required");
    assert!(aw_config::Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&document).unwrap())
        .is_err());
}

#[test]
fn actions_reuse_callbacks_and_optional_failure_preserves_native_exit() {
    for auth_status in [200, 401, 403] {
        let source_files = common::Fixture::new();
        let source = launch_source(&source_files);
        let fixture = launch::Fixture::new();
        let mut document = configured(&source, &fixture);
        fixture.provider(&mut document);
        *source.auth_status.lock().unwrap() = auth_status;
        let service = launch::Service::start(&fixture, &document);
        let mut process = launch::Process::spawn(
            &fixture,
            fixture.launch(&["--hold", "--agent-exit", "7"], None),
        );
        wait::ready(&mut process, &fixture, "one");
        if auth_status == 200 {
            wait::observation(&mut process, &service, "available", |state| {
                state["observations"][0]["source_state"] == "available"
            });
        }
        let state = service.status()["observations"][0].clone();
        assert_eq!(
            state["query_state"],
            if auth_status == 200 {
                "ready"
            } else {
                "unavailable"
            }
        );
        assert_eq!(state["callbacks"], 2);
        fs::write(fixture.root.join("release-one"), "exit").unwrap();
        let output = process.finish();
        assert_eq!(
            output.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["tool_executed"], true);
        for event in report["events"].as_array().unwrap() {
            assert_eq!(event["hooks"].as_array().unwrap().len(), 1);
            assert_ne!(event["hooks"][0]["step"], "$aw.observe");
            assert_eq!(event["hooks"][0]["status"], 0);
        }
        if auth_status != 200 {
            assert!(source
                .paths
                .lock()
                .unwrap()
                .iter()
                .all(|p| p == "/api/auth/status"));
        }
        assert_eq!(service.status()["bindings"], 0);
        fixture.clean_launches();
    }
}

#[test]
fn resident_source_loss_and_recovery_are_fenced_from_policy_dispatch() {
    let source_files = common::Fixture::new();
    let source = launch_source(&source_files);
    let fixture = launch::Fixture::new();
    let mut document = configured(&source, &fixture);
    fixture.provider(&mut document);
    let service = launch::Service::start(&fixture, &document);
    let mut process = launch::Process::spawn(
        &fixture,
        fixture.launch(&["--hold", "--run-id", "resident"], None),
    );
    wait::ready(&mut process, &fixture, "resident");
    let reads = || {
        source
            .paths
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.starts_with("/api/trajectories/"))
            .count()
    };

    wait::observation(&mut process, &service, "available", |state| {
        state["observations"][0]["source_state"] == "available" && reads() == 2
    });
    let observed = service.status()["observations"][0].clone();
    let key = observed["audit_key"].as_str().unwrap();
    assert_eq!(observed["source"]["installed"], true);
    assert_eq!(observed["source"]["state"], "ready");
    assert_eq!(observed["source"]["proof"], "query_transport");
    let reads_before = reads();
    for (code, expected) in [(503, "lost"), (404, "unsupported"), (200, "recovered")] {
        *source.auth_status.lock().unwrap() = code;
        wait::observation(&mut process, &service, expected, |state| {
            state["observations"][0]["source"]["state"] == expected
        });
        let state = service.status();
        assert_eq!(state["bindings"], 1);
        assert_eq!(state["audit_healthy"], true);
        assert_eq!(state["observations"][0]["ebpf"], "unknown");
        assert!(state["observations"][0]["source"]["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!("ebpf_coverage_unmeasured")));
        assert!(state["observations"][0]["source_revision"].is_null());
    }
    assert_eq!(
        reads(),
        reads_before,
        "recovery probed the endpoint without replaying trajectories"
    );
    fs::write(fixture.root.join("release-resident"), "exit").unwrap();
    let output = process.finish();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["tool_executed"], true);
    for event in report["events"].as_array().unwrap() {
        assert_eq!(event["hooks"].as_array().unwrap().len(), 1);
        assert_ne!(event["hooks"][0]["step"], "$aw.observe");
        assert_eq!(event["hooks"][0]["status"], 0);
    }
    let audit = common::audit(&service.client, key);
    let records: Vec<_> = audit["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["record"])
        .collect();
    let transitions: Vec<_> = records
        .iter()
        .filter(|row| row["event"] == "coverage.changed")
        .map(|row| row["source"]["state"].as_str().unwrap())
        .collect();
    assert_eq!(transitions, ["ready", "lost", "unsupported", "recovered"]);
    assert_eq!(
        records
            .iter()
            .filter(|row| row["event"] == "runtime.observed")
            .count(),
        2
    );
    assert_eq!(
        records
            .iter()
            .filter(|row| row["event"] == "runtime.exited")
            .count(),
        1
    );
    assert_eq!(service.status()["bindings"], 0);
}

#[test]
fn missing_service_store_and_source_errors_preserve_observation_lifecycle() {
    for scenario in ["service", "store", "error"] {
        let source_files = common::Fixture::new();
        let mut source = Some(launch_source(&source_files));
        let fixture = launch::Fixture::new();
        let document = configured(source.as_ref().unwrap(), &fixture);
        match scenario {
            "service" => drop(source.take()),
            "store" => *source.as_ref().unwrap().status.lock().unwrap() = 404,
            "error" => *source.as_ref().unwrap().status.lock().unwrap() = 503,
            _ => unreachable!(),
        }
        let service = launch::Service::start(&fixture, &document);
        let mut process = launch::Process::spawn(&fixture, fixture.launch(&["--hold"], None));
        wait::ready(&mut process, &fixture, "one");
        let reason = match scenario {
            "service" => "source_unavailable",
            "store" => "record_or_store_unavailable",
            _ => "source_unavailable",
        };
        wait::observation(&mut process, &service, reason, |state| {
            state["observations"][0]["source_reason"] == reason
        });
        let state = service.status()["observations"][0].clone();
        assert_eq!(state["callbacks"], 2);
        assert_eq!(state["coverage"], "unknown");
        assert_eq!(state["ebpf"], "unknown");
        assert_eq!(
            state["source_state"],
            if scenario == "store" {
                "unknown"
            } else {
                "unavailable"
            }
        );
        fs::write(fixture.root.join("release-one"), "exit").unwrap();
        let output = process.finish();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["tool_executed"], true);
        assert_eq!(
            common::audit(&service.client, state["audit_key"].as_str().unwrap())["terminal"],
            true
        );
        assert_eq!(service.status()["bindings"], 0);
        assert_eq!(service.status()["observations"], json!([]));
        fixture.clean_launches();
    }
}

#[test]
fn signal_exit_drains_slow_optional_reads_and_leaves_shared_service_alive() {
    let source_files = common::Fixture::new();
    let source = launch_source(&source_files);
    *source.delay.lock().unwrap() = Duration::from_secs(3);
    let fixture = launch::Fixture::new();
    let document = configured(&source, &fixture);
    let service = launch::Service::start(&fixture, &document);
    let mut process = launch::Process::spawn(&fixture, fixture.launch(&["--hold"], None));
    wait::ready(&mut process, &fixture, "one");
    let state = service.status()["observations"][0].clone();
    assert_eq!(state["callbacks"], 2);
    assert!(state["projection_revision"].is_null());
    let started = Instant::now();
    process.signal(libc::SIGTERM);
    let output = process.finish();
    assert_eq!(output.status.signal(), Some(libc::SIGTERM));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(service.status()["bindings"], 0);
    assert_eq!(service.status()["observations"], json!([]));
    assert_eq!(
        common::audit(&service.client, state["audit_key"].as_str().unwrap())["terminal"],
        true
    );
    fixture.clean_launches();
}

#[test]
fn pure_callbacks_reject_forgery_deduplicate_and_bound_the_queue() {
    let fixture = common::Fixture::new();
    let source = support::Source::new(&fixture, support::trajectory(3));
    *source.delay.lock().unwrap() = Duration::from_secs(1);
    let mut document = fixture.document();
    source.configure(&mut document);
    document["spec"]["events"] = json!({});
    document["spec"]["providers"] = json!({});
    let service = common::Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let state = service
        .client
        .call(
            Operation::StartObservation {
                instance_id: binding.instance_id.clone(),
            },
            Instant::now() + common::TIMEOUT,
        )
        .unwrap();
    let mut event = support::native_event(&binding, "tool.before");
    let request = |event: &Value| Operation::ObserveHookEvent {
        instance_id: binding.instance_id.clone(),
        event: event.clone(),
        native_input: serde_json::to_vec(&event["native"]).unwrap(),
    };
    let first = service
        .client
        .call(request(&event), Instant::now() + common::TIMEOUT)
        .unwrap();
    assert_eq!(first["duplicate"], false);
    let duplicate = service
        .client
        .call(request(&event), Instant::now() + common::TIMEOUT)
        .unwrap();
    assert_eq!(duplicate["duplicate"], true);
    let mut forged = event.clone();
    forged["session_id"] = json!("forged");
    assert!(service
        .client
        .call(request(&forged), Instant::now() + common::TIMEOUT)
        .is_err());
    for index in 0..12 {
        event["tool"]["call_id"] = json!(format!("next-{index}"));
        event["native"]["tool_use_id"] = event["tool"]["call_id"].clone();
        service
            .client
            .call(request(&event), Instant::now() + common::TIMEOUT)
            .unwrap();
    }
    let status = service
        .client
        .call(Operation::Status, Instant::now() + common::TIMEOUT)
        .unwrap();
    assert_eq!(status["observations"][0]["callbacks"], 13);
    assert!(status["observations"][0]["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|gap| gap == "observation_queue_full"));
    assert!(service
        .client
        .call(
            Operation::ReportRun {
                instance_id: binding.instance_id.clone(),
                outcome: RunOutcome::Exited {
                    code: Some(0),
                    signal: None
                }
            },
            Instant::now() + common::TIMEOUT
        )
        .is_err());
    service
        .client
        .call(
            Operation::ReleaseInstance {
                instance_id: binding.instance_id.clone(),
            },
            Instant::now() + common::TIMEOUT,
        )
        .unwrap();
    assert!(service
        .client
        .call(request(&event), Instant::now() + common::TIMEOUT)
        .is_err());
    assert_eq!(
        common::audit(&service.client, state["audit_key"].as_str().unwrap())["terminal"],
        true
    );
}

#[test]
fn zero_callbacks_and_spawn_failure_record_truthful_outcomes_and_release() {
    for fail_spawn in [false, true] {
        let source_files = common::Fixture::new();
        let source = launch_source(&source_files);
        let fixture = launch::Fixture::new();
        let document = configured(&source, &fixture);
        let service = launch::Service::start(&fixture, &document);
        let mut command = fixture.launch(&["--scenario", "no-hooks"], None);
        if fail_spawn {
            command.env("FAKE_REMOVE_AFTER_VERSION", "1");
        }
        let output = fixture.run(command);
        assert_eq!(output.status.success(), !fail_spawn);
        let audit = fixture.run_audit(&service, &output);
        assert_eq!(audit["terminal"], true);
        let records = audit["records"].as_array().unwrap();
        assert!(records
            .iter()
            .all(|row| row["record"]["phase"] != "native_callback"));
        assert_eq!(
            records.last().unwrap()["record"]["outcome"]["state"],
            if fail_spawn { "failed" } else { "exited" }
        );
        assert_eq!(service.status()["bindings"], 0);
        assert_eq!(source.paths.lock().unwrap().len(), 1);
        fixture.clean_launches();
    }
}

#[test]
fn concurrent_session_conflict_is_visible_and_release_preserves_other_run() {
    let source_files = common::Fixture::new();
    let source = launch_source(&source_files);
    let fixture = launch::Fixture::new();
    let document = configured(&source, &fixture);
    let service = launch::Service::start(&fixture, &document);
    let mut first = launch::Process::spawn(
        &fixture,
        fixture.launch(&["--run-id", "first", "--hold"], None),
    );
    wait::ready(&mut first, &fixture, "first");
    wait::observation(&mut first, &service, "available", |state| {
        state["observations"][0]["source_state"] == "available"
    });
    let mut second = launch::Process::spawn(
        &fixture,
        fixture.launch(&["--run-id", "second", "--hold"], None),
    );
    wait::ready(&mut second, &fixture, "second");
    wait::observation(&mut second, &service, "source_session_conflict", |state| {
        state["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["source_reason"] == "source_session_conflict")
    });
    first.signal(libc::SIGTERM);
    assert_eq!(first.finish().status.signal(), Some(libc::SIGTERM));
    assert_eq!(service.status()["bindings"], 1);
    assert_eq!(
        service.status()["observations"].as_array().unwrap().len(),
        1
    );
    fs::write(fixture.root.join("release-second"), "exit").unwrap();
    assert!(second.finish().status.success());
    assert_eq!(service.status()["bindings"], 0);
    fixture.clean_launches();
}

#[test]
fn interrupted_startup_drains_the_probe_without_spawning_an_agent() {
    let source_files = common::Fixture::new();
    let source = launch_source(&source_files);
    *source.probe_delay.lock().unwrap() = Duration::from_secs(2);
    let fixture = launch::Fixture::new();
    let document = configured(&source, &fixture);
    let service = launch::Service::start(&fixture, &document);
    let process = launch::Process::spawn(&fixture, fixture.launch(&[], None));
    source.wait_for(1);
    process.signal(libc::SIGTERM);
    let output = process.finish();
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGTERM),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.root.join("started-one").exists());
    assert_eq!(service.status()["bindings"], 0);
    assert_eq!(service.status()["observations"], json!([]));
    fixture.clean_launches();
}
