//! CLI-to-service hook bridge contracts; fake-host scheduling is not native adoption.
#![cfg(target_os = "linux")]

#[path = "launcher/support.rs"]
mod support;

use serde_json::{json, Value};
use std::{
    fs,
    os::unix::process::ExitStatusExt,
    thread,
    time::{Duration, Instant},
};
use support::{Fixture, Process, Service, LITERAL, TIMEOUT};

fn hooks(report: &Value, event: &str) -> Vec<Value> {
    report["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["event"] == event)
        .unwrap()["hooks"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn native_bytes_exit_two_signals_and_existing_settings_survive_the_bridge() {
    for after in [false, true] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        for (label, behavior) in [("bytes", "bytes"), ("block", "block"), ("signal", "signal")] {
            fixture.native(&mut document, label, behavior);
        }
        if after {
            let mut event = document["spec"]["events"]["tool.before"].take();
            for step in event["steps"].as_array_mut().unwrap() {
                step["on_error"] = json!("report");
            }
            document["spec"]["events"] = json!({"tool.after":event});
        }
        let service = Service::start(&fixture, &document);
        let settings = fixture.root.join("native-settings.json");
        let original = json!({"theme":"fixture-theme","hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":fixture.python,"args":[fixture.action(),"--root",fixture.root,"--protocol","native","--label","existing"]}]}]}});
        fs::write(&settings, original.to_string()).unwrap();
        let report =
            fixture.successful(fixture.launch(&["-p", "--run-id", "raw"], Some(&settings)));
        let event = if after { "PostToolUse" } else { "PreToolUse" };
        let result = hooks(&report, event);
        assert_eq!(result.len(), if after { 3 } else { 4 });
        for (label, status) in [("bytes", 0), ("block", 2), ("signal", -libc::SIGTERM)] {
            let item = result.iter().find(|entry| entry["step"] == label).unwrap();
            assert_eq!(item["status"], status);
            assert_eq!(item["stdout"], "6e617469766500ff0a");
            assert_eq!(item["stderr"], "646961676e6f73746963fe0a");
            assert_eq!(
                fs::read(fixture.root.join(format!("native-raw-{label}.bin"))).unwrap(),
                fs::read(fixture.root.join(format!("payload-raw-{event}.bin"))).unwrap()
            );
            assert_eq!(
                serde_json::from_slice::<Value>(
                    &fs::read(fixture.root.join(format!("argv-raw-{label}.json"))).unwrap()
                )
                .unwrap(),
                LITERAL
            );
        }
        assert_eq!(report["tool_executed"], after);
        let captured: Value =
            serde_json::from_slice(&fs::read(fixture.root.join("settings-raw.json")).unwrap())
                .unwrap();
        assert_eq!(captured["theme"], original["theme"]);
        assert_eq!(
            captured["hooks"]["PreToolUse"][0],
            original["hooks"]["PreToolUse"][0]
        );
        assert_eq!(fs::read_to_string(&settings).unwrap(), original.to_string());
        assert!(!fixture.root.join("NEVER").exists());
        service.released(&fixture);
    }
}

#[test]
fn structured_before_and_after_outputs_and_agent_exit_status_are_preserved() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    let allowed =
        fixture.successful(fixture.launch(&["--run-id", "allow", "--scenario", "allow"], None));
    assert_eq!(allowed["tool_executed"], true);
    for event in ["PreToolUse", "PostToolUse"] {
        let result = hooks(&allowed, event);
        assert_eq!(result[0]["status"], 0);
        assert_eq!(result[0]["stdout"], "7b7d");
    }
    service.released(&fixture);
    let blocked =
        fixture.successful(fixture.launch(&["--run-id", "deny", "--scenario", "block"], None));
    assert_eq!(blocked["tool_executed"], false);
    let result = hooks(&blocked, "PreToolUse");
    assert_eq!(result[0]["status"], 2);
    assert_eq!(result[0]["stdout"], "");
    assert!(!fixture.root.join("tool-deny").exists());
    let failure = fixture.run(fixture.launch(&["--run-id", "exit", "--agent-exit", "17"], None));
    assert_eq!(failure.status.code(), Some(17));
    service.released(&fixture);
    let signalled = fixture.run(fixture.launch(&["--run-id", "signal", "--agent-signal"], None));
    assert_eq!(signalled.status.signal(), Some(libc::SIGTERM));
    service.released(&fixture);
}

