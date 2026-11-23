//! Managed submissions use real admission, Provider calls and task cleanup.
use super::*;

fn submission_setup(fixture: &Fixture) -> Value {
    let mut document = setup(fixture);
    for (source, target) in [
        ("submission.mjs", "app.mjs"),
        (
            "submission-sdk.mjs",
            "node_modules/@earendil-works/pi-coding-agent/dist/index.js",
        ),
    ] {
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pi")
                .join(source),
            fixture.root.join(target),
        )
        .unwrap();
    }
    document["spec"]["providers"]["submit"] = json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":[fixture.python,
          Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/submission-policy.py"),fixture.root]},
        "timeout_ms":500,"max_output_bytes":4096,"config":{}});
    document["spec"]["events"]["input.submit"] = json!({"enabled":true,"required":true,"budget_ms":1500,
        "steps":[{"id":"first","provider":"submit","operation":"submit","effects":["observe","block"],"on_error":"block"},
        {"id":"second","provider":"submit","operation":"submit","effects":["observe","block"],"on_error":"block"}]});
    document
}

#[test]
fn text_submission_denial_recovers_and_preserves_session_turn_identity() {
    let fixture = Fixture::new();
    let document = submission_setup(&fixture);
    let service = Service::start(&fixture, &document);
    assert_eq!(fixture.successful(launch(&fixture))["passed"], true);
    service.released(&fixture);
}

#[test]
fn submission_faults_apply_block_and_report_before_dispatch() {
    for scenario in ["error", "invalid", "timeout"] {
        for action in ["block", "report"] {
            let fixture = Fixture::new();
            let mut document = submission_setup(&fixture);
            for step in document["spec"]["events"]["input.submit"]["steps"]
                .as_array_mut()
                .unwrap()
            {
                step["on_error"] = json!(action);
            }
            let service = Service::start(&fixture, &document);
            let mut command = launch(&fixture);
            command.env("PI_SUBMISSION_CASE", scenario).env(
                "PI_SUBMISSION_REPORT",
                if action == "report" { "1" } else { "0" },
            );
            assert_eq!(fixture.successful(command)["passed"], true);
            service.released(&fixture);
        }
    }
}

#[test]
fn submission_abort_dispose_and_concurrent_prompt_never_dispatch_or_leak() {
    for scenario in ["abort", "dispose"] {
        let fixture = Fixture::new();
        let mut document = submission_setup(&fixture);
        document["spec"]["providers"]["submit"]["timeout_ms"] = json!(5000);
        document["spec"]["events"]["input.submit"]["budget_ms"] = json!(8000);
        let service = Service::start(&fixture, &document);
        let mut command = launch(&fixture);
        command.env("PI_SUBMISSION_CASE", scenario);
        assert_eq!(fixture.successful(command)["model_calls"], 0);
        service.released(&fixture);
    }
}

#[test]
fn submission_requires_resident_worker_before_legacy_factory() {
    let fixture = Fixture::new();
    let document = submission_setup(&fixture);
    fs::write(fixture.root.join("app.mjs"), "export async function createSession(){throw Error('factory executed');} export async function run(){}").unwrap();
    let service = Service::start(&fixture, &document);
    let result = fixture.run(launch(&fixture));
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("input.submit requires the runWorker factory"));
    service.released(&fixture);
}

#[test]
fn submission_boundary_rejects_changed_messages_and_retains_native_continuations() {
    let fixture = Fixture::new();
    let mut command = Command::new("node");
    command.arg("--test").arg(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/submission-boundary.mjs"),
    );
    let result = fixture.run(command);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn skill_submission_pins_source_and_marks_ordinary_input_explicitly() {
    let fixture = Fixture::new();
    let mut command = Command::new("node");
    command
        .arg("--test")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/skill-boundary.mjs"));
    let result = fixture.run(command);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
