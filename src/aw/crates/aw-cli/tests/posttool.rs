//! Exercise bound result replacement through the real service and callback CLI.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

#[path = "regression/native.rs"]
mod native;

use serde_json::{json, Value};
use std::fs;
use support::{Fixture, Service};

#[test]
fn optional_projection_does_not_remove_large_results_from_the_journal() {
    for bytes in [340_000, 400_000] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        document["spec"]["providers"] = json!({"compression":{
            "protocol":"aw-provider/v1alpha1","transport":{"type":"stdio","location":"agent","argv":[fixture.python,fixture.action(),"--root",fixture.root,"--protocol","provider","--behavior","replace"]},
            "timeout_ms":2000,"max_output_bytes":4096,"config":{}}});
        document["spec"]["events"] = json!({"tool.after":{"enabled":true,"steps":[{"id":"compress","provider":"compression","operation":"record","effects":["replace_result"],"on_error":"report"}]}});
        let service = Service::start(&fixture, &document);
        let mut command = fixture.launch(&[], None);
        command.env("FAKE_QODER_RESULT_BYTES", bytes.to_string());
        let report = fixture.successful(command);
        let hook = &report["events"][1]["hooks"][0];
        assert_eq!(hook["status"], 0);
        assert_eq!(hook["stderr"], "");
        let request: Value = fs::read_dir(&fixture.root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("provider-"))
            .map(|entry| serde_json::from_slice::<Value>(&fs::read(entry.path()).unwrap()).unwrap())
            .find(|request| request["method"] == "invoke")
            .unwrap();
        assert_eq!(
            request["event"]["tool"].get("result_projection").is_some(),
            bytes == 340_000
        );
        let mut journaled = false;
        for entry in fs::read_dir(fixture.root.join("state/journal")).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains(&"x".repeat(128)));
            let records: Vec<Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            if records[0]["record"]["plan"]["kind"] == "event" {
                journaled = true;
                assert_eq!(records.last().unwrap()["record"]["status"], "completed");
            }
        }
        assert!(journaled);
        service.released(&fixture);
    }
}

#[test]
fn qoder_waiting_after_steps_cannot_renew_an_exhausted_budget() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["execution"]["default_event_budget_ms"] = json!(300);
    document["spec"]["providers"] = json!({
        "observer":{"protocol":"native-hook/v1alpha1","config":{},"transport":{"type":"stdio","location":"agent","argv":[fixture.python,"-c","import os,pathlib,sys,time; sys.stdin.read(); pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); time.sleep(2)",fixture.root.join("sleeper.pid")]},"timeout_ms":2000,"max_output_bytes":4096},
        "compression":{"protocol":"aw-provider/v1alpha1","config":{},"transport":{"type":"stdio","location":"agent","argv":[fixture.python,fixture.action(),"--root",fixture.root,"--protocol","provider","--behavior","replace"]},"timeout_ms":2000,"max_output_bytes":4096}
    });
    document["spec"]["events"] = json!({"tool.after":{"enabled":true,"steps":[
        {"id":"observe","provider":"observer","native":{},"on_error":"report"},
        {"id":"compress","provider":"compression","operation":"record","effects":["replace_result"],"on_error":"report"}
    ]}});
    let service = Service::start(&fixture, &document);
    let mut command = fixture.launch(&[], None);
    command.env("FAKE_QODER_RESULT_TEXT", "original");
    let report = fixture.successful(command);
    for hook in report["events"][1]["hooks"].as_array().unwrap() {
        assert_eq!(hook["status"], 0);
        assert!(!hook["stderr"].as_str().unwrap().is_empty());
        assert_eq!(hook["stdout"], "7b7d");
    }
    let pid = fs::read_to_string(fixture.root.join("sleeper.pid")).unwrap();
    assert!(!std::path::Path::new("/proc").join(pid).exists());
    for entry in fs::read_dir(&fixture.root)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("provider-"))
    {
        let request: Value = serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap();
        assert_ne!(request["method"], "invoke");
    }
    service.released(&fixture);
}

