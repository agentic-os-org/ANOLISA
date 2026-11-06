#![cfg(target_os = "linux")]

use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    path::Path,
    time::{Duration, Instant},
};

#[path = "support/fixture.rs"]
mod support;

fn invocation() -> Value {
    json!({"api_version":VERSION,"method":"invoke","request_id":"skill-1",
        "operation":"check_skill","config_revision":"b".repeat(64),"budget_ms":1000,
        "allowed_effects":["observe","block"],"config":{"version":1,"mode":"block","skill":true},
        "event":{"name":"tool.before","agent":{"adapter":"qwenpaw","binding_id":"target","instance_id":"instance"},
            "session_id":"session","tool":{"name":"Skill","native_name":"Skill","call_id":"call",
                "input":{"skill":"test-skill"},"result":null},
            "native":{"tool_call":{"id":"native-call"},"aw_skill":{"name":"test-skill","directory":"/actual/skill path","source":"toolkit"}}}})
}

fn run(value: Value, cli: &Path) -> Value {
    let protocol = Protocol::new().unwrap();
    let request = protocol.bind_invocation(value).unwrap();
    let mut output = Vec::new();
    aw_provider_sec_core::run(
        &mut Cursor::new(serde_json::to_vec(request.as_value()).unwrap()),
        &mut output,
        cli,
        Path::new("/unused.sock"),
        Duration::from_secs(2),
    )
    .unwrap();
    match protocol.check_response(&request, &output) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => panic!("{error}"),
    }
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn skill_states_preserve_trust_and_explicit_modes() {
    for (status, reason) in [
        ("pass", "skill_pass"),
        ("none", "skill_unsigned"),
        ("drifted", "skill_drifted"),
        ("warn", "skill_warn"),
        ("deny", "skill_deny"),
        ("tampered", "skill_tampered"),
    ] {
        for mode in ["block", "observe"] {
            let fixture =
                support::Directory::new(json!({"stdout":{"status":status,"findings":"PRIVATE"}}));
            let mut request = invocation();
            request["config"]["mode"] = json!(mode);
            let result = run(request, &fixture.cli());
            let effect = if mode == "block" && status != "pass" {
                "block"
            } else {
                "observe"
            };
            assert_eq!(
                result["effects"],
                json!([{"type":effect,"reason_code":reason}])
            );
            assert!(!result.to_string().contains("PRIVATE"));
            let argv = fixture.argv();
            assert_eq!(argv[0], "--trace-context");
            let context: Value = serde_json::from_str(argv[1].as_str().unwrap()).unwrap();
            assert_eq!(context["tool_call_id"], "native-call");
            assert_eq!(argv[2], "--socket");
            assert_eq!(argv[3], "/unused.sock");
            assert_eq!(argv[4], "--timeout-ms");
            assert_eq!(argv[6], "skill-ledger");
            assert_eq!(argv[7], "check");
            assert_eq!(argv[8], "/actual/skill path");
            assert_eq!(argv.as_array().unwrap().len(), 9);
            fixture.assert_stopped();
        }
    }
}

#[test]
fn native_skill_alias_keeps_resolution_and_unknown_tool_checks() {
    let fixture = support::Directory::new(json!({"stdout":{"status":"pass"}}));
    let mut request = invocation();
    request["event"]["agent"]["adapter"] = json!("hermes");
    request["event"]["tool"]["name"] = json!("skill_view");
    request["event"]["tool"]["native_name"] = json!("skill_view");
    assert_eq!(
        run(request.clone(), &fixture.cli())["effects"][0]["reason_code"],
        "skill_pass"
    );
    request["event"]["native"]["aw_skill"]["name"] = json!("different");
    assert_eq!(
        run(request.clone(), &fixture.cli())["error_code"],
        "skill_resolution_error"
    );
    request["event"]["agent"]["adapter"] = json!("qwenpaw");
    assert_eq!(
        run(request.clone(), &fixture.cli())["error_code"],
        "skill_tool_unmapped"
    );
    request["event"]["tool"]["native_name"] = json!("unmapped_tool");
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "skill_tool_unmapped"
    );
    fixture.assert_stopped();
}

#[test]
fn skill_checks_coexist_with_submission_configuration() {
    let fixture = support::Directory::new(json!({"stdout":{"status":"pass"}}));
    let mut request = invocation();
    request["config"]["prompt"] = json!({"mode":"block","scan_mode":"fast"});
    request["config"]["pii"] = json!({"input_mode":"block","output_mode":"observe"});
    assert_eq!(
        run(request, &fixture.cli())["effects"],
        json!([{"type":"observe","reason_code":"skill_pass"}])
    );
    fixture.assert_stopped();
}

