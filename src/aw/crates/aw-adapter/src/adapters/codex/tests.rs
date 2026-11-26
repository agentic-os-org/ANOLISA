//! Native field validation needs only launch identity, never a service binding.

use super::*;

#[test]
fn inventory_probe_reaps_its_child_on_sigint_and_sigterm() {
    use std::{
        fs,
        io::Read,
        os::unix::{fs::PermissionsExt, process::CommandExt},
        path::Path,
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    struct Probe(Child);

    impl Drop for Probe {
        fn drop(&mut self) {
            if thread::panicking() {
                // The fixture owns a private process group for failure cleanup.
                unsafe { libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL) };
            }
            let _ = self.0.wait();
        }
    }

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("native/codex/probe.py");
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let root = tempfile::tempdir().unwrap();
        let fixture = root.path().join("fake-codex");
        fs::write(
            &fixture,
            r#"#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
import time

assert sys.argv[1:] == ["app-server"]
assert json.loads(sys.stdin.readline())["method"] == "initialize"
pid = os.getpid()
started = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19]
temporary = Path("probe-child.pid.tmp")
temporary.write_text(f"{pid} {started}")
temporary.replace("probe-child.pid")
time.sleep(30)
"#,
        )
        .unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let mut probe = Probe(
            Command::new("python3")
                .arg("-B")
                .arg(&script)
                .arg(&fixture)
                .arg("{}")
                .arg("app-server")
                .arg(root.path().join("result.json"))
                .current_dir(root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );

        let marker = root.path().join("probe-child.pid");
        let startup = Instant::now() + Duration::from_secs(5);
        while !marker.exists() {
            assert!(probe.0.try_wait().unwrap().is_none(), "probe exited early");
            assert!(Instant::now() < startup, "app-server never initialized");
            thread::sleep(Duration::from_millis(5));
        }
        let identity = fs::read_to_string(&marker).unwrap();
        let (child_pid, started) = identity.split_once(' ').unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            unsafe { libc::kill(probe.0.id() as libc::pid_t, signal) },
            0
        );
        let status = loop {
            assert!(Instant::now() < deadline, "probe ignored signal {signal}");
            if let Some(status) = probe.0.try_wait().unwrap() {
                break status;
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(!status.success(), "probe succeeded after signal {signal}");
        let mut stderr = String::new();
        probe
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(
            stderr.contains("ProbeCancelled"),
            "signal {signal}: {stderr}"
        );
        match fs::read_to_string(format!("/proc/{child_pid}/stat")) {
            Ok(stat) => {
                let (_, fields) = stat.rsplit_once(')').unwrap();
                assert_ne!(
                    fields.split_whitespace().nth(19).unwrap(),
                    started,
                    "app-server {child_pid} remains after signal {signal}"
                );
            }
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        }
    }
}

#[test]
fn normalizes_native_identity_and_preserves_failed_tool_results() {
    let context = NativeContext {
        target: "coding".into(),
        instance_id: "instance".into(),
        cwd: "/workspace".into(),
    };
    let payload = json!({
        "hook_event_name":"PostToolUse", "cwd":"/workspace",
        "session_id":"session", "tool_use_id":"call", "tool_name":"Bash",
        "tool_input":{"command":"false"},
        "tool_response":{"stdout":"", "stderr":"failure", "exit_code":1}
    });
    let result = Codex.normalize(&context, "tool.after", &payload).unwrap();
    assert_eq!(
        result["agent"],
        json!({"adapter":"codex", "binding_id":"coding", "instance_id":"instance"})
    );
    assert_eq!(result["session_id"], "session");
    assert_eq!(result["tool"]["call_id"], "call");
    assert_eq!(result["tool"]["result"], payload["tool_response"]);
    assert_eq!(result["native"], payload);

    for (key, value) in [
        ("hook_event_name", json!("PreToolUse")),
        ("cwd", json!("/other")),
        ("session_id", json!("")),
        ("tool_use_id", json!(" ")),
        ("tool_name", json!("x".repeat(513))),
        ("tool_input", json!("false")),
    ] {
        let mut invalid = payload.clone();
        invalid[key] = value;
        assert!(
            Codex.normalize(&context, "tool.after", &invalid).is_err(),
            "{key}"
        );
    }
    let mut missing = payload.clone();
    missing.as_object_mut().unwrap().remove("tool_response");
    assert!(Codex.normalize(&context, "tool.after", &missing).is_err());
    assert!(Codex.normalize(&context, "unsupported", &payload).is_err());
}

#[test]
fn submission_requires_bounded_turn_and_text() {
    let context = NativeContext {
        target: "coding".into(),
        instance_id: "instance".into(),
        cwd: "/workspace".into(),
    };
    let payload = json!({"hook_event_name":"UserPromptSubmit", "cwd":"/workspace",
        "session_id":"session", "turn_id":"turn", "prompt":"check this"});
    let normalized = Codex.normalize(&context, "input.submit", &payload).unwrap();
    assert_eq!(normalized["input"]["text"], "check this");
    assert_eq!(normalized["turn_id"], "turn");
    for (key, value) in [
        ("turn_id", json!(" ")),
        ("session_id", json!("x".repeat(513))),
        ("prompt", json!({"text":"check this"})),
        ("cwd", json!("/other")),
    ] {
        let mut invalid = payload.clone();
        invalid[key] = value;
        assert!(
            Codex.normalize(&context, "input.submit", &invalid).is_err(),
            "{key}"
        );
    }
}

