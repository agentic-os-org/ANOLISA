//! Every native callback dialect must acknowledge skipped steps without approving tools.

use super::*;
#[allow(dead_code)]
#[path = "../regression/tool_scope.rs"]
mod scope;

#[test]
fn all_native_dialects_close_skipped_callbacks_without_failure_blocks() {
    for adapter in native::agents() {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        document["spec"]["agents"] = json!({"agent":{"adapter":adapter,"argv":["/bin/true"]}});
        for id in ["first", "middle", "last"] {
            scope::step(
                &fixture,
                &mut document,
                "tool.before",
                id,
                &["*"],
                "native-error",
            );
        }
        document["spec"]["events"]["tool.before"]["match"] = json!({"tools":["file_read"]});
        let service = Service::start(&fixture, &document);
        let deadline = || Instant::now() + support::TIMEOUT;
        let binding: Binding = serde_json::from_value(
            service
                .client
                .call(
                    Operation::Bind {
                        target: "agent".into(),
                        capabilities: Capabilities {
                            failure_actions: Default::default(),
                            adapter: adapter.clone(),
                            version: "fixture".into(),
                            entrypoint: "fixture".into(),
                            events: BTreeMap::from([(
                                "tool.before".into(),
                                vec!["observe".into(), "block".into()],
                            )]),
                        },
                        cwd: fixture.root.to_str().unwrap().into(),
                        environment: BTreeMap::new(),
                    },
                    deadline(),
                )
                .unwrap(),
        )
        .unwrap();
        let path = fixture.root.join("binding.json");
        fs::write(&path, json!({"adapter":adapter,"binding":binding,"socket":fixture.root.join("state/aw.sock"),"cwd":fixture.root,
            "events":{"tool.before":{"budget_ms":5000,"steps":{"first":"block","middle":"block","last":"block"}}}}).to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let payload = native::callback(
            &adapter,
            "tool.before",
            "00000000-0000-4000-8000-000000000001",
            json!({"command":"fixture"}),
            &fixture.root,
        );
        for step in ["first", "middle", "last"] {
            let mut command = fixture.command();
            command
                .args(["hook", "--adapter", &adapter, "--binding"])
                .arg(&path)
                .args([
                    "--event",
                    "tool.before",
                    "--step",
                    step,
                    "--on-error",
                    "block",
                ]);
            let output = fixture.run_input(command, Some(&serde_json::to_vec(&payload).unwrap()));
            assert!(output.status.success(), "{adapter}/{step}: {output:?}");
            assert!(!blocked(&adapter, &output), "{adapter}/{step}: {output:?}");
            assert!(output.stderr.is_empty(), "{adapter}/{step}: {output:?}");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(!stdout.contains("approve"), "{adapter}: {stdout}");
            assert!(!stdout.contains("updatedInput"), "{adapter}: {stdout}");
        }
        scope::drained(&service);
        assert_eq!(service.status()["bindings"], 1);
        assert!(scope::invocations(&fixture).is_empty(), "{adapter}");
        scope::audit(
            &fixture,
            "tool.before",
            &[("first", true), ("middle", true), ("last", true)],
            "completed",
        );
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: binding.instance_id,
                },
                deadline(),
            )
            .unwrap();
        service.released(&fixture);
    }
}
