//! The native command owns its PID while the shared service updates only idle policy.
use super::*;

#[test]
fn native_gateway_checks_launch_identity_and_completed_run_replay() {
    let fixture = Fixture::new();
    for (script, count) in [("startup.mjs", 12), ("reload.mjs", 9)] {
        let mut command = Command::new("node");
        command
            .args(["--test", "--test-reporter=tap"])
            .arg(source(script));
        let output = fixture.run(command);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success()
                && stdout.contains(&format!("# pass {count}\n"))
                && stdout.contains("# fail 0\n"),
            "{script}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn native_gateway_updates_between_confirmed_runs() {
    let fixture = Fixture::new();
    let mut document = document(&fixture);
    command_policy::configure(&fixture, &mut document);
    let entry = fixture.root.join("native with spaces/openclaw.mjs");
    fs::copy(source("native-update.mjs"), &entry).unwrap();
    let service = Service::start(&fixture, &document);
    let mut allow = document.clone();
    allow["spec"]["events"] = json!({});
    fs::write(fixture.root.join("allow.json"), allow.to_string()).unwrap();
    let mut command = native_command(&fixture, &document);
    command
        .env("AW_TEST_CONFIG", fixture.root.join("aw.json"))
        .env("AW_TEST_POLICY", fixture.root.join("allow.json"));
    let output = fixture.run(command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("native-result.json")).unwrap())
            .unwrap();
    assert_eq!(result["passed"], true);
    assert!(fixture.root.join("tool-ran").is_file());
    service.released(&fixture);
    assert!(
        !fs::read_dir(fixture.root.join("state"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("native-")),
        "normal Gateway exit must release its prepared assets"
    );
    let starts = fs::read_dir(fixture.root.join("state/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .flat_map(|path| {
            fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .map(|line| serde_json::from_str::<Value>(&line).unwrap())
        .filter(|record| {
            record["record"]["plan"]["executionEvent"]["data"]["name"] == "instance.started"
        })
        .count();
    assert_eq!(starts, 1);
    assert_eq!(service.status()["observations"], json!([]));
}

fn native_command(fixture: &Fixture, document: &Value) -> Command {
    let mut setup = fixture.command();
    setup
        .args(["instance", "setup", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "openclaw", "--directory"])
        .arg(fixture.root.join("startup"))
        .arg("--native-settings")
        .arg(fixture.root.join("native.json"))
        .arg("--native-state-dir")
        .arg(fixture.root.join("profile"));
    let prepared = fixture.successful(setup);
    let argv = document["spec"]["agents"]["openclaw"]["argv"]
        .as_array()
        .unwrap();
    let mut command = Command::new(argv[0].as_str().unwrap());
    command
        .args(argv.iter().skip(1).map(|value| value.as_str().unwrap()))
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", &fixture.root);
    for (key, value) in prepared["environment"].as_object().unwrap() {
        command.env(key, value.as_str().unwrap());
    }
    command
}

#[test]
fn native_gateway_keeps_tokenless_retry_arguments_and_rechecks_fresh_calls() {
    let fixture = Fixture::new();
    let mut document = document(&fixture);
    fs::copy(
        source("native-retry.mjs"),
        fixture.root.join("native with spaces/openclaw.mjs"),
    )
    .unwrap();
    for (label, behavior) in [("security", "retry-security"), ("tokenless", "retry")] {
        step(
            &fixture,
            &mut document,
            "tool.before",
            label,
            behavior,
            true,
        );
    }
    let service = Service::start(&fixture, &document);
    let output = fixture.run(native_command(&fixture, &document));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("retry-result.json")).unwrap()).unwrap();
    assert_eq!(result["blocked"], true);
    assert_eq!(result["rechecked"], true);
    assert_eq!(
        result["corrected"],
        json!({"command":"rtk git status --short","cwd":"/work tree"})
    );
    assert!(!fixture.root.join("original-ran").exists());
    assert!(!fixture.root.join("retry-ran").exists());
    service.released(&fixture);
    assert_eq!(service.status()["observations"], json!([]));
}