#[test]
fn negative_trust_exit_one_is_a_completed_check() {
    for status in ["none", "drifted", "warn", "deny", "tampered"] {
        let fixture = support::Directory::new(json!({"exit":1,"stdout":{"status":status}}));
        assert_eq!(
            run(invocation(), &fixture.cli())["effects"][0]["type"],
            "block"
        );
    }
}

#[test]
fn skill_faults_are_errors_without_success_effects() {
    for (case, code) in [
        (json!({"raw":""}), "invalid_skill_result"),
        (json!({"raw":"not JSON"}), "invalid_skill_result"),
        (
            json!({"raw":"{\"status\":\"pass\",\"status\":\"none\"}"}),
            "invalid_skill_result",
        ),
        (
            json!({"stdout":{"status":"unknown"}}),
            "invalid_skill_result",
        ),
        (json!({"stdout":{"status":"error"}}), "skill_check_error"),
        (
            json!({"exit":1,"stdout":{"status":"pass"}}),
            "invalid_skill_result",
        ),
        (json!({"exit":2,"stdout":{"status":"none"}}), "cli_failed"),
        (
            json!({"exit":1,"stderr":"PermissionDenied PRIVATE"}),
            "invalid_skill_result",
        ),
        (json!({"stdout_repeat":1048577}), "cli_output_limit"),
    ] {
        let fixture = support::Directory::new(case);
        let reply = run(invocation(), &fixture.cli());
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["error_code"], code);
        assert!(reply.get("effects").is_none());
        assert!(!reply.to_string().contains("PRIVATE"));
    }
    let fixture = support::Directory::new(json!({"sleep":2}));
    let mut request = invocation();
    request["budget_ms"] = json!(100);
    let started = Instant::now();
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "deadline_exceeded"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    fixture.assert_stopped();
    assert_eq!(
        run(invocation(), Path::new("/absent-cli"))["error_code"],
        "cli_transport_error"
    );
}

#[test]
fn missing_or_untrustworthy_resolution_does_not_call_backend() {
    let fixture = support::Directory::new(json!({"stdout":{"status":"pass"}}));
    for resolved in [
        json!(null),
        json!({"error_code":"skill_resolution_error"}),
        json!({"name":"test-skill","source":"toolkit","directory":"relative"}),
        json!({"name":"test-skill","source":"toolkit","directory":"/wrong/../skill"}),
        json!({"name":"test-skill","source":"toolkit","directory":"/"}),
        json!({"name":"wrong-skill","source":"toolkit","directory":"/skill"}),
    ] {
        let mut request = invocation();
        request["event"]["native"]["aw_skill"] = resolved;
        assert_eq!(
            run(request, &fixture.cli())["error_code"],
            "skill_resolution_error"
        );
    }
    let mut request = invocation();
    request["allowed_effects"] = json!(["observe"]);
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "block_not_admitted"
    );
    let mut request = invocation();
    request["config"]["skill"] = json!(false);
    request["config"]["tools"] = json!({"bash":{"language":"bash","input_pointer":"/command"}});
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "skill_check_disabled"
    );
    assert!(!fixture.0.join("cli.pid").exists());
}

#[test]
fn pi_submission_checks_selected_skills_and_skips_only_explicit_plain_text() {
    let mut request = invocation();
    request["event"]["name"] = json!("input.submit");
    request["event"]["turn_id"] = json!("turn");
    request["event"]["agent"]["adapter"] = json!("pi");
    request["event"].as_object_mut().unwrap().remove("tool");
    request["event"]["input"] = json!({"text":"expanded Skill instructions"});
    request["event"]["native"]["aw_skill_request"] = json!("test-skill");
    request["event"]["native"]["aw_skill"]["digest"] = json!(format!("sha256:{}", "a".repeat(64)));
    for (status, effect) in [("pass", "observe"), ("none", "block")] {
        let fixture = support::Directory::new(json!({}));
        let hash = write_manifest(&fixture, &format!("sha256:{}", "a".repeat(64)));
        std::fs::write(
            fixture.0.join("case.json"),
            json!({"stdout":{"status":status,"manifestHash":hash}}).to_string(),
        )
        .unwrap();
        request["event"]["native"]["aw_skill"]["directory"] = json!(fixture.0);
        assert_eq!(
            run(request.clone(), &fixture.cli())["effects"][0]["type"],
            effect
        );
        assert_eq!(fixture.argv()[8], json!(fixture.0));
        fixture.assert_stopped();
    }
    for checked in [Value::Null, json!("sha256:invalid")] {
        let fixture =
            support::Directory::new(json!({"stdout":{"status":"pass","manifestHash":checked}}));
        assert_eq!(
            run(request.clone(), &fixture.cli())["error_code"],
            "invalid_skill_result"
        );
        fixture.assert_stopped();
    }
    for digest in [
        Value::Null,
        json!("invalid"),
        json!(format!("sha256:{}", "A".repeat(64))),
    ] {
        let fixture = support::Directory::new(json!({"stdout":{"status":"pass"}}));
        let mut invalid = request.clone();
        invalid["event"]["native"]["aw_skill"]["digest"] = digest;
        assert_eq!(
            run(invalid, &fixture.cli())["error_code"],
            "skill_resolution_error"
        );
        assert!(!fixture.0.join("cli.pid").exists());
    }
    let fixture = support::Directory::new(json!({"stdout":{"status":"pass"}}));
    request["event"]["native"] = json!({"aw_skill":null,"aw_skill_request":null});
    assert_eq!(
        run(request.clone(), &fixture.cli())["effects"],
        json!([{"type":"observe","reason_code":"skill_not_requested"}])
    );
    for native in [
        json!({}),
        json!({"aw_skill":null}),
        json!({"aw_skill":null,"aw_skill_request":"test-skill"}),
        json!({"aw_skill":{"error_code":"skill_resolution_error"},"aw_skill_request":"test-skill"}),
    ] {
        request["event"]["native"] = native;
        assert_eq!(
            run(request.clone(), &fixture.cli())["error_code"],
            "skill_resolution_error"
        );
    }
    request["event"]["native"] = json!({"aw_skill":null,"aw_skill_request":null});
    request["event"]["agent"]["adapter"] = json!("claude-sdk");
    assert_eq!(
        run(request, &fixture.cli())["error_code"],
        "skill_source_unmapped"
    );
    assert!(!fixture.0.join("cli.pid").exists());
}