#[test]
fn launch_entrypoints_cannot_dispatch_bare_prompts_as_hidden_commands() {
    for command in [
        "responses-api-proxy",
        "migrate-rollouts",
        "e",
        "a",
        "resume",
        "future-command",
    ] {
        for prefix in [vec![], vec!["exec"]] {
            let mut args = prefix.clone();
            args.push(command);
            assert!(
                preflight::arguments(&args.iter().map(Into::into).collect::<Vec<_>>()).is_err()
            );
            let mut literal = prefix;
            literal.extend(["--", command]);
            assert!(
                preflight::arguments(&literal.iter().map(Into::into).collect::<Vec<_>>()).is_ok()
            );
        }
    }
    for args in [
        vec![],
        vec!["--model", "responses-api-proxy"],
        vec!["exec", "--ephemeral", "--", "a prompt"],
    ] {
        assert!(preflight::arguments(&args.iter().map(Into::into).collect::<Vec<_>>()).is_ok());
    }
    assert!(preflight::arguments(&["--".into(), "one".into(), "two".into()]).is_err());
    let literal =
        preflight::arguments(&["--".into(), "--dangerously-bypass-hook-trust".into()]).unwrap();
    assert!(!literal.bypass_hook_trust);
}

#[test]
fn native_lifecycle_uses_codex_session_and_turn_identity() {
    let context = crate::NativeContext {
        cwd: "/work".into(),
        target: "codex".into(),
        instance_id: "instance".into(),
    };
    let mut native = json!({"hook_event_name":"SessionStart","session_id":"session","cwd":"/work"});
    assert_eq!(
        super::Codex
            .normalize(&context, "session.start", &native)
            .unwrap()["session_id"],
        "session"
    );
    native["hook_event_name"] = json!("Stop");
    native["turn_id"] = json!("turn");
    native["stop_hook_active"] = json!(false);
    let turn = super::Codex
        .normalize(&context, "turn.stop", &native)
        .unwrap();
    assert_eq!(turn["turn_id"], "turn");
    assert_eq!(turn["native"]["reason"], "stop_requested");
    native["stop_hook_active"] = json!(true);
    assert!(super::Codex
        .normalize(&context, "turn.stop", &native)
        .is_err());
}

#[test]
fn gateway_requires_the_exact_local_responses_route() {
    let base = [
        "app-server",
        "--listen",
        "stdio://",
        "-c",
        r#"model_provider="gateway""#,
        "-c",
        r#"model_providers.gateway.name="Local Gateway""#,
        "-c",
        r#"model_providers.gateway.env_key="LOCAL_CODEX_GATEWAY_KEY""#,
        "-c",
        "model_providers.gateway.supports_websockets=false",
        "-c",
        r#"model_providers.gateway.wire_api="responses""#,
        "-c",
        r#"model_providers.gateway.base_url="http://127.0.0.1:4242/v1""#,
    ];
    let admitted = |args: &[&str]| {
        preflight::arguments(&args.iter().map(Into::into).collect::<Vec<_>>()).is_ok()
    };
    assert!(admitted(&base));
    assert!(!admitted(&base[..base.len() - 2]));
    let mut alternate: Vec<_> = base
        .iter()
        .map(|arg| arg.replace("gateway", "local-v2_test").into())
        .collect();
    // The selector may follow its settings.
    alternate[3..].rotate_left(2);
    let selected = preflight::arguments(&alternate).unwrap();
    assert!(
        matches!(selected.entrypoint, preflight::Entrypoint::Gateway(id) if id == "local-v2_test")
    );
    let with = |index, setting: &str| {
        let mut args = base;
        args[index] = setting;
        admitted(&args)
    };
    for provider in [
        "",
        "Local",
        "2local",
        "local.key",
        "local/key",
        &"a".repeat(65),
    ] {
        let args: Vec<_> = base
            .iter()
            .map(|arg| arg.replace("gateway", provider).into())
            .collect();
        assert!(preflight::arguments(&args).is_err(), "{provider}");
    }
    assert!(!with(4, r#"model_provider="other""#));
    assert!(!with(6, r#"model_providers.other.name="Local Gateway""#));
    assert!(!with(6, base[8]));
    assert!(with(6, r#"model_providers.gateway.name="Local \\ 网关""#));
    for name in ["", "one\ntwo", "one'two", "one\"two", "\u{85}"] {
        assert!(!with(
            6,
            &format!("model_providers.gateway.name={}", json!(name))
        ));
    }
    assert!(with(
        8,
        &format!(r#"model_providers.gateway.env_key="{}""#, "A".repeat(64))
    ));
    for key in [
        "",
        "a",
        "_KEY",
        "A-B",
        "HOME",
        "PATH",
        "LD_PRELOAD",
        "CODEX_HOME",
        "AW_SOCKET",
        "PYTHONPATH",
        "NODE_OPTIONS",
        "HTTPS_PROXY",
        &"A".repeat(65),
    ] {
        assert!(
            !with(8, &format!(r#"model_providers.gateway.env_key="{key}""#)),
            "{key}"
        );
    }

    for assignment in [
        "hooks.state.example.enabled=true",
        "features.code_mode=true",
        "features.hooks=false",
        r#"model_providers.gateway.base_url="http://127.0.0.1:4242/v1?proxy=1""#,
        r#"model_providers.gateway.base_url="http://localhost:4242/v1""#,
        r#"model_providers.gateway.base_url="http://127.0.0.1:0/v1""#,
        r#"model_providers.gateway.base_url="http://127.0.0.1:4242@remote/v1""#,
        r#"model_providers.gateway.env_key="HOME""#,
    ] {
        let mut args = base;
        args[base.len() - 1] = assignment;
        assert!(!admitted(&args), "{assignment}");
    }
    let mut duplicate = base.to_vec();
    duplicate.extend_from_slice(&base[3..5]);
    assert!(!admitted(&duplicate));
}
