//! Supported native result callbacks retain their envelope when compression does not match.

use super::*;
use aw_service::{Binding, Capabilities, Operation};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Instant};

#[test]
fn skipped_replacement_is_neutral_for_claude_and_qwen_callbacks() {
    for adapter in ["claude-sdk", "qwenpaw"] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        document["spec"]["agents"] = json!({"agent":{"adapter":adapter,"argv":["/bin/true"]}});
        scope::step(
            &fixture,
            &mut document,
            "tool.after",
            "observe",
            &["*"],
            "observe",
        );
        scope::step(
            &fixture,
            &mut document,
            "tool.after",
            "compress",
            &["file_read"],
            "replace",
        );
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
                            adapter: adapter.into(),
                            version: "fixture".into(),
                            entrypoint: "fixture".into(),
                            events: BTreeMap::from([(
                                "tool.after".into(),
                                vec!["observe".into(), "replace_result".into()],
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
            "events":{"tool.after":{"budget_ms":5000,"replacement_step":"compress","steps":{"observe":"report","compress":"report"}}}}).to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut payload = native::callback(
            adapter,
            "tool.after",
            "00000000-0000-4000-8000-000000000001",
            json!({"command":"fixture"}),
            &fixture.root,
        );
        payload["tool_response"]["fixture_metadata"] = json!({"retained":true});
        for step in ["observe", "compress"] {
            let mut command = fixture.command();
            command
                .args(["hook", "--adapter", adapter, "--binding"])
                .arg(&path)
                .args([
                    "--event",
                    "tool.after",
                    "--step",
                    step,
                    "--on-error",
                    "report",
                ]);
            let output = fixture.run_input(command, Some(&serde_json::to_vec(&payload).unwrap()));
            assert!(output.status.success(), "{adapter}/{step}: {output:?}");
            assert!(output.stderr.is_empty(), "{adapter}/{step}: {output:?}");
            if !output.stdout.is_empty() {
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                    json!({}),
                    "{adapter}/{step}"
                );
            }
        }
        scope::drained(&service);
        assert_eq!(service.status()["bindings"], 1);
        let requests = scope::invocations(&fixture);
        assert_eq!(requests.len(), 1, "{adapter}: {requests:?}");
        assert_eq!(requests[0]["label"], "observe");
        assert_eq!(
            requests[0]["event"]["tool"]["result"],
            payload["tool_response"]
        );
        scope::audit(
            &fixture,
            "tool.after",
            &[("observe", false), ("compress", true)],
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
