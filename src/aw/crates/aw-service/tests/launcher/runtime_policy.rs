//! Reusable native hosts exercise aw run, explicit updates, callbacks and cleanup.

use super::support::{Fixture, Process, Service, TIMEOUT};
use aw_service::Operation;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

fn wait(path: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "fixture did not produce {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn command(fixture: &Fixture, index: usize, value: Value) {
    let path = fixture.root.join(format!("command-{index}.json"));
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, value.to_string()).unwrap();
    fs::rename(temporary, path).unwrap();
}

fn report(fixture: &Fixture, index: usize) -> Value {
    let path = fixture.root.join(format!("result-{index}.json"));
    wait(&path);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn update(fixture: &Fixture, candidate: &Value, extra: &[&str]) -> Output {
    let path = fixture.root.join("candidate.json");
    let bytes = serde_json::to_vec(candidate).unwrap();
    fs::write(&path, &bytes).unwrap();
    let mut command = fixture.command();
    command
        .args(["update", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--policy"])
        .arg(&path)
        .args(extra);
    let output = fixture.run(command);
    assert_eq!(fs::read(path).unwrap(), bytes);
    output
}

fn active(service: &Service) -> Value {
    service.status()["instances"][0].clone()
}

#[test]
fn reload_fixture_publishes_reports_only_after_writing_completes() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("settings.json"), br#"{"hooks":{}}"#).unwrap();
    command(&fixture, 0, json!({"action":"turn"}));
    command(&fixture, 1, json!({"action":"stop"}));
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/reload_host.py");
    let mut launch = Command::new(&fixture.python);
    launch
        .args([
            "-c",
            r#"
import os
from pathlib import Path
import runpy
import sys
import time

root = Path(os.environ["FAKE_ROOT"])
write_text = Path.write_text

def paused_write(path, text, *args, **kwargs):
    if path.stem != "result-0":
        return write_text(path, text, *args, **kwargs)
    with path.open("w", encoding="utf-8") as output:
        write_text(root / "writing-report", "ready")
        deadline = time.monotonic() + 10
        while not (root / "finish-report").exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("report writer was not released")
            time.sleep(0.005)
        return output.write(text)

Path.write_text = paused_write
source = sys.argv[1]
sys.argv = [source, "--settings", str(root / "settings.json")]
runpy.run_path(source, run_name="__main__")
"#,
        ])
        .arg(source)
        .current_dir(&fixture.root)
        .env_clear()
        .env("FAKE_ROOT", &fixture.root)
        .env("FAKE_RELOAD_ADAPTER", "qoder");
    let process = Process::spawn(&fixture, launch);
    wait(&fixture.root.join("writing-report"));
    assert!(
        !fixture.root.join("result-0.json").exists(),
        "an incomplete report became visible to its reader"
    );
    fs::write(fixture.root.join("finish-report"), b"continue").unwrap();
    let result = report(&fixture, 0);
    assert_eq!(result["executed"], true);
    assert!(!fixture.root.join("result-0.tmp").exists());
    let output = process.finish();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn prewarmed_qoder_and_qwenpaw_switch_before_dispatch_without_restarting() {
    for adapter in ["qoder", "qwenpaw"] {
        let fixture = Fixture::new();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/reload_host.py");
        let host = fixture.root.join("reusable host");
        fs::copy(source, &host).unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(fixture.root.join("home/config.json"), b"{}").unwrap();
        let mut baseline = fixture.document();
        baseline["spec"]["agents"]["qoder"] = json!({"adapter":adapter,"argv":if adapter=="qoder" {json!([host])} else {json!([host,"app"])}});
        if adapter == "qoder" {
            baseline["spec"]["agents"]["qoder"]["qoder"] = json!({"sequential":true});
        }
        fixture.observation(&mut baseline, "qoder");
        let service = Service::start(&fixture, &baseline);
        let original_config = fs::read(fixture.root.join("aw.json")).unwrap();
        let missing = update(&fixture, &baseline, &[]);
        assert!(!missing.status.success());
        assert!(String::from_utf8_lossy(&missing.stderr).contains("no matching instance"));
        let mut launch = fixture.command();
        launch
            .args(["run", "--config"])
            .arg(fixture.root.join("aw.json"))
            .args(["--agent", "qoder", "--reloadable"])
            .env("FAKE_RELOAD_ADAPTER", adapter)
            .env("QWENPAW_WORKING_DIR", fixture.root.join("home"));
        let process = Process::spawn(&fixture, launch);
        wait(&fixture.root.join("native-ready"));
        let native_pid: u32 = fs::read_to_string(fixture.root.join("native-ready"))
            .unwrap()
            .parse()
            .unwrap();
        let state = active(&service);
        let instance = state["instance_id"].as_str().unwrap();
        let initial = state["policy_revision"].as_str().unwrap();
        assert!(state["ready"].as_bool().unwrap());
        assert_eq!(state["process"]["pid"], native_pid);
        let context = aw_adapter::NativeContext {
            target: "qoder".into(),
            instance_id: instance.into(),
            cwd: fixture.root.clone(),
        };
        for index in 0..256 {
            let call = format!("observed-{index}");
            let native = if adapter == "qoder" {
                json!({"hook_event_name":"PreToolUse", "cwd":fixture.root,
                    "session_id":"session", "tool_use_id":call,
                    "tool_name":"custom_tool", "tool_input":{}})
            } else {
                json!({"event":"tool.before", "session_id":"session",
                    "tool_call":{"id":call,"name":"custom_tool","input":"{}"}})
            };
            let event = aw_adapter::select(adapter)
                .unwrap()
                .normalize(&context, "tool.before", &native)
                .unwrap();
            service
                .client
                .call(
                    Operation::ObserveHookEvent {
                        instance_id: instance.into(),
                        event,
                        native_input: serde_json::to_vec(&native).unwrap(),
                    },
                    Instant::now() + TIMEOUT,
                )
                .unwrap();
        }
        command(&fixture, 0, json!({"action":"turn"}));
        let observed = report(&fixture, 0);
        assert_eq!(observed["executed"], true, "{adapter}: {observed}");
        assert!(service.status()["observations"][0]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap == "observation_callback_limit"));
        // Resolve all matches, never silently choose one ready/reloadable instance.
        let other = service
            .client
            .call(
                Operation::Bind {
                    target: "qoder".into(),
                    capabilities: aw_service::Capabilities {
                        adapter: adapter.into(),
                        version: state["native_version"].as_str().unwrap().into(),
                        entrypoint: state["entrypoint"].as_str().unwrap().into(),
                        events: Default::default(),
                    },
                    cwd: fixture.root.to_str().unwrap().into(),
                    environment: Default::default(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        let ambiguous = update(&fixture, &baseline, &[]);
        assert!(!ambiguous.status.success());
        assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("multiple matching instances"));
        let stale = update(
            &fixture,
            &baseline,
            &[
                "--instance",
                instance,
                "--expected-revision",
                &"0".repeat(64),
            ],
        );
        assert!(!stale.status.success());
        assert!(String::from_utf8_lossy(&stale.stderr).contains("policy_revision_conflict"));
        let mut explicit = fixture.command();
        explicit
            .args(["update", "--socket"])
            .arg(fixture.root.join("state/aw.sock"))
            .args([
                "--instance",
                instance,
                "--expected-revision",
                initial,
                "--policy",
            ])
            .arg(fixture.root.join("candidate.json"));
        assert!(fixture.run(explicit).status.success());
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: other["instance_id"].as_str().unwrap().into(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        // The default policy has no hooks; activation precedes the first request.
        let mut candidate = baseline.clone();
        fixture.provider(&mut candidate);
        if adapter == "qwenpaw" {
            candidate["spec"]["providers"]["policy"]["transport"]["argv"] = json!([
                fixture.python,
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qwenpaw/action.py"),
                fixture.root,
                "provider",
                "policy"
            ]);
        }
        candidate["spec"]["providers"]["compress"] = json!({"protocol":"aw-provider/v1alpha1",
            "transport":{"type":"stdio","location":"agent","argv":[fixture.python,fixture.action(),"--root",fixture.root,"--protocol","provider","--behavior","replace"]},
            "timeout_ms":4500,"max_output_bytes":4096,"config":{}});
        candidate["spec"]["events"]["tool.after"]["steps"].as_array_mut().unwrap().push(json!({"id":"compress","provider":"compress","operation":"record","effects":["replace_result"],"on_error":"report"}));
        let changed = update(&fixture, &candidate, &[]);
        assert!(
            changed.status.success(),
            "{}",
            String::from_utf8_lossy(&changed.stderr)
        );
        let changed: Value = serde_json::from_slice(&changed.stdout).unwrap();
        let audit_key = changed["audit_key"].as_str().unwrap();
        let updater = fs::read_dir(&fixture.root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("process-")
                    && path
                        .extension()
                        .is_some_and(|extension| extension == "json")
            })
            .map(|path| serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap())
            .find(|receipt| {
                fs::read_to_string(receipt["stdout"].as_str().unwrap())
                    .unwrap()
                    .contains(audit_key)
            })
            .expect("update process receipt");
        let audit = service
            .client
            .call(
                Operation::Audit {
                    key: audit_key.into(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        let seed = &audit["records"][0]["record"]["plan"];
        assert_eq!(seed["caller"]["pid"], updater["pid"]);
        assert_eq!(
            seed["caller"]["uid"],
            fs::metadata(&fixture.root).unwrap().uid()
        );
        assert_ne!(seed["caller"]["pid"], service.status()["pid"]);
        let revision = changed["policy_revision"].as_str().unwrap();
        assert_ne!(revision, initial);
        command(&fixture, 1, json!({"action":"turn","scenario":"block"}));
        let denied = report(&fixture, 1);
        assert_eq!(denied["executed"], false, "{adapter}: {denied}");
        assert_eq!(denied["pid"], native_pid);
        // No tools are active yet: an in-flight model wait alone must return busy.
        command(&fixture, 2, json!({"action":"turn","hold":true}));
        wait(&fixture.root.join("entered-2"));
        let busy = update(&fixture, &baseline, &[]);
        assert!(!busy.status.success());
        assert!(String::from_utf8_lossy(&busy.stderr).contains("busy"));
        assert!(busy.stdout.is_empty());
        fs::write(fixture.root.join("continue-2"), b"continue").unwrap();
        let held = report(&fixture, 2);
        assert_eq!(
            held["executed"],
            true,
            "{adapter}: {held}; audit {:?}",
            service
                .client
                .call(Operation::RecentExecutions, Instant::now() + TIMEOUT)
        );
        if adapter == "qoder" {
            let reply: Value =
                serde_json::from_str(held["after"][0]["stdout"].as_str().unwrap()).unwrap();
            assert_eq!(reply["hookSpecificOutput"]["updatedToolOutput"], "compact");
        } else {
            assert_eq!(held["output"][0]["content"][0]["text"], "compact");
            assert_eq!(held["output"][0]["metadata"]["kept"], true);
        }
        assert_eq!(active(&service)["policy_revision"], revision);
        let removed = update(&fixture, &baseline, &[]);
        assert!(
            removed.status.success(),
            "{}",
            String::from_utf8_lossy(&removed.stderr)
        );
        command(&fixture, 3, json!({"action":"turn","scenario":"block"}));
        let allowed = report(&fixture, 3);
        assert_eq!(allowed["executed"], true);
        assert_eq!(allowed["pid"], native_pid);
        assert_eq!(active(&service)["policy_revision"], initial);
        assert_eq!(
            fs::read(fixture.root.join("aw.json")).unwrap(),
            original_config
        );
        command(&fixture, 4, json!({"action":"stop"}));
        let output = process.finish();
        assert!(
            output.status.success(),
            "{adapter}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        service.released(&fixture);
        if adapter == "qwenpaw" {
            assert!(!fixture.root.join("home/plugins/aw-native").exists());
        }
        assert_eq!(
            service
                .client
                .call(Operation::Status, Instant::now() + TIMEOUT)
                .unwrap()["bindings"],
            0
        );
    }
}

#[test]
fn reload_optin_rejects_unverified_native_modes_before_starting_a_service() {
    for print in [false, true] {
        let fixture = Fixture::new();
        let document = fixture.document();
        fixture.save(&document);
        let mut command = fixture.command();
        command
            .args(["run", "--config"])
            .arg(fixture.root.join("aw.json"))
            .args(["--agent", "qoder", "--reloadable"]);
        if print {
            command
                .env("FAKE_VERSION", "1.1.73")
                .args(["--", "-p", "unused"]);
        }
        let output = fixture.run(command);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("runtime policy updates require"));
        assert!(!fixture.root.join("state").exists());
        assert!(!fixture.root.join("started-one").exists());
    }
}

#[test]
fn qoder_dynamic_hooks_preserve_permission_and_additional_context() {
    let fixture = Fixture::new();
    let host = fixture.root.join("reusable host");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/reload_host.py"),
        &host,
    )
    .unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    let mut baseline = fixture.document();
    baseline["spec"]["agents"]["qoder"] =
        json!({"adapter":"qoder", "argv":[host], "qoder":{"sequential":true}});
    let service = Service::start(&fixture, &baseline);
    let mut launch = fixture.command();
    launch
        .args(["run", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--reloadable"])
        .env("FAKE_RELOAD_ADAPTER", "qoder");
    let process = Process::spawn(&fixture, launch);
    wait(&fixture.root.join("native-ready"));
    let identity = active(&service)["process"].clone();
    for (index, decisions) in [
        [
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}),
        ],
        [
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
        ],
        [
            json!({"decision":"deny", "hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            json!({"decision":"allow"}),
        ],
        [
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
            json!({"decision":"approve"}),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let mut candidate = baseline.clone();
        for (event, replies) in [
            ("tool.before", decisions),
            (
                "tool.after",
                [
                    json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"first-context"}}),
                    json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"second-context"}}),
                ],
            ),
        ] {
            candidate["spec"]["events"][event] = json!({"enabled":true, "steps":[]});
            for (number, reply) in replies.into_iter().enumerate() {
                let id = format!(
                    "hook-{}-{number}",
                    if event == "tool.before" {
                        "pre"
                    } else {
                        "post"
                    }
                );
                candidate["spec"]["providers"][&id] = json!({"protocol":"native-hook/v1alpha1",
                    "transport":{"type":"stdio","location":"agent","argv":[fixture.python,"-c","import sys; print(sys.argv[1])", reply.to_string()]},
                    "timeout_ms":1000,"max_output_bytes":4096,"config":{}});
                candidate["spec"]["events"][event]["steps"].as_array_mut().unwrap().push(json!({"id":id,"provider":id,"native":{},"on_error":if event == "tool.before" {"block"} else {"report"}}));
            }
        }
        let changed = update(&fixture, &candidate, &[]);
        assert!(
            changed.status.success(),
            "{}",
            String::from_utf8_lossy(&changed.stderr)
        );
        command(&fixture, index, json!({"action":"turn"}));
        let result = report(&fixture, index);
        let before: Value =
            serde_json::from_str(result["before"][0]["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(result["before"][0]["code"], 0);
        assert_eq!(
            before["hookSpecificOutput"]["permissionDecision"]
                .as_str()
                .or(before["decision"].as_str()),
            Some("ask")
        );
        let after: Value =
            serde_json::from_str(result["after"][0]["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(
            after["hookSpecificOutput"]["additionalContext"],
            "first-context\nsecond-context"
        );
        assert_eq!(active(&service)["process"], identity);
        assert_eq!(active(&service)["tools"], 0);
    }
    command(&fixture, 4, json!({"action":"stop"}));
    assert!(process.finish().status.success());
    service.released(&fixture);
}