fn write_manifest(fixture: &support::Directory, digest: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut manifest = json!({
        "version": 2, "canonicalSkillDir": fixture.0, "scanStatus": "pass",
        "fileHashes": {"SKILL.md": digest},
        "scans": [{"details": {"z": "中文", "a": [1.0, true]}}]
    });
    manifest.sort_all_objects();
    let hash = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&manifest).unwrap())
    );
    manifest["manifestHash"] = json!(hash);
    manifest["signature"] = json!({"value":"authenticated-by-cli"});
    std::fs::create_dir_all(fixture.0.join(".skill-meta")).unwrap();
    std::fs::write(
        fixture.0.join(".skill-meta/latest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    hash
}

#[test]
fn pi_snapshot_requires_the_exact_manifest_authenticated_by_the_existing_cli() {
    use std::{fs, os::unix::fs::symlink};
    for scenario in [
        "pass",
        "snapshot-mismatch",
        "forged-label",
        "missing",
        "symlink",
        "fifo",
        "oversized",
    ] {
        let fixture = support::Directory::new(json!({}));
        let digest = format!("sha256:{}", "a".repeat(64));
        let hash = write_manifest(&fixture, &digest);
        let path = fixture.0.join(".skill-meta/latest.json");
        let mut request = invocation();
        request["event"].as_object_mut().unwrap().remove("tool");
        request["event"]["name"] = json!("input.submit");
        request["event"]["turn_id"] = json!("turn");
        request["event"]["input"] = json!({"text":"expanded"});
        request["event"]["agent"]["adapter"] = json!("pi");
        request["event"]["native"]["aw_skill_request"] = json!("test-skill");
        request["event"]["native"]["aw_skill"]["directory"] = json!(fixture.0);
        request["event"]["native"]["aw_skill"]["digest"] = json!(digest);
        match scenario {
            "snapshot-mismatch" => {
                request["event"]["native"]["aw_skill"]["digest"] =
                    json!(format!("sha256:{}", "b".repeat(64)))
            }
            "forged-label" => {
                let mut forged: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                forged["fileHashes"]["SKILL.md"] = json!(format!("sha256:{}", "b".repeat(64)));
                request["event"]["native"]["aw_skill"]["digest"] =
                    forged["fileHashes"]["SKILL.md"].clone();
                fs::write(&path, forged.to_string()).unwrap();
            }
            "missing" => fs::remove_file(&path).unwrap(),
            "symlink" => {
                fs::rename(&path, fixture.0.join("manifest.json")).unwrap();
                symlink(fixture.0.join("manifest.json"), &path).unwrap();
            }
            "fifo" => {
                fs::remove_file(&path).unwrap();
                assert!(std::process::Command::new("mkfifo")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success());
            }
            "oversized" => fs::write(&path, vec![b' '; 8 * 1024 * 1024 + 1]).unwrap(),
            _ => {}
        }
        fs::write(
            fixture.0.join("case.json"),
            json!({"stdout":{"status":"pass","manifestHash":hash}}).to_string(),
        )
        .unwrap();
        let result = run(request, &fixture.cli());
        assert_eq!(
            result["effects"][0]["reason_code"],
            if scenario == "pass" {
                "skill_pass"
            } else {
                "skill_drifted"
            },
            "{scenario}: {result}"
        );
        assert_eq!(
            result["effects"][0]["type"],
            if scenario == "pass" {
                "observe"
            } else {
                "block"
            }
        );
    }
}