#[test]
fn native_query_separator_keeps_generated_hooks_and_literal_query() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    let query = ["--cwd", "/literal", "-dw", "--settings=literal", LITERAL];
    let native: Vec<_> = ["-p", "--run-id", "separator", "--"]
        .into_iter()
        .chain(query)
        .collect();
    let report = fixture.successful(fixture.launch(&native, None));
    assert_eq!(report["query"], json!(query));
    assert_eq!(report["tool_executed"], true);
    for event in ["PreToolUse", "PostToolUse"] {
        let result = hooks(&report, event);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["status"], 0);
    }
    service.released(&fixture);
}

#[test]
fn native_commands_use_verified_directory_and_callback_environment() {
    for (after, work_mode, client_version, source, version) in [
        (false, None, None, "cli", "1.1.64"),
        (true, Some("1"), Some("custom"), "qoderwork", "custom"),
        (false, Some("0"), Some(""), "cli", "1.1.64"),
    ] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        fixture.native(&mut document, "context", "environment");
        if after {
            let mut event = document["spec"]["events"]["tool.before"].take();
            event["steps"][0]["on_error"] = json!("report");
            document["spec"]["events"] = json!({"tool.after": event});
        }
        let service = Service::start(&fixture, &document);
        let mut command = fixture.launch(&["--run-id", "environment"], None);
        command
            .env("QODER_PROJECT_DIR", "/stale/qoder")
            .env("CLAUDE_PROJECT_DIR", "/stale/claude")
            .env("QODER_HOOK_SOURCE", "stale-source")
            .env("QODER_HOOK_VERSION", "stale-version")
            .env("QODER_SITE", "stale-site")
            .env("FAKE_SHARED_ENV", "launcher");
        if let Some(work_mode) = work_mode {
            command.env("QODER_WORK_INTEGRATION_MODE", work_mode);
        }
        if let Some(client_version) = client_version {
            command.env("QODER_CLIENT_VERSION", client_version);
        }
        let report = fixture.successful(command);
        let event = if after { "PostToolUse" } else { "PreToolUse" };
        assert_eq!(hooks(&report, event)[0]["status"], 0);
        let environment: Value = serde_json::from_slice(
            &fs::read(fixture.root.join("environment-environment-context.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            environment,
            json!({"QODER_PROJECT_DIR":fixture.root,"CLAUDE_PROJECT_DIR":fixture.root,
                   "FAKE_SHARED_ENV":"callback","FAKE_CALLBACK_ONLY":"callback",
                   "QODER_HOOK_SOURCE":source,"QODER_HOOK_VERSION":version,"QODER_SITE":"GLOBAL"})
        );
        service.released(&fixture);
    }
}

#[test]
fn validated_integral_float_budgets_launch_before_and_after_hooks() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    document["spec"]["execution"]["default_event_budget_ms"] = json!(5000.0);
    document["spec"]["events"]["tool.after"]["budget_ms"] = json!(4500.0);
    let service = Service::start(&fixture, &document);
    let mut validate = fixture.command();
    validate
        .args(["validate", "--config"])
        .arg(fixture.root.join("aw.json"));
    assert!(fixture.run(validate).status.success());
    let report = fixture.successful(fixture.launch(&["--run-id", "float"], None));
    assert_eq!(report["tool_executed"], true);
    for event in ["PreToolUse", "PostToolUse"] {
        assert_eq!(hooks(&report, event)[0]["status"], 0);
    }
    service.released(&fixture);
}

#[test]
fn fake_host_can_schedule_generated_steps_in_parallel_or_serially() {
    for sequential in [false, true] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        let behavior = if sequential { "ordered" } else { "barrier" };
        fixture.native(&mut document, "first", behavior);
        fixture.native(&mut document, "second", behavior);
        if sequential {
            document["spec"]["agents"]["qoder"]["qoder"] = json!({"sequential":true});
        }
        let service = Service::start(&fixture, &document);
        let report = fixture.successful(fixture.launch(&["--run-id", "order"], None));
        for result in hooks(&report, "PreToolUse") {
            assert_eq!(result["status"], 0, "{result}");
        }
        let settings: Value =
            serde_json::from_slice(&fs::read(fixture.root.join("settings-order.json")).unwrap())
                .unwrap();
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["sequential"],
            if sequential { json!(true) } else { Value::Null }
        );
        for label in ["first", "second"] {
            assert!(fixture
                .root
                .join(format!(
                    "{}-order-{label}",
                    if sequential { "done" } else { "overlap" }
                ))
                .exists());
        }
        service.released(&fixture);
    }
}

