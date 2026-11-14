//! Real service cleanup ownership, including lost transport and cleanup failures.
use super::*;

#[test]
fn typescript_receipts_close_real_service_events_once() {
    for (mode, step) in [
        ("deny", "first"),
        ("deny", "last"),
        ("error", "first"),
        ("error", "last"),
        ("abort", "first"),
        ("abort", "last"),
        ("report", "first"),
        ("report", "last"),
        ("cleanup-error", "first"),
    ] {
        let fixture = Fixture::new();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = root.join("tests/fixtures/claude-sdk/event_receipt.py");
        let action = if mode == "report" { "report" } else { "block" };
        let mut document = fixture.document();
        document["spec"]["agents"] =
            json!({"claude":{"adapter":"claude-sdk","argv":["node","worker.mjs"]}});
        document["spec"]["providers"] = json!({"policy":{
            "protocol":"aw-provider/v1alpha1", "transport":{"type":"stdio","location":"agent",
                "argv":[fixture.python,script,"provider"]},
            "timeout_ms":500,"max_output_bytes":4096,"config":{}}});
        document["spec"]["events"] = json!({"tool.before":{"enabled":true,"steps":[
            {"id":"first","provider":"policy","operation":"first","effects":["block"],"on_error":action},
            {"id":"last","provider":"policy","operation":"last","effects":["block"],"on_error":action}
        ]}});
        let service = Service::start(&fixture, &document);
        let binding: Binding = serde_json::from_value(
            service
                .client
                .call(
                    Operation::Bind {
                        target: "claude".into(),
                        capabilities: Capabilities {
                            adapter: "claude-sdk".into(),
                            version: "0.3.325".into(),
                            entrypoint: "sdk-worker".into(),
                            failure_actions: Default::default(),
                            events: BTreeMap::from([("tool.before".into(), vec!["block".into()])]),
                        },
                        cwd: fixture.root.to_str().unwrap().into(),
                        environment: Default::default(),
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
                "tool.before":{"budget_ms":1000,"steps":{"first":action,"last":action}}
            }})
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(fixture.root.join("mode"), mode).unwrap();
        fs::write(fixture.root.join("step"), step).unwrap();
        let mut command = std::process::Command::new(&fixture.python);
        command
            .arg(script)
            .arg(&fixture.root)
            .arg(root.join("../aw-adapter/native"))
            .arg(env!("CARGO_BIN_EXE_aw"));
        let output = fixture.run(command);
        assert!(
            output.status.success(),
            "{mode}/{step}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls: Vec<String> =
            serde_json::from_slice(&fs::read(fixture.root.join("rpc.json")).unwrap()).unwrap();
        let early =
            usize::from(step == "first" && matches!(mode, "deny" | "error" | "cleanup-error"));
        let recovery = usize::from(matches!(mode, "abort" | "cleanup-error"));
        assert_eq!(
            calls.iter().filter(|v| *v == "close_event").count(),
            early,
            "{mode}/{step}: {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|v| *v == "close_hook_event").count(),
            recovery,
            "{mode}/{step}: {calls:?}"
        );
        let deadline = Instant::now() + TIMEOUT;
        while mode != "cleanup-error" && service.status()["events"] != 0 {
            assert!(Instant::now() < deadline, "event leaked: {mode}/{step}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            service.status()["events"],
            usize::from(mode == "cleanup-error")
        );
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
