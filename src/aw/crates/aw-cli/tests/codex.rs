//! Codex launcher contracts; pinned CLI/model acceptance runs separately in nightly.
#![cfg(target_os = "linux")]

#[path = "codex/app_server.rs"]
mod app_server;
#[path = "codex/audit.rs"]
mod audit;
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use audit::{assert_after_audit, assert_after_audit_unavailable};

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
        "input.submit":{"enabled":true,"steps":[
            {"id":"prompt-security","provider":"policy","operation":"scan_prompt","effects":["observe","block"],"on_error":"block"}]},
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
    let output = f.run(command(f, "connect"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn native_launch_preserves_config_serializes_steps_and_cleans_instances() {
    for (mode, calls) in [
        ("allow", "scan_prompt\nscan\nretry\nrecord\n"),
        ("block", "scan_prompt\nscan\n"),
        ("retry", "scan_prompt\nscan\nretry\n"),
        ("rtk candidate", "scan_prompt\nscan\n"),
        ("error", "scan_prompt\nscan\n"),
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
        let backup = f.root.join("home/hooks.json.aw-backup");
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&backup).unwrap()).unwrap(),
            user
        );
        fs::rename(backup, f.root.join("reviewed-hooks.json")).unwrap();
        install(&f);
        assert_eq!(installed, fs::read(&hooks).unwrap());
        let parsed: Value = serde_json::from_slice(&installed).unwrap();
        assert_eq!(parsed["description"], user["description"]);
        assert_eq!(
            parsed["hooks"]["PreToolUse"][0],
            user["hooks"]["PreToolUse"][0]
        );
        let mut launch = command(&f, "run");
        if mode == "allow" {
            fs::write(f.root.join("sitecustomize.py"),
                "import sys\nif sys.argv[0] == '-c': print('Codex inventory startup banner', flush=True)\n").unwrap();
            launch
                .env("PYTHONPATH", &f.root)
                .env("FAKE_CODEX_VERSION", "0.163.1");
        }
        launch.args(["--", "--", mode]);
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
            assert_eq!(result["results"][3]["stdout"], "{}\n");
            assert_after_audit(&f, &result["response"], "error", None);
        }
        if mode == "retry" {
            assert!(result["results"][2]["stderr"]
                .as_str()
                .unwrap()
                .contains("Retry with rtk candidate"));
        }
        assert_eq!(
            serde_json::from_str::<Value>(
                &fs::read_to_string(f.root.join("codex-started")).unwrap()
            )
            .unwrap(),
            json!(["--no-daemon", "--", mode])
        );
        assert_eq!(installed, fs::read(&hooks).unwrap());
        service.released(&f);
    }
}

#[test]
fn submission_denial_prevents_tool_execution_and_releases_event() {
    let f = Fixture::new();
    let doc = setup(&f);
    let service = Service::start(&f, &doc);
    install(&f);
    let mut launch = command(&f, "run");
    launch.args(["--", "--", "prompt-block"]);
    let output = f.run(launch);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["results"][0]["event"], "UserPromptSubmit");
    assert_eq!(result["results"][0]["code"], 2);
    assert_eq!(
        fs::read_to_string(f.root.join("policy-calls")).unwrap(),
        "scan_prompt\n"
    );
    assert!(!f.root.join("codex-tool-executed").exists());
    service.released(&f);
}