#[test]
fn unsafe_native_launch_options_and_disabled_hooks_are_rejected_before_binding() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    for native in [
        vec!["--settings=override.json"],
        vec!["--resume", "session"],
        vec!["--bare"],
        vec!["-dw", "/tmp"],
        vec!["-pc"],
        vec!["-pPrompt"],
    ] {
        let output = fixture.run(fixture.launch(&native, None));
        assert!(!output.status.success(), "accepted {native:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("not supported"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!fixture.root.join("started-one").exists());
        assert_eq!(service.status()["bindings"], 0);
    }
    for (name, value) in [
        ("QODER_HEADLESS_FAST_HOOKS", "1"),
        ("QODER_CONFIG_DIR_NAME", "custom"),
        ("FAKE_VERSION", "0.0.0"),
    ] {
        let mut command = fixture.launch(&[], None);
        command.env(name, value);
        assert!(!fixture.run(command).status.success());
    }
    let settings = fixture.root.join("disabled.json");
    fs::write(&settings, b"{\"disableAllHooks\":true}").unwrap();
    assert!(!fixture
        .run(fixture.launch(&[], Some(&settings)))
        .status
        .success());
    fs::create_dir(fixture.root.join(".qoder")).unwrap();
    fs::write(
        fixture.root.join(".qoder/settings.local.json"),
        b"{\"hooksConfig\":{\"enabled\":false}}",
    )
    .unwrap();
    assert!(!fixture.run(fixture.launch(&[], None)).status.success());
    assert!(!fixture.root.join("started-one").exists());
    service.released(&fixture);
}

#[test]
fn two_agent_instances_release_independently_and_leave_the_daemon_running() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    let pid = service.status()["pid"].clone();
    let first = Process::spawn(
        &fixture,
        fixture.launch(&["--run-id", "first", "--hold"], None),
    );
    let deadline = Instant::now() + TIMEOUT;
    while !fixture.root.join("ready-first").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(service.status()["bindings"], 1);
    let second = fixture.successful(fixture.launch(&["--run-id", "second"], None));
    assert_eq!(second["tool_executed"], true);
    assert_eq!(service.status()["bindings"], 1);
    assert_eq!(service.status()["pid"], pid);
    fs::write(fixture.root.join("release-first"), b"release").unwrap();
    let output = first.finish();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    service.released(&fixture);
    assert_eq!(service.status()["pid"], pid);
}

#[test]
fn terminating_during_preparation_cancels_only_that_binding() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGKILL] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        fixture.provider(&mut document);
        let service = Service::start(&fixture, &document);
        let mut command = fixture.launch(&[], None);
        command.env("FAKE_PREPARE_BLOCK", "1");
        let process = Process::spawn(&fixture, command);
        let deadline = Instant::now() + TIMEOUT;
        let marker = fixture.root.join("preparing.pid");
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "Provider preparation never started"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let pid: u32 = fs::read_to_string(marker).unwrap().parse().unwrap();
        let started = Instant::now();
        process.signal(signal);
        let output = process.finish();
        assert_eq!(output.status.signal(), Some(signal));
        let deadline = started + Duration::from_secs(2);
        while service.status()["bindings"] != 0 {
            assert!(
                Instant::now() < deadline,
                "orphan binding after termination"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert!(!fixture.root.join("started-one").exists());
        // SIGKILL cannot run local file destructors; the owning fixture removes them.
        if signal != libc::SIGKILL {
            fixture.clean_launches();
        }
        let report = fixture.successful(fixture.launch(&[], None));
        assert_eq!(report["tool_executed"], true);
        assert_eq!(service.status()["bindings"], 0);
    }
}

#[test]
fn byte_environment_reaches_agent_native_hooks_and_structured_providers() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    fixture.native(&mut document, "raw", "observe");
    let service = Service::start(&fixture, &document);
    let mut command = fixture.launch(&[], None);
    command.env(
        "AW_TEST_BYTES_VALUE",
        OsString::from_vec(b"ok\xff".to_vec()),
    );
    command.env(
        OsString::from_vec(b"AW_TEST_BYTES_\xff".to_vec()),
        OsString::from_vec(vec![254]),
    );
    let report = fixture.successful(command);
    assert_eq!(report["tool_executed"], true);
    let expected = json!([
        ["41575f544553545f42595445535f56414c5545", "6f6bff"],
        ["41575f544553545f42595445535fff", "fe"]
    ]);
    for recipient in ["agent", "native", "provider"] {
        let actual: Value = serde_json::from_slice(
            &fs::read(
                fixture
                    .root
                    .join(format!("environment-bytes-{recipient}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(actual, expected, "{recipient}");
    }
    service.released(&fixture);
}
