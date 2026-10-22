//! Claude callback transport fixtures; real CLI adoption uses the opt-in Python test.
#![cfg(target_os = "linux")]
// Only the fixture service and metadata helpers are used by this callback target.
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;
use aw_service::{Binding, Capabilities, Operation};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, time::Instant};
use support::{Fixture, Service, TIMEOUT};

#[test]
fn claude_hook_cli_preserves_deny_success_failure_identity_and_audit() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["agents"] =
        json!({"claude":{"adapter":"claude-sdk","argv":["python3","worker.py"]}});
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    let binding: Binding = serde_json::from_value(
        service
            .client
            .call(
                Operation::Bind {
                    target: "claude".into(),
                    capabilities: Capabilities {
                        adapter: "claude-sdk".into(),
                        version: "0.2.88".into(),
                        entrypoint: "sdk-worker".into(),
                        events: BTreeMap::from([
                            ("tool.before".into(), vec!["observe".into(), "block".into()]),
                            ("tool.after".into(), vec!["observe".into()]),
                        ]),
                    },
                    cwd: fixture.root.to_str().unwrap().into(),
                    environment: fixture
                        .command()
                        .get_envs()
                        .filter_map(|(key, value)| {
                            value.map(|value| (key.to_owned(), value.to_owned()))
                        })
                        .collect(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap(),
    )
    .unwrap();
    let path = fixture.root.join("binding.json");
    fs::write(&path, json!({"adapter":"claude-sdk","binding":binding,"socket":fixture.root.join("state/aw.sock"),"cwd":fixture.root,"events":{
        "tool.before":{"budget_ms":5000,"steps":{"check":"block"}},"tool.after":{"budget_ms":5000,"steps":{"record":"report"}}
    }}).to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for (index, event, name, deny, expected) in [
        (0, "tool.before", "PreToolUse", false, 0),
        (1, "tool.before", "PreToolUse", true, 2),
        (0, "tool.after", "PostToolUse", false, 0),
        (2, "tool.before", "PreToolUse", false, 0),
        (2, "tool.after", "PostToolUseFailure", false, 0),
    ] {
        let payload = json!({"session_id":"native-session","cwd":fixture.root,"tool_use_id":format!("native-{index}"),"tool_name":"Bash","tool_input":{"command":"fixture","deny":deny},"hook_event_name":name,"scenario":if deny {"block"} else {"observe"},"tool_response":"native-success","error":"native-failure"});
        let input = fixture.root.join(format!("input-{index}-{event}.json"));
        fs::write(&input, payload.to_string()).unwrap();
        let mut command = fixture.command();
        command
            .args(["hook", "--adapter", "claude-sdk", "--binding"])
            .arg(&path)
            .args([
                "--event",
                event,
                "--step",
                if event == "tool.before" {
                    "check"
                } else {
                    "record"
                },
                "--on-error",
                if event == "tool.before" {
                    "block"
                } else {
                    "report"
                },
            ])
            .stdin(fs::File::open(input).unwrap());
        let result = command.output().unwrap();
        assert_eq!(
            result.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(fixture.root.join("state/journal")).unwrap() {
        let entries: Vec<Value> = fs::read_to_string(entry.unwrap().path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let Some(event) = entries[0]["record"]["plan"].get("event") else {
            continue;
        };
        let response = service
            .client
            .call(
                Operation::Audit {
                    key: entries[0]["event_key"].as_str().unwrap().into(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        assert!(response.is_object());
        found.insert(
            (
                event["tool"]["call_id"].as_str().unwrap().to_owned(),
                event["name"].as_str().unwrap().to_owned(),
            ),
            event["session_id"].clone(),
        );
    }
    assert_eq!(found.len(), 5);
    for index in [0, 2] {
        assert_eq!(
            found[&(format!("native-{index}"), "tool.before".into())],
            found[&(format!("native-{index}"), "tool.after".into())]
        );
    }
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
