//! Exact OpenClaw selectors survive native policy aliases and completion phases.

use super::*;
use aw_service::{Binding, Capabilities, Operation};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Instant};

#[test]
fn exact_native_selectors_preserve_standalone_callback_names() {
    check_names(false);
}

#[test]
fn exact_native_selectors_preserve_replacement_completion_identity() {
    check_names(true);
}

fn check_names(replacement: bool) {
    for (raw, canonical) in [
        ("Custom/Tool", "custom/tool"),
        ("Bash", "exec"),
        ("apply-patch", "apply_patch"),
        ("CRON", "automations"),
    ] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        document["spec"]["agents"] = json!({"agent":{"adapter":"openclaw","argv":["/bin/true"]}});
        document["spec"]["events"] = json!({});
        let selector = format!("native:openclaw:{raw}");
        let other = format!("native:openclaw:{canonical}");
        for event in ["tool.before", "tool.after"] {
            scope::step(
                &fixture,
                &mut document,
                event,
                "other",
                &[&other],
                "native-error",
            );
            scope::step(
                &fixture,
                &mut document,
                event,
                "observe",
                &[&selector],
                "observe",
            );
        }
        if replacement {
            scope::step(
                &fixture,
                &mut document,
                "tool.after",
                "compress",
                &[&selector],
                "replace",
            );
        }
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
                            adapter: "openclaw".into(),
                            version: "fixture".into(),
                            entrypoint: "fixture".into(),
                            events: BTreeMap::from([
                                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                                (
                                    "tool.after".into(),
                                    vec!["observe".into(), "replace_result".into()],
                                ),
                            ]),
                        },
                        cwd: fixture.root.to_str().unwrap().into(),
                        environment: BTreeMap::new(),
                    },
                    deadline(),
                )
                .unwrap(),
        )
        .unwrap();
        let mut events = json!({
            "tool.before":{"budget_ms":10000,"steps":{"other":"block","observe":"block"}},
            "tool.after":{"budget_ms":10000,"steps":{"other":"report","observe":"report"}},
        });
        if replacement {
            events["tool.after"]["steps"]["compress"] = json!("report");
            events["tool.after"]["replacement_step"] = json!("compress");
        }
        let path = fixture.root.join("binding.json");
        fs::write(
            &path,
            json!({"adapter":"openclaw","binding":binding,
            "socket":fixture.root.join("state/aw.sock"),"cwd":fixture.root,"events":events})
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let invoke = |event: &str, step: &str, payload: &Value| {
            let mut command = fixture.command();
            command
                .args(["hook", "--adapter", "openclaw", "--binding"])
                .arg(&path)
                .args([
                    "--event",
                    event,
                    "--step",
                    step,
                    "--on-error",
                    if event == "tool.before" {
                        "block"
                    } else {
                        "report"
                    },
                ]);
            fixture.run_input(command, Some(&serde_json::to_vec(payload).unwrap()))
        };
        let mut before = native::callback(
            "openclaw",
            "tool.before",
            "call",
            json!({"command":"fixture"}),
            &fixture.root,
        );
        before["event"]["toolName"] = json!(raw);
        for step in ["other", "observe"] {
            let output = invoke("tool.before", step, &before);
            assert!(
                output.status.success() && output.stderr.is_empty(),
                "{raw}/{step}: {output:?}"
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                json!({})
            );
        }
        scope::drained(&service);
        let mut after = native::callback(
            "openclaw",
            "tool.after",
            "call",
            json!({"command":"fixture"}),
            &fixture.root,
        );
        after["event"]["toolName"] = json!(raw);
        let snapshot = after.to_string();
        let frame = |payload: &Value, phase: &str, snapshot: &str| {
            json!({
                "native_input":payload.to_string(),
                "aw_openclaw_completion":{"phase":phase,"snapshot":snapshot},
            })
        };
        if replacement {
            let output = invoke(
                "tool.after",
                "compress",
                &frame(&after, "replacement", &snapshot),
            );
            assert!(
                output.status.success() && output.stderr.is_empty(),
                "{raw}: {output:?}"
            );
            let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                reply["aw_replacement"]["content"][0]["text"], "compact",
                "{raw}: {reply}"
            );
            after["event"]["toolName"] = json!(canonical);
            after["event"]["durationMs"] = json!(37);
            // A completion cannot change the bound name, arguments, run or snapshot.
            for pointer in ["/event/toolName", "/event/params", "/event/runId"] {
                let mut changed = after.clone();
                *changed.pointer_mut(pointer).unwrap() = if pointer.ends_with("params") {
                    json!({"command":"changed"})
                } else {
                    json!("changed")
                };
                let rejected = invoke(
                    "tool.after",
                    "other",
                    &frame(&changed, "observation", &snapshot),
                );
                assert!(String::from_utf8_lossy(&rejected.stderr).contains("aw hook failed"));
            }
            let mut changed: Value = serde_json::from_str(&snapshot).unwrap();
            changed["event"]["toolName"] = json!(canonical);
            let rejected = invoke(
                "tool.after",
                "other",
                &frame(&after, "observation", &changed.to_string()),
            );
            assert!(String::from_utf8_lossy(&rejected.stderr).contains("aw hook failed"));
        }
        for step in ["other", "observe"] {
            let payload = if replacement {
                frame(&after, "observation", &snapshot)
            } else {
                after.clone()
            };
            let output = invoke("tool.after", step, &payload);
            assert!(
                output.status.success() && output.stderr.is_empty(),
                "{raw}/{step}: {output:?}"
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                json!({})
            );
        }
        scope::drained(&service);
        let requests = scope::invocations(&fixture);
        assert_eq!(
            requests.len(),
            if replacement { 3 } else { 2 },
            "{raw}: {requests:?}"
        );
        for request in requests {
            assert_eq!(request["event"]["tool"]["native_name"], raw);
            assert_ne!(request["label"], "other");
            let event = &request["event"];
            let expected = if event["name"] == "tool.after" {
                canonical
            } else {
                raw
            };
            assert_eq!(event["tool"]["name"], expected);
            if replacement && event["name"] == "tool.after" && request["label"] == "observe" {
                assert_eq!(event["native"]["event"]["toolName"], canonical);
                assert_eq!(event["native"]["event"]["durationMs"], 37);
                assert_eq!(event["tool"]["result"]["content"][0]["text"], "original");
            }
            assert_eq!(
                aw_adapter::association::association(
                    "openclaw",
                    event,
                    Some(&serde_json::to_vec(&event["native"]).unwrap()),
                    |bytes| serde_json::from_slice(bytes).ok()
                )["status"],
                "verified"
            );
        }
        scope::audit(
            &fixture,
            "tool.before",
            &[("other", true), ("observe", false)],
            "completed",
        );
        let expected = if replacement {
            vec![("compress", false), ("other", true), ("observe", false)]
        } else {
            vec![("other", true), ("observe", false)]
        };
        scope::audit(&fixture, "tool.after", &expected, "completed");
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