#[test]
fn qoder_callback_delivers_one_replacement_and_keeps_audit_metadata_only() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["providers"] = json!({"compression":{
        "protocol":"aw-provider/v1alpha1", "transport":{"type":"stdio","location":"agent","argv":[fixture.python,fixture.action(),"--root",fixture.root,"--protocol","provider","--behavior","replace"]},
        "config":{},"timeout_ms":2000,"max_output_bytes":4096}});
    document["spec"]["events"] = json!({"tool.after":{"enabled":true,"steps":[{"id":"compress","provider":"compression","operation":"record","effects":["replace_result"],"on_error":"report"}]}});
    let mut observer = document["spec"]["providers"]["compression"].clone();
    *observer["transport"]["argv"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap() = json!("observe");
    document["spec"]["providers"]["security"] = observer;
    document["spec"]["providers"]["broken"] = json!({"transport":{"type":"stdio","location":"agent","argv":[fixture.python,"-c","print('invalid boolean')"]},"timeout_ms":1000,"max_output_bytes":32});
    document["spec"]["providers"]["user"] = json!({"protocol":"native-hook/v1alpha1","config":{},"transport":{"type":"stdio","location":"agent","argv":[fixture.python,fixture.action(),"--root",fixture.root,"--protocol","native","--label","user","--behavior","barrier"]},"timeout_ms":4500,"max_output_bytes":4096});
    let after = document["spec"]["events"]["tool.after"]["steps"]
        .as_array_mut()
        .unwrap();
    after.insert(
        0,
        json!({"id":"user","provider":"user","native":{},"on_error":"report"}),
    );
    after.insert(0,json!({"id":"observe","provider":"security","operation":"record","effects":["observe"],"on_error":"report"}));
    after.insert(
        1,
        json!({"id":"broken","provider":"broken","effects":["observe"],"on_error":"report"}),
    );
    document["spec"]["agents"]["qoder"]["qoder"] = json!({"sequential":false});
    let service = Service::start(&fixture, &document);
    let native_settings = fixture.root.join("external.json");
    fs::write(&native_settings, json!({"hooks":{"PostToolUse":[{"matcher":"*","hooks":[{"type":"command","command":fixture.python,"args":[fixture.action(),"--root",fixture.root,"--protocol","native","--label","external","--behavior","barrier"]}]}]}}).to_string()).unwrap();
    let mut launch = fixture.launch(&[], Some(&native_settings));
    launch.env("FAKE_QODER_RESULT_TEXT", "original");
    let report = fixture.successful(launch);
    let hooks = report["events"].as_array().unwrap()[1]["hooks"]
        .as_array()
        .unwrap();
    assert_eq!(
        hooks
            .iter()
            .map(|hook| hook["step"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["existing", "observe", "broken", "user", "compress"]
    );
    assert!(hooks.iter().all(|hook| hook["status"] == 0));
    let settings: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("settings-one.json")).unwrap()).unwrap();
    assert_eq!(
        settings["hooks"]["PostToolUse"][0]["sequential"],
        Value::Null
    );
    assert_eq!(settings["hooks"]["PostToolUse"][1]["sequential"], false);
    for label in ["external", "user"] {
        assert!(fixture.root.join(format!("overlap-one-{label}")).exists());
    }
    let hook = hooks.last().unwrap();
    assert!(!hooks[2]["stderr"].as_str().unwrap().is_empty());
    assert_eq!(hook["status"], 0);
    let encoded = hook["stdout"].as_str().unwrap();
    let bytes: Vec<u8> = encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let output: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(output["hookSpecificOutput"]["updatedToolOutput"], "compact");
    assert!(output.get("additionalContext").is_none());
    let mut started = Vec::new();
    for file in fs::read_dir(fixture.root.join("state/journal")).unwrap() {
        let path = file.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            let journal = fs::read_to_string(path).unwrap();
            assert!(!journal.contains("compact"));
            assert!(!journal.contains("original"));
            for line in journal.lines() {
                let record: Value = serde_json::from_str(line).unwrap();
                if record["record"]["phase"] == "started" {
                    started.push(record["record"]["step_id"].as_str().unwrap().to_owned());
                }
            }
        }
    }
    assert_eq!(started, ["observe", "broken", "user", "compress"]);
    service.released(&fixture);
}

