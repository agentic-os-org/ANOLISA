//! Real resident plugin registration and service wiring over framework doubles.

use super::*;

fn resident(fixture: &Fixture, after: &str) -> Value {
    let mut document = setup(fixture);
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qwenpaw/resident_host.py"),
        fixture.root.join("fake qwenpaw"),
    )
    .unwrap();
    fs::set_permissions(
        fixture.root.join("fake qwenpaw"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fixture.provider(&mut document);
    document["spec"]["providers"]["policy"]["transport"]["argv"] = json!([
        "/usr/bin/python3",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qwenpaw/resident_provider.py"),
        fixture.root
    ]);
    document["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!(after);
    document
}

fn events(fixture: &Fixture) -> Vec<Value> {
    fs::read_to_string(fixture.root.join("provider-events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn withhold_failure_blocks_result_and_next_dispatch_through_launcher() {
    check_withhold_failure(false);
}

#[test]
fn boolean_after_failure_withholds_result_and_stops_next_dispatch() {
    check_withhold_failure(true);
}

fn check_withhold_failure(boolean: bool) {
    let fixture = Fixture::new();
    let mut document = resident(&fixture, "withhold_result");
    if boolean {
        document["spec"]["providers"]["broken"] = json!({
            "transport":{"type":"stdio","location":"agent","argv":["/usr/bin/python3","-c","import sys; sys.stdin.read(); print('invalid boolean')"]},
            "timeout_ms":1000,"max_output_bytes":32});
        document["spec"]["events"]["tool.after"]["steps"] = json!([{
            "id":"record","provider":"broken","effects":["observe"],"on_error":"withhold_result"}]);
    }
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("FAKE_QWENPAW_AFTER_FAILURE", "1");
    let result = fixture.successful(command);
    assert_eq!(result["ready_hook_count"], 2);
    assert_eq!(result["failure"], "ResidentFailure");
    assert_eq!(result["results"], json!([]));
    assert_eq!(
        fs::read_to_string(fixture.root.join("tool-executed")).unwrap(),
        "call-0\n"
    );
    assert!(!fixture.root.join("native-chunks.jsonl").exists());
    let callbacks = events(&fixture);
    assert_eq!(callbacks.len(), if boolean { 1 } else { 2 });
    assert_eq!(callbacks[0]["name"], "tool.before");
    if !boolean {
        assert_eq!(callbacks[1]["name"], "tool.after");
    }
    let recent = service
        .client
        .call(Operation::RecentExecutions, Instant::now() + TIMEOUT)
        .unwrap();
    let after = recent["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["step_id"] == "record")
        .unwrap();
    assert_eq!(after["failure_action"], "withhold_result");
    assert_eq!(after["failure"], if boolean { "protocol" } else { "exit" });
    let durable_withhold = fs::read_dir(fixture.root.join("state/journal"))
        .unwrap()
        .any(|entry| {
            fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .any(|record| record["record"]["failure_action"] == "withhold_result")
        });
    assert!(
        durable_withhold,
        "strict failure must reach the durable audit"
    );
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn report_failure_preserves_results_and_next_dispatch_through_launcher() {
    let fixture = Fixture::new();
    let document = resident(&fixture, "report");
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("FAKE_QWENPAW_AFTER_FAILURE", "1");
    let result = fixture.successful(command);
    assert_eq!(result["failure"], Value::Null);
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["results"][0]["content"][0]["text"],
        "protected tool output"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("tool-executed")).unwrap(),
        "call-0\ncall-1\n"
    );
    assert_eq!(events(&fixture).len(), 4);
    assert_eq!(
        fs::read_to_string(fixture.root.join("native-chunks.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn before_denial_prevents_native_execution_through_resident_factory() {
    let fixture = Fixture::new();
    let document = resident(&fixture, "withhold_result");
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("FAKE_QWENPAW_DENY", "1");
    let result = fixture.successful(command);
    assert_eq!(result["failure"], Value::Null);
    assert_eq!(result["results"][0]["state"], "denied");
    assert_eq!(
        result["results"][0]["content"][0]["text"],
        r#"[tokenless:rewrite] Retry this tool with arguments: {"command":"rtk git status"}"#
    );
    assert!(!fixture.root.join("tool-executed").exists());
    assert_eq!(events(&fixture).len(), 4);
    preserved(&fixture);
    service.released(&fixture);
}

#[test]
fn registration_and_missing_factory_fail_closed_and_release_launch() {
    for variable in ["FAKE_QWENPAW_DISABLED", "FAKE_QWENPAW_DROP_FACTORY"] {
        let fixture = Fixture::new();
        let document = resident(&fixture, "withhold_result");
        let service = Service::start(&fixture, &document);
        let mut command = launch(&fixture);
        command.env(variable, "1");
        let output = fixture.run(command);
        assert!(!output.status.success());
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains(if variable == "FAKE_QWENPAW_DISABLED" {
                "configuration disables the AW plugin"
            } else {
                "requires one installed resident middleware"
            }),
            "{diagnostics}"
        );
        assert!(!fixture.root.join("tool-executed").exists());
        assert!(!fixture.root.join("provider-events.jsonl").exists());
        preserved(&fixture);
        service.released(&fixture);
    }
}

#[test]
fn reloadable_entry_rejects_withhold_before_service_or_plugin_creation() {
    let fixture = Fixture::new();
    let document = resident(&fixture, "withhold_result");
    fixture.save(&document);
    let mut command = launch(&fixture);
    command.arg("--reloadable");
    let output = fixture.run(command);
    assert!(!output.status.success());
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains("failure action is not supported"),
        "{diagnostics}"
    );
    assert!(!fixture.root.join("state").exists());
    assert!(!fixture.root.join("tool-executed").exists());
    preserved(&fixture);
}

#[test]
fn native_denial_text_survives_resident_scope_transport() {
    let fixture = Fixture::new();
    let mut document = resident(&fixture, "withhold_result");
    native(&fixture, &mut document, "tool.before", "block");
    let service = Service::start(&fixture, &document);
    let result = fixture.successful(launch(&fixture));
    assert_eq!(result["results"][0]["state"], "denied");
    assert_eq!(result["results"][0]["content"][0]["text"], "native block");
    assert!(!fixture.root.join("tool-executed").exists());
    preserved(&fixture);
    service.released(&fixture);
}
