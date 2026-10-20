//! Codex launcher contracts; pinned CLI/model acceptance runs separately in nightly.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use serde_json::{json, Value};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use support::{Fixture, Process, Service, TIMEOUT};

fn setup(f: &Fixture) -> Value {
    fs::set_permissions(f.root.join("home"), fs::Permissions::from_mode(0o700)).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex");
    let cli = f.root.join("codex");
    fs::copy(source.join("cli.py"), &cli).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    let mut doc = f.document();
    doc["spec"]["agents"] = json!({"codex":{"adapter":"codex","argv":[cli]}});
    doc["spec"]["providers"] = json!({"policy":{
        "protocol":"aw-provider/v1alpha1","transport":{"type":"stdio","location":"agent","argv":[f.python,source.join("policy.py")]},
        "config":{},"timeout_ms":2000,"max_output_bytes":4096}});
    doc["spec"]["events"] = json!({
        "tool.before":{"enabled":true,"steps":[
            {"id":"z-security","provider":"policy","operation":"scan","effects":["observe","block"],"on_error":"block"},
            {"id":"a-retry","provider":"policy","operation":"retry","effects":["observe","block"],"on_error":"report"}]},
        "tool.after":{"enabled":true,"steps":[
            {"id":"record","provider":"policy","operation":"record","effects":["observe"],"on_error":"report"}]}});
    f.save(&doc);
    doc
}

fn command(f: &Fixture, verb: &str) -> Command {
    let mut command = f.command();
    command.env("CODEX_HOME", f.root.join("home"));
    command
        .args([verb, "--config"])
        .arg(f.root.join("aw.json"))
        .args(["--agent", "codex"]);
    command
}