#[test]
fn adapters_keep_observers_and_user_hooks_before_one_final_replacement() {
    use aw_service::{Binding, Capabilities, Operation};
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Instant};
    for agent in native::agents() {
        let adapter = agent.as_str();
        let fixture = Fixture::new();
        let mut document = fixture.document();
        document["spec"]["agents"] =
            json!({"agent":{"adapter":adapter,"argv":["/unused-native-agent"]}});
        for (name, behavior) in [("security", "observe"), ("tokenless", "replace")] {
            document["spec"]["providers"][name] = json!({"protocol":"aw-provider/v1alpha1", "transport":{"type":"stdio","location":"agent","argv":[fixture.python,fixture.action(),"--root",fixture.root,"--protocol","provider","--label",name,"--behavior",behavior]},"config":{},"timeout_ms":2000,"max_output_bytes":65536});
            // A broken optional view must never participate in event preparation
            // or alter observation/replacement in any native Adapter.
            document["spec"]["providers"][name]["view"] = json!({
                "protocol":"aw-provider-view/v1alpha1","implementation":name,
                "transport":{"type":"stdio","location":"agent","argv":["/view-must-not-run"]}});
        }
        document["spec"]["providers"]["custom"] = json!({"transport":{"type":"stdio","location":"agent","argv":[fixture.python,"-c","print('true')"]},"timeout_ms":1000,"max_output_bytes":32});
        document["spec"]["providers"]["user"] = json!({"protocol":"native-hook/v1alpha1","config":{},"transport":{"type":"stdio","location":"agent","argv":[fixture.python,fixture.action(),"--root",fixture.root,"--protocol","native","--label","user"]},"timeout_ms":1000,"max_output_bytes":4096});
        document["spec"]["events"] = json!({"tool.after":{"enabled":true,"steps":[
            {"id":"security","provider":"security","operation":"record","effects":["observe"],"on_error":"report"},
            {"id":"custom","provider":"custom","effects":["observe"],"on_error":"report"},
            {"id":"user","provider":"user","native":{},"on_error":"report"},
            {"id":"compress","provider":"tokenless","operation":"record","effects":["replace_result"],"on_error":"report"}]}});
        let service = Service::start(&fixture, &document);
        let deadline = || Instant::now() + support::TIMEOUT;
        let binding: Binding = serde_json::from_value(
            service
                .client
                .call(
                    Operation::Bind {
                        target: "agent".into(),
                        capabilities: Capabilities {
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
        fs::write(&path,json!({"adapter":adapter,"binding":binding,"socket":fixture.root.join("state/aw.sock"),"cwd":fixture.root,"events":{"tool.after":{"budget_ms":5000,"replacement_step":"compress","steps":{"security":"report","custom":"report","user":"report","compress":"report"}}}}).to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let input = json!({"command":"printf original"});
        let mut native = native::callback(
            adapter,
            "tool.after",
            "00000000-0000-4000-8000-000000000001",
            input,
            &fixture.root,
        );
        native["run_id"] = json!("coexist");
        let mut original = serde_json::to_vec(&native).unwrap();
        let snapshot = String::from_utf8(original.clone()).unwrap();
        let order = if adapter == "openclaw" {
            ["compress", "security", "custom", "user"]
        } else {
            ["security", "custom", "user", "compress"]
        };
        for step in order {
            if adapter == "openclaw" && step != "compress" {
                native["event"]["toolName"] = json!("custom/tool");
                native["event"]["durationMs"] = json!(37);
                native["event"]["nativeExtra"] = json!("preserved");
                original = serde_json::to_vec(&native).unwrap();
            }
            if adapter == "openclaw" && step == "custom" {
                for change_snapshot in [false, true] {
                    let mut altered = native.clone();
                    let mut altered_snapshot: Value = serde_json::from_str(&snapshot).unwrap();
                    if change_snapshot {
                        altered_snapshot["event"]["result"]["content"][0]["text"] =
                            json!("tampered");
                    } else {
                        altered["event"]["durationMs"] = json!(38);
                    }
                    let frame = json!({"native_input":altered.to_string(),"aw_openclaw_completion":{"phase":"observation","snapshot":altered_snapshot.to_string()}});
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
                    let rejected =
                        fixture.run_input(command, Some(&serde_json::to_vec(&frame).unwrap()));
                    assert!(String::from_utf8_lossy(&rejected.stderr).contains("aw hook failed"));
                }
            }
            if adapter == "hermes" && step == "compress" {
                for pointer in ["/extra/result", "/extra/duration_ms"] {
                    let mut changed = native.clone();
                    *changed.pointer_mut(pointer).unwrap() = json!("changed");
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
                    let rejected =
                        fixture.run_input(command, Some(&serde_json::to_vec(&changed).unwrap()));
                    assert!(!String::from_utf8_lossy(&rejected.stdout).contains("compact"));
                    assert!(String::from_utf8_lossy(&rejected.stderr).contains("aw hook failed"));
                }
            }
            if adapter == "hermes" && step == "compress" {
                native["hook_event_name"] = json!("transform_tool_result");
                native["extra"]
                    .as_object_mut()
                    .unwrap()
                    .remove("middleware_trace");
            }
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
            let bytes = if adapter == "openclaw" {
                serde_json::to_vec(&json!({"native_input":native.to_string(),"aw_openclaw_completion":{"phase":if step == "compress" {"replacement"} else {"observation"},"snapshot":snapshot}})).unwrap()
            } else {
                serde_json::to_vec(&native).unwrap()
            };
            let output = fixture.run_input(command, Some(&bytes));
            assert!(
                output.status.success(),
                "{adapter}/{step}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stderr.is_empty(),
                "{adapter}/{step}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let reply: Value = if output.stdout.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&output.stdout).unwrap()
            };
            if step == "compress" {
                assert!(reply.to_string().contains("compact"), "{adapter}: {reply}");
            } else {
                assert!(!reply.to_string().contains("compact"));
            }
        }
        assert_eq!(
            fs::read(fixture.root.join("native-coexist-user.bin")).unwrap(),
            original
        );
        let requests: Vec<Value> = fs::read_dir(&fixture.root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|e| e.file_name().to_string_lossy().starts_with("provider-"))
            .map(|e| serde_json::from_slice(&fs::read(e.path()).unwrap()).unwrap())
            .filter(|v: &Value| v["method"] == "invoke")
            .collect();
        assert_eq!(requests.len(), 2);
        for request in requests {
            if request["allowed_effects"] == json!(["replace_result"]) {
                assert_eq!(
                    request["event"]["tool"]["result_projection"]["text"],
                    "original"
                );
            } else {
                assert!(request["event"]["tool"].get("result_projection").is_none());
                if adapter == "openclaw" {
                    assert_eq!(request["event"]["native"]["event"]["durationMs"], 37);
                    assert_eq!(
                        request["event"]["native"]["event"]["nativeExtra"],
                        "preserved"
                    );
                    assert!(request["event"]["native"].get("aw_completion").is_none());
                }
            }
        }
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