#[test]
fn native_replacement_returns_feedback_to_codex() {
    for (on_error, mode, expected_reply, failure_action) in [
        (
            "withhold_result",
            "replace",
            json!({"continue":false,"reason":"AW_REDACTED_RESULT"}),
            None,
        ),
        (
            "withhold_result",
            "replace-mcp",
            json!({"continue":false,"reason":"AW_REDACTED_RESULT"}),
            None,
        ),
        (
            "withhold_result",
            "replace-mcp-failure",
            json!({"continue":false,"reason":"AW result withheld"}),
            Some("withhold_result"),
        ),
        (
            "withhold_result",
            "replace-failure",
            json!({"continue":false,"reason":"AW result withheld"}),
            Some("withhold_result"),
        ),
        (
            "withhold_result",
            "replace-media",
            json!({"continue":false,"reason":"AW result withheld"}),
            None,
        ),
        (
            "report",
            "replace",
            json!({"continue":false,"reason":"AW_REDACTED_RESULT"}),
            None,
        ),
        (
            "report",
            "replace-mcp",
            json!({"continue":false,"reason":"AW_REDACTED_RESULT"}),
            None,
        ),
        ("report", "replace-mcp-failure", json!({}), Some("report")),
        ("report", "replace-failure", json!({}), Some("report")),
        ("report", "replace-media", json!({}), None),
    ] {
        let f = Fixture::new();
        let mut doc = setup(&f);
        doc["spec"]["events"]["tool.after"]["steps"][0]["effects"] =
            json!(["observe", "replace_result"]);
        doc["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!(on_error);
        f.save(&doc);
        let service = Service::start(&f, &doc);
        install(&f);
        let mut launch = command(&f, "run");
        launch.env("FAKE_CODEX_VERSION", "0.166.0");
        launch.args(["--", "--", mode]);
        let output = f.run(launch);
        assert!(
            output.status.success(),
            "{on_error}/{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        let reply: Value =
            serde_json::from_str(result["results"][2]["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(reply, expected_reply, "{on_error}/{mode}");
        assert_eq!(
            fs::read_to_string(f.root.join("policy-calls")).unwrap(),
            "scan_prompt\nscan\nretry\nrecord\n"
        );
        service.released(&f);
        if mode.starts_with("replace-mcp") || mode == "replace-media" {
            assert_eq!(result["response"]["isError"], false);
            assert_after_audit(&f, &result["response"], "success", failure_action);
        } else {
            assert_eq!(result["response"], "original output");
            assert_after_audit_unavailable(&f);
        }
    }
}

#[test]
fn bound_after_hook_withholds_when_registration_is_unreadable() {
    let f = Fixture::new();
    let mut hook = f.command();
    hook.args(["codex-hook", "--event", "tool.after"])
        .env("AW_CODEX_CONFIG", f.root.join("missing-chain.json"));
    let output = f.run(hook);
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"continue":false,"reason":"AW result withheld"})
    );
}

#[test]
fn untrusted_disabled_and_unsupported_launches_never_start_tools() {
    for case in [
        "untrusted",
        "modified",
        "disabled",
        "version",
        "native",
        "args",
        "args-login",
        "args-hidden",
        "args-alias",
        "args-delimiter",
        "missing",
        "duplicate",
        "hook-disabled",
        "async",
        "inventory-error",
        "peer-required",
    ] {
        let f = Fixture::new();
        let mut doc = setup(&f);
        if case == "native" {
            doc["spec"]["providers"]["policy"]["protocol"] = json!("native-hook/v1alpha1");
            doc["spec"]["events"] = json!({"tool.before":{"enabled":true,"steps":[{"id":"native","provider":"policy","native":{},"on_error":"block"}]}});
        }
        if case == "peer-required" {
            doc["spec"]["events"]["tool.before"]["required"] = json!(true);
        }
        f.save(&doc);
        let service = Service::start(&f, &doc);
        install(&f);
        if case == "peer-required" {
            let hooks = f.root.join("home/hooks.json");
            let mut value: Value = serde_json::from_slice(&fs::read(&hooks).unwrap()).unwrap();
            value["hooks"]["PreToolUse"]
                .as_array_mut()
                .unwrap()
                .push(json!({"matcher":"Bash","hooks":[{"type":"command","command":"printf '{}'","timeout":3}]}));
            fs::write(hooks, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let mut launch = command(&f, "run");
        match case {
            "untrusted" | "modified" => {
                launch.env("FAKE_CODEX_TRUST", case);
            }
            "disabled" => {
                launch.env("FAKE_CODEX_DISABLED", "1");
            }
            "version" => {
                launch.env("FAKE_CODEX_VERSION", "invalid");
            }
            "args" => {
                launch.args(["--", "--worktree"]);
            }
            "args-login" => {
                launch.args(["--", "login"]);
            }
            "args-hidden" => {
                launch.args(["--", "responses-api-proxy"]);
            }
            "args-alias" => {
                launch.args(["--", "e"]);
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

fn assert_reaped(marker: &Path) {
    let identity = fs::read_to_string(marker).unwrap();
    let (pid, started) = identity.split_once(' ').unwrap();
    let pid: u32 = pid.parse().unwrap();
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("cannot inspect Codex fixture {pid}: {error}"),
    };
    let (_, fields) = stat.rsplit_once(')').unwrap();
    let mut fields = fields.split_whitespace();
    let state = fields.next().unwrap();
    let starttime = fields.nth(18).unwrap();
    assert_ne!(
        starttime, started,
        "Codex fixture {pid} remains in state {state}"
    );
}

#[test]
fn interrupted_native_launch_releases_binding_and_owned_process() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let f = Fixture::new();
        let doc = setup(&f);
        let service = Service::start(&f, &doc);
        install(&f);
        let mut command = command(&f, "run");
        command.args(["--", "--", "hold"]);
        let process = Process::spawn(&f, command);
        let deadline = Instant::now() + TIMEOUT;
        let marker = f.root.join("codex.pid");
        while !marker.exists() {
            assert!(Instant::now() < deadline, "Codex fixture never started");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(service.status()["bindings"], 1);
        process.signal(signal);
        assert_eq!(process.finish().status.signal(), Some(signal));
        service.released(&f);
        assert_reaped(&marker);
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
        assert_eq!(service.status()["bindings"], 0);
        process.signal(signal);
        assert_eq!(process.finish().status.signal(), Some(signal));
        service.released(&f);
        assert_reaped(&marker);
        assert!(!f.root.join("codex-started").exists());
        assert!(!f.root.join("policy-calls").exists());
    }
}

#[test]
fn report_cannot_override_a_large_native_denial() {
    for (mode, event) in [
        ("large-prompt-block", "input.submit"),
        ("large-block", "tool.before"),
    ] {
        for bytes in [70_000, 1024 * 1024 - 1024] {
            let f = Fixture::new();
            let mut doc = setup(&f);
            doc["spec"]["providers"]["policy"]["config"]["block_bytes"] = json!(bytes);
            doc["spec"]["providers"]["policy"]["max_output_bytes"] = json!(1024 * 1024);
            doc["spec"]["events"][event]["steps"][0]["on_error"] = json!("report");
            f.save(&doc);
            let service = Service::start(&f, &doc);
            install(&f);
            let mut launch = command(&f, "run");
            launch.args(["--", "--", mode]);
            let output = f.run(launch);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            let rejection = result["results"].as_array().unwrap().last().unwrap();
            assert_eq!(rejection["code"], 2);
            assert_eq!(rejection["stderr"].as_str().unwrap().len(), bytes + 1);
            assert!(!f.root.join("codex-tool-executed").exists());
            service.released(&f);
        }
    }
}

#[test]
fn malformed_inventory_fails_before_binding_or_launch() {
    for (key, values) in [
        ("FAKE_CODEX_ERRORS", vec!["null", "{}", "\"\"", "false"]),
        (
            "FAKE_CODEX_FEATURES",
            vec![
                r#"{"code_mode":"true"}"#,
                r#"{"code_mode":1}"#,
                r#"{"code_mode":null}"#,
                r#"{"hooks":1}"#,
                r#"{"codex_hooks":"true"}"#,
                "[]",
            ],
        ),
    ] {
        for value in values {
            let f = Fixture::new();
            let doc = setup(&f);
            let service = Service::start(&f, &doc);
            install(&f);
            let mut launch = command(&f, "run");
            launch.env(key, value);
            let output = f.run(launch);
            assert_eq!(output.status.code(), Some(1), "{key}={value}");
            assert!(!f.root.join("codex-started").exists());
            assert!(!f.root.join("policy-calls").exists());
            assert_eq!(service.status()["bindings"], 0);
        }
    }
}

#[test]
fn schema_integer_budget_encodings_launch_the_same_chain() {
    for encoded in ["5000.0", "5e3"] {
        let f = Fixture::new();
        let mut doc = setup(&f);
        let budget: Value = serde_json::from_str(encoded).unwrap();
        doc["spec"]["execution"]["default_event_budget_ms"] = budget.clone();
        doc["spec"]["events"]["tool.before"]["budget_ms"] = budget;
        f.save(&doc);
        let path = f.root.join("aw.json");
        let mut raw = fs::read_to_string(&path).unwrap();
        for field in ["default_event_budget_ms", "budget_ms"] {
            let normalized = format!("\"{field}\":5000.0");
            assert_eq!(raw.matches(&normalized).count(), 1);
            raw = raw.replace(&normalized, &format!("\"{field}\":{encoded}"));
        }
        fs::write(&path, &raw).unwrap();
        let service = Service::start_saved(&f, &f.root.join("state"));
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        let mut validate = f.command();
        validate
            .args(["config", "validate", "--config"])
            .arg(f.root.join("aw.json"));
        assert!(f.run(validate).status.success());
        install(&f);
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        let mut launch = command(&f, "run");
        launch.args(["--", "--", "allow"]);
        let output = f.run(launch);
        assert!(
            output.status.success(),
            "{encoded}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(f.root.join("codex-tool-executed").exists());
        service.released(&f);
    }
}

#[test]
fn connect_installs_lifecycle_hooks_only_for_the_selected_target() {
    for owner in ["codex", "other"] {
        let f = Fixture::new();
        let mut doc = setup(&f);
        doc["spec"]["agents"]["other"] = doc["spec"]["agents"]["codex"].clone();
        doc["spec"]["events"]["session.start"] =
            json!({"enabled":true, "match":{"agents":[owner]}, "steps":[]});
        doc["spec"]["events"]["turn.stop"] = json!({"enabled":true, "steps":[{
            "id":"record","provider":"policy","operation":"record","effects":["observe"],
            "on_error":"report","match":{"agents":[owner]}
        }]});
        f.save(&doc);
        install(&f);
        let installed: Value =
            serde_json::from_slice(&fs::read(f.root.join("home/hooks.json")).unwrap()).unwrap();
        for event in ["SessionStart", "Stop"] {
            assert_eq!(
                installed["hooks"].get(event).is_some(),
                owner == "codex",
                "{owner} {event}"
            );
        }
    }
}

#[test]
fn install_requires_reconciliation_before_noop_or_changed_retry() {
    for complete in [false, true] {
        let f = Fixture::new();
        setup(&f);
        install(&f);
        let hooks = f.root.join("home/hooks.json");
        if !complete {
            fs::write(&hooks, b"{\"hooks\":{}}").unwrap();
        }
        let live = fs::read(&hooks).unwrap();
        let backup = f.root.join("home/hooks.json.aw-backup");
        fs::write(&backup, b"unreconciled-concurrent-edit").unwrap();
        for _ in 0..2 {
            let output = f.run(command(&f, "connect"));
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("recovery review"));
            assert_eq!(fs::read(&hooks).unwrap(), live);
            assert_eq!(fs::read(&backup).unwrap(), b"unreconciled-concurrent-edit");
        }
        fs::rename(backup, f.root.join("reviewed-hooks.json")).unwrap();
        install(&f);
    }
}

#[test]
fn app_server_legacy_capacity_blocks_and_recovers_on_relaunch() {
    use aw_service::{Client, Operation};
    let f = Fixture::new();
    let mut doc = setup(&f);
    let cli = doc["spec"]["agents"]["codex"]["argv"][0].clone();
    doc["spec"]["agents"]["codex"]["argv"] = json!([cli, "app-server"]);
    f.save(&doc);
    let service = Service::start(&f, &doc);
    install(&f);
    let mut launch = command(&f, "run");
    launch.env("FAKE_CODEX_APP_MODE", "capacity");
    let process = Process::spawn(&f, launch);
    let marker = f.root.join("codex-chain");
    let deadline = Instant::now() + TIMEOUT;
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "Codex did not publish its binding"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let chain: Value =
        serde_json::from_slice(&fs::read(fs::read_to_string(marker).unwrap()).unwrap()).unwrap();
    let binding: Value =
        serde_json::from_slice(&fs::read(chain["binding"].as_str().unwrap()).unwrap()).unwrap();
    let client = Client::connect(Path::new(binding["socket"].as_str().unwrap()), deadline).unwrap();
    let close = |index| Operation::CloseHookEvent {
        instance_id: binding["binding"]["instance_id"].as_str().unwrap().into(),
        event: if index % 2 == 0 {
            "tool.before"
        } else {
            "tool.after"
        }
        .into(),
        session_id: "capacity".into(),
        call_id: format!("call-{}", index / 2),
    };
    for index in 0..1024 {
        assert_eq!(
            client.call(close(index), Instant::now() + TIMEOUT).unwrap()["closed"],
            true
        );
    }
    let error = client
        .call(close(1024), Instant::now() + TIMEOUT)
        .unwrap_err();
    assert!(error.to_string().contains("native_event_limit"), "{error}");
    fs::write(f.root.join("capacity-ready"), b"ready").unwrap();
    let output = process.finish();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["results"][0]["code"], 2);
    assert!(!f.root.join("codex-tool-executed").exists());
    service.released(&f);
    let output = f.run(command(&f, "run"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(f.root.join("codex-tool-executed").exists());
    service.released(&f);
}

#[path = "codex/shadow.rs"]
mod shadow;
