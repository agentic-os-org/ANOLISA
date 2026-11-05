//! Submission policy, explicit closure and recovery through the real CLI/service.

use super::*;

#[test]
fn submission_client_checks_and_cleanup_contracts() {
    let fixture = Fixture::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = std::process::Command::new(&fixture.python);
    command
        .args(["-B"])
        .arg(root.join("tests/fixtures/claude-sdk/submission_contract.py"))
        .arg(root.join("../aw-adapter/native/claude-sdk"));
    let result = fixture.run(command);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success() && stderr.contains("Ran 11 tests") && stderr.contains("\nOK\n"),
        "{stderr}"
    );
}

#[test]
fn submission_provider_faults_and_rejection_close_before_recovery() {
    for (scenario, action, expected) in [
        ("observe", "block", 0),
        ("block", "report", 2),
        ("error", "block", 2),
        ("error", "report", 0),
        ("timeout", "block", 2),
        ("timeout", "report", 0),
    ] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        document["spec"]["agents"] = json!({"claude":{
            "adapter":"claude-sdk","argv":["python3","worker.py"]}});
        fixture.provider(&mut document);
        document["spec"]["events"] = json!({"input.submit": {
            "enabled":true,"required":true,"budget_ms":1000,
            "steps":[{"id":"check","provider":"policy","operation":"check",
                "effects":["observe","block"],"on_error":action},
                {"id":"later","provider":"policy","operation":"check",
                "effects":["observe","block"],"on_error":action}]}});
        document["spec"]["providers"]["policy"]["timeout_ms"] = json!(100);
        let service = Service::start(&fixture, &document);
        let binding: Binding = serde_json::from_value(
            service
                .client
                .call(
                    Operation::Bind {
                        target: "claude".into(),
                        capabilities: Capabilities {
                            failure_actions: Default::default(),
                            adapter: "claude-sdk".into(),
                            version: "0.2.88".into(),
                            entrypoint: "sdk-worker".into(),
                            events: BTreeMap::from([(
                                "input.submit".into(),
                                vec!["observe".into(), "block".into()],
                            )]),
                        },
                        cwd: fixture.root.to_str().unwrap().into(),
                        environment: fixture
                            .command()
                            .get_envs()
                            .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
                            .collect(),
                    },
                    Instant::now() + TIMEOUT,
                )
                .unwrap(),
        )
        .unwrap();
        let path = fixture.root.join("binding.json");
        fs::write(
            &path,
            json!({"adapter":"claude-sdk","binding":binding,
            "socket":fixture.root.join("state/aw.sock"),"cwd":fixture.root,"events":{
                "input.submit":{"budget_ms":1000,"steps":{"check":action,"later":action}}
            }})
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let invoke = |payload: &Value, close: bool| {
            let input = fixture.root.join("input.json");
            fs::write(&input, payload.to_string()).unwrap();
            let mut command = fixture.command();
            command
                .args(["hook", "--adapter", "claude-sdk", "--binding"])
                .arg(&path)
                .args([
                    "--event",
                    "input.submit",
                    "--step",
                    "check",
                    "--on-error",
                    action,
                ])
                .stdin(fs::File::open(input).unwrap());
            if close {
                command.args(["--close-event", "true"]);
            }
            fixture.run_input(command, Some(payload.to_string().as_bytes()))
        };
        let mut payload = json!({"hook_event_name":"AWInputSubmit","cwd":fixture.root,
            "session_id":"session","aw_turn_id":"first","input_text":"fresh user text",
            "scenario":scenario});
        let output = invoke(&payload, false);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(invoke(&payload, true).status.success());
        // Explicit closure is idempotent, and another turn reuses this binding.
        assert!(invoke(&payload, true).status.success());
        payload["aw_turn_id"] = json!("second");
        payload["scenario"] = json!("observe");
        assert!(invoke(&payload, false).status.success());
        assert!(invoke(&payload, true).status.success());
        // Foreground normalization/identity faults must remain failures even in report mode.
        payload["session_id"] = json!("");
        assert_eq!(invoke(&payload, false).status.code(), Some(1));
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: binding.instance_id,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        service.released(&fixture);
    }
}
