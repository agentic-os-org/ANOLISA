//! Canonical validation shares presentation while legacy output remains unchanged.

use super::{output::invoke, *};

fn validate(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    command
        .args(["config", "validate", "--config"])
        .arg(&fixture.config);
    command
}

#[test]
fn canonical_validation_reports_success_and_scope_without_runtime_work() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    let bytes = fs::read(&f.config).unwrap();
    let inode = fs::metadata(&f.config).unwrap().ino();
    let current = invoke(&validate(&f));
    let legacy = invoke(f.command().args(["validate", "--config"]).arg(&f.config));
    assert!(current.status.success() && legacy.status.success());
    assert_eq!(current.stdout, legacy.stdout);
    assert_eq!(current.stdout, b"configuration valid\n");
    for against in [false, true] {
        let mut command = validate(&f);
        command.arg("--json");
        if against {
            command.arg("--against").arg(&f.config);
        }
        let output = invoke(&command);
        assert!(output.status.success() && output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["command"], "config validate");
        assert_eq!(report["status"], "succeeded");
        assert_eq!(
            report["result"],
            json!({"kind":"configuration_checked", "policy_update_scope":against})
        );
        assert!(report.get("mutation").is_none());
    }
    assert_eq!(fs::read(&f.config).unwrap(), bytes);
    assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
    assert!(!f.state.exists());
}

#[test]
fn validation_reports_target_projection_without_private_values_or_runtime_work() {
    let f = Fixture::new();
    let mut config = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!(
            "../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap()
        .as_value()
        .clone();
    config["spec"]["agents"] = json!({
        "sdk":{"adapter":"qoder-sdk", "argv":["/agent-private-argument"]},
        "app":{"adapter":"qwenpaw", "argv":["/agent-private-argument"]}
    });
    config["spec"]["providers"]["policy"] = json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio", "location":"agent", "argv":["/provider-private-argument"]},
        "timeout_ms":1000, "max_output_bytes":4096, "config":{"token":"private-api-key"}
    });
    config["spec"]["events"]["tool.after"] = json!({
        "enabled":true, "match":{"tools":["bash"]}, "steps":[
            {"id":"app-result", "provider":"policy", "operation":"check",
             "effects":["replace_result"], "on_error":"withhold_result",
             "match":{"tools":["native:qwenpaw:execute_shell_command"]}},
            {"id":"sdk-observe", "provider":"policy", "operation":"check",
             "effects":["observe"], "on_error":"report",
             "match":{"tools":["native:qoder-sdk:Bash"]}},
            {"id":"sdk-unknown-alias", "provider":"policy", "operation":"check",
             "effects":["observe"], "on_error":"report",
             "match":{"tools":["native:qoder-sdk:UnknownAlias"]}},
            {"id":"disabled", "enabled":false, "provider":"policy", "operation":"check",
             "effects":["observe"], "on_error":"report",
             "match":{"tools":["native:qwenpaw:execute_shell_command"]}}
        ]
    });
    let bytes = serde_json::to_vec(&config).unwrap();
    fs::write(&f.config, &bytes).unwrap();
    let inode = fs::metadata(&f.config).unwrap().ino();
    let details = concat!(
        "target app: projected out tool.after/sdk-observe (tool selectors do not overlap)\n",
        "target app: projected out tool.after/sdk-unknown-alias (tool selectors do not overlap)\n",
        "target sdk: projected out tool.after/app-result (tool selectors do not overlap)\n",
    );
    for against in [false, true] {
        let mut canonical = validate(&f);
        let mut legacy = f.command();
        legacy.args(["validate", "--config"]).arg(&f.config);
        if against {
            canonical.arg("--against").arg(&f.config);
            legacy.arg("--against").arg(&f.config);
        }
        let summary = if against {
            "policy update scope valid; runtime activation is not checked\n"
        } else {
            "configuration valid\n"
        };
        for command in [&canonical, &legacy] {
            let output = invoke(command);
            assert!(output.status.success() && output.stderr.is_empty());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("{summary}{details}")
            );
        }
        let output = invoke(canonical.arg("--json"));
        assert!(output.status.success() && output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"], "succeeded");
        assert_eq!(
            report["result"],
            json!({
                "kind":"configuration_checked", "policy_update_scope":against,
                "projected_out_steps":[
                    {"target":"app", "event":"tool.after", "step_id":"sdk-observe"},
                    {"target":"app", "event":"tool.after", "step_id":"sdk-unknown-alias"},
                    {"target":"sdk", "event":"tool.after", "step_id":"app-result"}
                ]
            })
        );
    }
    let output = invoke(validate(&f).arg("--quiet"));
    assert!(output.status.success() && output.stdout.is_empty() && output.stderr.is_empty());
    assert_eq!(fs::read(&f.config).unwrap(), bytes);
    assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
    assert!(!f.state.exists());
}

#[test]
fn validation_failures_mask_private_fields_and_keep_the_source() {
    let f = Fixture::new();
    fs::write(&f.config, "private-api-key: private-api-key").unwrap();
    let before = fs::read(&f.config).unwrap();
    let output = invoke(validate(&f).arg("--json"));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], "invalid_configuration");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-api-key"));
    assert_eq!(fs::read(&f.config).unwrap(), before);
    let output = invoke(
        f.command()
            .args(["config", "validate", "--config", "/missing", "--json"]),
    );
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["error"]["code"], "io");
    assert!(report.get("result").is_none());
}