fn install(f: &Fixture) {
    let output = f.run(command(f, "install"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn native_launch_preserves_config_serializes_steps_and_cleans_instances() {
    for (mode, calls) in [
        ("allow", "scan\nretry\nrecord\n"),
        ("block", "scan\n"),
        ("retry", "scan\nretry\n"),
        ("rtk candidate", "scan\n"),
        ("error", "scan\n"),
    ] {
        let f = Fixture::new();
        let doc = setup(&f);
        let service = Service::start(&f, &doc);
        let user = json!({"description":"keep user settings","hooks":{"PreToolUse":[{
            "matcher":"Bash","hooks":[{"type":"command","command":"printf '{}'","timeout":3}]}]}});
        let hooks = f.root.join("home/hooks.json");
        fs::write(&hooks, serde_json::to_vec(&user).unwrap()).unwrap();
        fs::set_permissions(&hooks, fs::Permissions::from_mode(0o600)).unwrap();
        install(&f);
        let installed = fs::read(&hooks).unwrap();
        install(&f);
        assert_eq!(installed, fs::read(&hooks).unwrap());
        let parsed: Value = serde_json::from_slice(&installed).unwrap();
        assert_eq!(parsed["description"], user["description"]);
        assert_eq!(
            parsed["hooks"]["PreToolUse"][0],
            user["hooks"]["PreToolUse"][0]
        );
        let mut launch = command(&f, "run");
        launch.args(["--", mode]);
        let output = f.run(launch);
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(f.root.join("policy-calls").exists(), "{mode}: {result}");
        assert_eq!(
            fs::read_to_string(f.root.join("policy-calls")).unwrap(),
            calls
        );
        assert_eq!(f.root.join("codex-tool-executed").exists(), mode == "allow");
        if mode == "allow" {
            assert_eq!(
                result["response"],
                json!({"stdout":"original output","stderr":"original error","exit_code":7})
            );
            assert_eq!(result["results"][2]["stdout"], "{}\n");
        }
        if mode == "retry" {
            assert!(result["results"][1]["stderr"]
                .as_str()
                .unwrap()
                .contains("Retry with rtk candidate"));
        }
        assert_eq!(
            serde_json::from_str::<Value>(
                &fs::read_to_string(f.root.join("codex-started")).unwrap()
            )
            .unwrap(),
            json!(["--no-daemon", mode])
        );
        assert_eq!(installed, fs::read(&hooks).unwrap());
        service.released(&f);
    }
}

#[test]
fn untrusted_disabled_and_unsupported_launches_never_start_tools() {
    for case in [
        "untrusted",
        "modified",
        "disabled",
        "version",
        "replace",
        "native",
        "args",
        "args-login",
        "args-delimiter",
        "missing",
        "duplicate",
        "hook-disabled",
        "async",
        "inventory-error",
    ] {
        let f = Fixture::new();
        let mut doc = setup(&f);
        if case == "replace" {
            doc["spec"]["events"]["tool.after"]["steps"][0]["effects"] = json!(["replace_result"]);
        }
        if case == "native" {
            doc["spec"]["providers"]["policy"]["protocol"] = json!("native-hook/v1alpha1");
            doc["spec"]["events"] = json!({"tool.before":{"enabled":true,"steps":[{"id":"native","provider":"policy","native":{},"on_error":"block"}]}});
        }
        f.save(&doc);
        let service = Service::start(&f, &doc);
        install(&f);
        let mut launch = command(&f, "run");
        match case {
            "untrusted" | "modified" => {
                launch.env("FAKE_CODEX_TRUST", case);
            }
            "disabled" => {
                launch.env("FAKE_CODEX_DISABLED", "1");
            }
            "version" => {
                launch.env("FAKE_CODEX_VERSION", "0.162.2");
            }
            "args" => {
                launch.args(["--", "--worktree"]);
            }
            "args-login" => {
                launch.args(["--", "login"]);
            }
            "args-delimiter" => {
                launch.env("FAKE_CODEX_TRUST", "untrusted");
                launch.args(["--", "--", "--dangerously-bypass-hook-trust"]);
            }
            "missing" | "duplicate" | "hook-disabled" | "async" | "inventory-error" => {
                launch.env("FAKE_CODEX_INVENTORY", case);
            }
            _ => {}
        }
        let output = f.run(launch);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!f.root.join("codex-started").exists(), "{case}");
        assert!(!f.root.join("policy-calls").exists(), "{case}");
        assert_eq!(service.status()["bindings"], 0);
    }
}

#[test]
fn interrupted_native_launch_releases_binding_and_owned_process() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let f = Fixture::new();
        let doc = setup(&f);
        let service = Service::start(&f, &doc);
        install(&f);
        let mut command = command(&f, "run");
        command.args(["--", "hold"]);
        let process = Process::spawn(&f, command);
        let deadline = Instant::now() + TIMEOUT;
        let marker = f.root.join("codex.pid");
        while !marker.exists() {
            assert!(Instant::now() < deadline, "Codex fixture never started");
            thread::sleep(Duration::from_millis(5));
        }
        let pid = fs::read_to_string(marker).unwrap();
        assert_eq!(service.status()["bindings"], 1);
        process.signal(signal);
        assert_eq!(process.finish().status.signal(), Some(signal));
        service.released(&f);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(!f.root.join("policy-calls").exists());
        assert!(!f.root.join("codex-tool-executed").exists());
    }
}

#[test]
fn interrupted_inventory_probe_reaps_child_without_binding() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let f = Fixture::new();
        let doc = setup(&f);
        let service = Service::start(&f, &doc);
        install(&f);
        let mut command = command(&f, "run");
        command.env("FAKE_CODEX_PROBE_HOLD", "1");
        let process = Process::spawn(&f, command);
        let deadline = Instant::now() + TIMEOUT;
        let marker = f.root.join("codex-probe.pid");
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "Codex inventory fixture never started"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let pid = fs::read_to_string(marker).unwrap();
        assert_eq!(service.status()["bindings"], 0);
        process.signal(signal);
        assert_eq!(process.finish().status.signal(), Some(signal));
        service.released(&f);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(!f.root.join("codex-started").exists());
        assert!(!f.root.join("policy-calls").exists());
    }
}