#[test]
fn validation_enforces_the_binding_step_limit_after_target_projection() {
    let f = Fixture::new();
    let mut config = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!(
            "../../../aw-config/examples/aw.minimal.yaml"
        ))
        .unwrap()
        .as_value()
        .clone();
    config["spec"]["agents"] = json!({
        "sdk":{"adapter":"qoder-sdk", "argv":["/must-not-execute-agent"]},
        "app":{"adapter":"qwenpaw", "argv":["/must-not-execute-agent"]}
    });
    config["spec"]["providers"]["policy"] = json!({
        "transport":{"type":"stdio", "location":"agent", "argv":["/must-not-execute-provider"]},
        "timeout_ms":1000, "max_output_bytes":4096
    });
    let step = |index: usize| {
        json!({
            "id":format!("step-{index}"), "provider":"policy",
            "effects":["observe"], "on_error":"report"
        })
    };
    for (before, after, disabled, excluded, valid) in [
        (64, 0, 0, 0, true),
        (65, 0, 0, 0, false),
        (66, 0, 0, 0, false),
        (32, 32, 0, 0, true),
        (32, 33, 0, 0, false),
        (64, 0, 2, 0, true),
        (64, 0, 0, 2, true),
    ] {
        let mut before_steps: Vec<Value> = (0..before).map(step).collect();
        for index in 0..disabled {
            let mut item = step(before + index);
            item["enabled"] = json!(false);
            before_steps.push(item);
        }
        for index in 0..excluded {
            let mut item = step(before + disabled + index);
            item["match"] = json!({"tools":["file_read"]});
            before_steps.push(item);
        }
        config["spec"]["events"] = json!({
            "tool.before":{"enabled":true, "match":{"tools":["bash"]}, "steps":before_steps},
            "tool.after":{"enabled":true, "steps":(0..after).map(step).collect::<Vec<_>>()},
            "input.submit":{"enabled":false, "steps":(0..65).map(step).collect::<Vec<_>>()}
        });
        let bytes = serde_json::to_vec(&config).unwrap();
        fs::write(&f.config, &bytes).unwrap();
        for legacy in [false, true] {
            let mut command = if legacy {
                let mut command = f.command();
                command.args(["validate", "--config"]).arg(&f.config);
                command
            } else {
                validate(&f)
            };
            for against in [false, true] {
                if against {
                    command.arg("--against").arg(&f.config);
                }
                let output = invoke(&command);
                assert_eq!(
                    output.status.success(),
                    valid,
                    "{before}/{after}/{disabled}/{excluded}"
                );
                if !valid {
                    assert!(output.stdout.is_empty());
                    assert!(String::from_utf8_lossy(&output.stderr)
                        .contains("maximum_steps_per_binding: 64"));
                }
            }
        }
        let output = invoke(validate(&f).arg("--json"));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(output.stderr.is_empty());
        if valid {
            assert_eq!(report["status"], "succeeded");
        } else {
            assert_eq!(report["status"], "failed");
            assert_eq!(report["error"]["code"], "invalid_configuration");
            assert_eq!(
                report["error"]["message"],
                "Configured target exceeds the service step limit"
            );
        }
        assert_eq!(fs::read(&f.config).unwrap(), bytes);
        assert!(!f.state.exists());
    }

    // A total above 64 remains valid when each target selects at most 64 steps.
    let mut steps: Vec<Value> = (0..64).map(step).collect();
    for item in &mut steps {
        item["match"] = json!({"tools":["native:qoder-sdk:Bash"]});
    }
    let mut app = step(64);
    app["match"] = json!({"tools":["native:qwenpaw:execute_shell_command"]});
    steps.push(app);
    config["spec"]["events"] = json!({"tool.before":{"enabled":true, "steps":steps}});
    fs::write(&f.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(invoke(&validate(&f)).status.success());
    config["spec"]["events"]["tool.before"]["steps"][64]["match"] =
        json!({"tools":["native:qoder-sdk:Bash"]});
    fs::write(&f.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert_eq!(invoke(&validate(&f)).status.code(), Some(1));
}

#[test]
fn validation_human_options_and_usage_preserve_local_output_boundaries() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    let output = invoke(validate(&f).arg("--quiet"));
    assert!(output.status.success());
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let output = invoke(validate(&f).arg("--verbose"));
    assert!(output.status.success());
    assert_eq!(output.stdout, b"configuration valid\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no runtime activation"));
    for arguments in [
        vec!["config", "validate", "--json"],
        vec![
            "config", "validate", "--config", "/missing", "--json", "--quiet",
        ],
        vec!["validate", "--config", "/missing", "--json"],
    ] {
        let output = invoke(f.command().args(arguments));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty() && !output.stderr.is_empty());
    }
}

#[test]
fn validation_rejects_nonregular_and_oversized_documents_without_blocking() {
    let f = Fixture::new();
    let fifo = std::ffi::CString::new(f.config.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the C string is valid and this creates only the owned fixture FIFO.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let output = invoke(validate(&f).arg("--json"));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error"]["code"],
        "io"
    );
    fs::remove_file(&f.config).unwrap();
    fs::write(&f.config, vec![b' '; aw_config::MAX_DOCUMENT_BYTES + 1]).unwrap();
    let output = invoke(validate(&f).arg("--json"));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error"]["code"],
        "invalid_configuration"
    );
}
